//! "Not taken" (docs/wf-orchestration-v1.md §5.4.6, built as §5.4.10 piece 4): nothing a person
//! writes ends in silence.
//!
//! - When a WF takes a message nowhere, its engine answers the sender with `NOT_TAKEN`: a system
//!   message with the message's `trace_id` and thread id, and the payload
//!   `{"ok": false, "error": "not_taken", "reason": "...", "detail": "..."}`.
//! - io.slack remembers each message and click it relays, for 15 minutes and 2,000 at most
//!   (`RelayedMessages`): its `trace_id`, its channel, its Slack thread and its Slack user, from its
//!   IO context, and the node it was relayed to, its `routing.dst` (the node's `io.dst_node` then).
//!   It remembers one before sending it, so an answer that comes back at once finds it.
//! - A `NOT_TAKEN` with a remembered `trace_id`, from the node the message was relayed to when that
//!   node is a WF (by the name its router stamps, `routing.src_l2_name`), posts "Not taken:
//!   <reason>", which only that user sees, where they wrote. The text is fixed by the reason: the
//!   answer's own words, its `detail` included, are never posted. A `NOT_TAKEN` is shown once.
//! - A `NOT_TAKEN` from any other sender is dropped with a log line, and takes nothing: a node that
//!   holds a relayed `trace_id` cannot show "Not taken" to a person, nor keep the WF's own answer
//!   from showing.
//! - Any other system message stays ignored, as before.

use crate::SlackClients;
use fluxbee_sdk::protocol::{Destination, Message as WireMessage};
use io_common::io_context::extract_io_context;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The engine's answer to a message it took nowhere.
pub(crate) const MSG_NOT_TAKEN: &str = "NOT_TAKEN";
/// How long a relayed message is remembered.
const REMEMBERED_FOR_MS: u64 = 15 * 60 * 1000;
/// How many relayed messages are remembered at most; past it, the oldest go first.
const REMEMBERED_MAX: usize = 2_000;

/// Where a relayed message was written, and who wrote it: where "not taken" is shown, and to whom.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WrittenAt {
    channel_id: String,
    /// The Slack thread, `None` at the channel's top level.
    thread_ts: Option<String>,
    user_id: String,
}

impl WrittenAt {
    /// The place and the writer a relayed message's IO context names: its conversation, its
    /// `reply_target`'s Slack thread, and its sender.
    fn of_message(msg: &WireMessage) -> Option<Self> {
        let io = extract_io_context(msg.meta.context.as_ref()?)?;
        if io.channel != "slack" {
            return None;
        }
        let channel_id = Some(io.conversation.id.trim())
            .filter(|id| !id.is_empty())?
            .to_string();
        let user_id = Some(io.sender.id.trim())
            .filter(|id| !id.is_empty())?
            .to_string();
        let thread_ts = io
            .reply_target
            .params
            .get("thread_ts")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|ts| !ts.is_empty())
            .map(str::to_string);
        Some(Self {
            channel_id,
            thread_ts,
            user_id,
        })
    }
}

/// What the memory keeps of a relayed message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Relayed {
    /// Until when it is remembered (Unix milliseconds).
    until: u64,
    written_at: WrittenAt,
    /// The node it was relayed to, its `routing.dst`: the node's `io.dst_node` when it went, or
    /// `None` when the router resolved its destination.
    relayed_to: Option<String>,
}

/// Why a `NOT_TAKEN` is not shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotShown {
    /// Its message is not remembered here: older than 15 minutes, shown already, or never relayed
    /// by this node.
    NotRemembered,
    /// Its sender is not the WF node its message was relayed to.
    NotFromItsWf,
}

/// The messages and clicks the node relayed lately, by `trace_id` (the module's doc).
#[derive(Default)]
pub(crate) struct RelayedMessages {
    remembered: Mutex<Remembered>,
}

#[derive(Default)]
struct Remembered {
    /// Each message, by its `trace_id`.
    by_trace: HashMap<String, Relayed>,
    /// The `trace_id`s in the order they came, with their time: the oldest go first.
    order: VecDeque<(u64, String)>,
}

impl Remembered {
    /// Forgets the message `order` gives back, unless it was taken already.
    fn forget(&mut self, (until, trace_id): (u64, String)) {
        if self
            .by_trace
            .get(&trace_id)
            .is_some_and(|relayed| relayed.until == until)
        {
            self.by_trace.remove(&trace_id);
        }
    }

    fn forget_expired(&mut self, now_ms: u64) {
        while self
            .order
            .front()
            .is_some_and(|(until, _)| *until <= now_ms)
        {
            if let Some(oldest) = self.order.pop_front() {
                self.forget(oldest);
            }
        }
    }
}

impl RelayedMessages {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A panic elsewhere while the lock was held leaves the memory usable.
    fn lock(&self) -> MutexGuard<'_, Remembered> {
        self.remembered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Remembers a message the node relays, by its `trace_id`, with the place and the writer its
    /// IO context names, and the node it goes to. One with no `trace_id`, or no Slack IO context,
    /// is not remembered.
    pub(crate) fn remember(&self, msg: &WireMessage, now_ms: u64) {
        let trace_id = msg.routing.trace_id.trim();
        let Some(written_at) = WrittenAt::of_message(msg).filter(|_| !trace_id.is_empty()) else {
            return;
        };
        let relayed_to = match &msg.routing.dst {
            Destination::Unicast(node) => Some(node.trim().to_string()).filter(|n| !n.is_empty()),
            _ => None,
        };
        let until = now_ms.saturating_add(REMEMBERED_FOR_MS);
        let mut remembered = self.lock();
        remembered.forget_expired(now_ms);
        remembered.by_trace.insert(
            trace_id.to_string(),
            Relayed {
                until,
                written_at,
                relayed_to,
            },
        );
        remembered.order.push_back((until, trace_id.to_string()));
        while remembered.order.len() > REMEMBERED_MAX {
            if let Some(oldest) = remembered.order.pop_front() {
                remembered.forget(oldest);
            }
        }
    }

    /// Takes the message of `trace_id` for a `NOT_TAKEN` from `from`, the name its router stamps:
    /// when the message is remembered, and was relayed to `from`, a WF node. An answer is shown
    /// once. One from any other sender takes nothing, so the answer of the WF the message went to
    /// still finds it.
    fn take(&self, trace_id: &str, from: Option<&str>, now_ms: u64) -> Result<WrittenAt, NotShown> {
        let mut remembered = self.lock();
        remembered.forget_expired(now_ms);
        let trace_id = trace_id.trim();
        let relayed = remembered
            .by_trace
            .get(trace_id)
            .ok_or(NotShown::NotRemembered)?;
        if !crate::is_wf_node(from) || relayed.relayed_to.as_deref() != from {
            return Err(NotShown::NotFromItsWf);
        }
        remembered
            .by_trace
            .remove(trace_id)
            .map(|relayed| relayed.written_at)
            .ok_or(NotShown::NotRemembered)
    }
}

/// What a reason says to whoever wrote: the reasons the engine gives, or `None`.
fn reason_text(reason: &str) -> Option<&'static str> {
    match reason {
        "no_conversation" => Some("no conversation here takes this message"),
        "not_expected" => Some("the conversation does not expect a message from you now"),
        "invalid_input" => Some("the message is not valid here"),
        "no_thread" => Some("the message has no conversation"),
        _ => None,
    }
}

/// The text a `NOT_TAKEN`'s payload shows: "Not taken: <reason>", or "Not taken." for a reason
/// this node does not know.
fn not_taken_text(payload: &Value) -> String {
    let reason = payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match reason_text(reason) {
        Some(text) => format!("Not taken: {text}"),
        None => "Not taken.".to_string(),
    }
}

/// Shows the engine's `NOT_TAKEN` to whoever wrote the message it names, where they wrote: a
/// message only they see. One for a message not remembered here, older than 15 minutes or never
/// relayed by this node, or from a sender that is not the WF node the message was relayed to, is
/// logged and dropped.
pub(crate) async fn show(slack: &SlackClients, relayed: &RelayedMessages, msg: &WireMessage) {
    let trace_id = msg.routing.trace_id.as_str();
    let from = msg.routing.src_l2_name.as_deref();
    let reason = msg
        .payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if reason_text(reason).is_none() {
        tracing::warn!(
            trace_id,
            reason,
            "NOT_TAKEN with a reason this node does not know"
        );
    }
    let written_at = match relayed.take(trace_id, from, crate::now_epoch_ms()) {
        Ok(written_at) => written_at,
        Err(NotShown::NotRemembered) => {
            tracing::debug!(
                trace_id,
                reason,
                "NOT_TAKEN for a message not remembered here; dropped"
            );
            return;
        }
        Err(NotShown::NotFromItsWf) => {
            tracing::warn!(
                trace_id,
                reason,
                from = from.unwrap_or_default(),
                "NOT_TAKEN from a sender that is not the WF node the message was relayed to; \
                 dropped"
            );
            return;
        }
    };
    tracing::info!(
        trace_id,
        reason,
        channel_id = %written_at.channel_id,
        thread_ts = written_at.thread_ts.as_deref().unwrap_or(""),
        user = %written_at.user_id,
        "a relayed message was not taken; telling whoever wrote it"
    );
    if let Err(err) = slack
        .post_ephemeral(
            &written_at.channel_id,
            &written_at.user_id,
            &not_taken_text(&msg.payload),
            written_at.thread_ts.as_deref(),
        )
        .await
    {
        tracing::warn!(
            trace_id,
            channel_id = %written_at.channel_id,
            error = %err,
            "telling a person that their message was not taken failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_web_api::FakeWebApi;
    use fluxbee_sdk::protocol::{Meta, Routing};
    use io_common::io_context::{slack_inbound_io_context, wrap_in_meta_context};
    use io_common::router_message::{build_user_message, DEFAULT_TTL};
    use serde_json::json;

    const NOW: u64 = 1_793_000_000_000;
    const MINUTE_MS: u64 = 60 * 1000;
    /// The node's `io.dst_node`: the WF its messages are relayed to.
    const WF: &str = "WF.sales.invoice@motherbee";

    /// A message the node relays to `WF`, as io-common builds it: `user` wrote it in the Slack
    /// thread `thread_ts`, or at the channel's top level.
    fn relayed_message(trace_id: &str, user: &str, thread_ts: Option<&str>) -> WireMessage {
        relayed_message_to(Some(WF), trace_id, user, thread_ts)
    }

    /// A message the node relays to `dst_node`, or to the router's resolution with none.
    fn relayed_message_to(
        dst_node: Option<&str>,
        trace_id: &str,
        user: &str,
        thread_ts: Option<&str>,
    ) -> WireMessage {
        let io = slack_inbound_io_context("T0WORKSPACE", user, "C0SALES", thread_ts, "Ev0001");
        build_user_message(
            "io-slack-uuid",
            dst_node.map(str::to_string),
            DEFAULT_TTL,
            trace_id.to_string(),
            Some("ilk:person".to_string()),
            None,
            wrap_in_meta_context(&io),
            json!({"type": "text", "content": "hola"}),
        )
    }

    /// The engine's `NOT_TAKEN` for `trace_id`, from `from` as its router stamps it.
    fn not_taken(trace_id: &str, from: Option<&str>) -> WireMessage {
        WireMessage {
            routing: Routing {
                src: "wf-uuid".to_string(),
                src_l2_name: from.map(str::to_string),
                dst: Destination::Unicast("io-slack-uuid".to_string()),
                ttl: 16,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some(MSG_NOT_TAKEN.to_string()),
                thread_id: Some("thread:sha256:case".to_string()),
                ..Meta::default()
            },
            payload: json!({
                "ok": false,
                "error": "not_taken",
                "reason": "not_expected",
                "detail": "the instance took nothing"
            }),
        }
    }

    fn written_at(user: &str, thread_ts: Option<&str>) -> WrittenAt {
        WrittenAt {
            channel_id: "C0SALES".to_string(),
            thread_ts: thread_ts.map(str::to_string),
            user_id: user.to_string(),
        }
    }

    /// A relayed message is remembered by its `trace_id`, with where it was written and who wrote
    /// it, from its IO context (a Slack thread, or the top level), and the node it went to. An
    /// answer takes it once.
    #[test]
    fn a_relayed_message_is_remembered_with_where_it_was_written() {
        let relayed = RelayedMessages::new();
        relayed.remember(
            &relayed_message("trace-thread", "U0JUAN", Some("1793000000.000100")),
            NOW,
        );
        relayed.remember(&relayed_message("trace-top", "U0ANA", None), NOW);
        assert_eq!(
            relayed.lock().by_trace["trace-top"].relayed_to.as_deref(),
            Some(WF)
        );

        assert_eq!(
            relayed.take("trace-thread", Some(WF), NOW + 1),
            Ok(written_at("U0JUAN", Some("1793000000.000100")))
        );
        assert_eq!(
            relayed.take("trace-thread", Some(WF), NOW + 2),
            Err(NotShown::NotRemembered)
        );
        assert_eq!(
            relayed.take("trace-top", Some(WF), NOW + 1),
            Ok(written_at("U0ANA", None))
        );
        assert_eq!(
            relayed.take("trace-unknown", Some(WF), NOW + 1),
            Err(NotShown::NotRemembered)
        );
    }

    /// Only the WF a message was relayed to takes it, by the name its router stamps. Another WF,
    /// an IO node or a sender with no name takes nothing, and the message stays for its WF; a
    /// message relayed to a node that is no WF, or with no `io.dst_node`, is taken by no one.
    #[test]
    fn only_the_wf_a_message_was_relayed_to_takes_it() {
        let relayed = RelayedMessages::new();
        relayed.remember(&relayed_message("trace-1", "U0JUAN", None), NOW);
        for from in [
            Some("WF.other@motherbee"),
            Some("IO.api.sales@worker1"),
            Some("WF.sales.invoice@worker1"),
            None,
        ] {
            assert_eq!(
                relayed.take("trace-1", from, NOW),
                Err(NotShown::NotFromItsWf),
                "{from:?}"
            );
        }
        assert_eq!(
            relayed.take("trace-1", Some(WF), NOW),
            Ok(written_at("U0JUAN", None))
        );

        relayed.remember(
            &relayed_message_to(Some("AI.chat@motherbee"), "trace-ai", "U0JUAN", None),
            NOW,
        );
        assert_eq!(
            relayed.take("trace-ai", Some("AI.chat@motherbee"), NOW),
            Err(NotShown::NotFromItsWf)
        );
        relayed.remember(
            &relayed_message_to(None, "trace-resolved", "U0JUAN", None),
            NOW,
        );
        assert_eq!(relayed.lock().by_trace["trace-resolved"].relayed_to, None);
        assert_eq!(
            relayed.take("trace-resolved", Some(WF), NOW),
            Err(NotShown::NotFromItsWf)
        );
    }

    /// A message with no Slack IO context, or no `trace_id`, has nowhere to show an answer: it is
    /// not remembered.
    #[test]
    fn a_message_with_nowhere_to_answer_is_not_remembered() {
        let relayed = RelayedMessages::new();
        let mut no_context = relayed_message("trace-1", "U0JUAN", None);
        no_context.meta.context = None;
        relayed.remember(&no_context, NOW);
        let mut another_channel = relayed_message("trace-2", "U0JUAN", None);
        if let Some(io) = another_channel
            .meta
            .context
            .as_mut()
            .and_then(|context| context.get_mut("io"))
        {
            io["channel"] = json!("whatsapp");
        }
        relayed.remember(&another_channel, NOW);
        relayed.remember(&relayed_message("  ", "U0JUAN", None), NOW);
        assert_eq!(
            relayed.take("trace-1", Some(WF), NOW),
            Err(NotShown::NotRemembered)
        );
        assert_eq!(
            relayed.take("trace-2", Some(WF), NOW),
            Err(NotShown::NotRemembered)
        );
        assert_eq!(relayed.lock().by_trace.len(), 0);
    }

    /// A relayed message is remembered for 15 minutes, and 2,000 at most: past either, the oldest
    /// go first.
    #[test]
    fn messages_are_remembered_15_minutes_and_2000_at_most() {
        let relayed = RelayedMessages::new();
        relayed.remember(&relayed_message("trace-old", "U0JUAN", None), NOW);
        relayed.remember(
            &relayed_message("trace-recent", "U0JUAN", None),
            NOW + 10 * MINUTE_MS,
        );
        assert_eq!(
            relayed.take("trace-old", Some(WF), NOW + 15 * MINUTE_MS),
            Err(NotShown::NotRemembered)
        );
        assert!(relayed
            .take("trace-recent", Some(WF), NOW + 15 * MINUTE_MS)
            .is_ok());

        for index in 0..=REMEMBERED_MAX {
            relayed.remember(
                &relayed_message(&format!("trace-{index}"), "U0JUAN", None),
                NOW,
            );
        }
        assert_eq!(relayed.lock().by_trace.len(), REMEMBERED_MAX);
        assert_eq!(relayed.lock().order.len(), REMEMBERED_MAX);
        assert_eq!(
            relayed.take("trace-0", Some(WF), NOW),
            Err(NotShown::NotRemembered)
        );
        assert!(relayed.take("trace-1", Some(WF), NOW).is_ok());
        assert!(relayed
            .take(&format!("trace-{REMEMBERED_MAX}"), Some(WF), NOW)
            .is_ok());
    }

    /// Each reason has its text, and one this node does not know shows "Not taken." alone: the
    /// answer's own words, its `detail` included, are never shown.
    #[test]
    fn each_reason_shows_its_text_and_never_the_answers_words() {
        let text = |reason: &str| {
            not_taken_text(&json!({
                "ok": false,
                "error": "not_taken",
                "reason": reason,
                "detail": "<!channel> the engine's detail"
            }))
        };
        assert_eq!(
            text("no_conversation"),
            "Not taken: no conversation here takes this message"
        );
        assert_eq!(
            text("not_expected"),
            "Not taken: the conversation does not expect a message from you now"
        );
        assert_eq!(
            text("invalid_input"),
            "Not taken: the message is not valid here"
        );
        assert_eq!(
            text("no_thread"),
            "Not taken: the message has no conversation"
        );
        assert_eq!(text("<!here> anything"), "Not taken.");
        assert_eq!(not_taken_text(&json!({})), "Not taken.");
    }

    /// The NOT_TAKEN of the WF a message was relayed to is shown once, to whoever wrote it, where
    /// they wrote: a message only they see. A second one, or one for a message never relayed here,
    /// shows nothing.
    #[tokio::test]
    async fn a_not_taken_answer_is_shown_once_to_whoever_wrote() {
        let api = FakeWebApi::start(&[]).await;
        let slack = api.client().await;
        let relayed = RelayedMessages::new();
        relayed.remember(
            &relayed_message("trace-1", "U0JUAN", Some("1793000000.000100")),
            crate::now_epoch_ms(),
        );
        show(&slack, &relayed, &not_taken("trace-1", Some(WF))).await;
        show(&slack, &relayed, &not_taken("trace-1", Some(WF))).await;
        show(&slack, &relayed, &not_taken("trace-never", Some(WF))).await;
        assert_eq!(
            api.calls(),
            vec![(
                "chat.postEphemeral".to_string(),
                json!({
                    "channel": "C0SALES",
                    "user": "U0JUAN",
                    "text": "Not taken: the conversation does not expect a message from you now",
                    "thread_ts": "1793000000.000100",
                })
            )]
        );
    }

    /// A NOT_TAKEN from any sender but the WF the message was relayed to shows nothing, and takes
    /// nothing: the WF's own still shows.
    #[tokio::test]
    async fn a_not_taken_from_another_sender_shows_nothing() {
        let api = FakeWebApi::start(&[]).await;
        let slack = api.client().await;
        let relayed = RelayedMessages::new();
        relayed.remember(
            &relayed_message("trace-1", "U0JUAN", None),
            crate::now_epoch_ms(),
        );
        for from in [
            Some("IO.api.sales@worker1"),
            Some("WF.other@motherbee"),
            None,
        ] {
            show(&slack, &relayed, &not_taken("trace-1", from)).await;
        }
        assert_eq!(api.calls(), Vec::new());
        show(&slack, &relayed, &not_taken("trace-1", Some(WF))).await;
        assert_eq!(api.bodies("chat.postEphemeral").len(), 1);
    }
}
