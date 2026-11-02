//! "Not taken" (docs/wf-orchestration-v1.md §5.4.6, built as §5.4.10 piece 4): nothing a person
//! writes ends in silence.
//!
//! - When a WF takes a message nowhere, its engine answers the sender with `NOT_TAKEN`: a system
//!   message with the message's `trace_id` and thread id, and the payload
//!   `{"ok": false, "error": "not_taken", "reason": "...", "detail": "..."}`.
//! - io.slack remembers each message and click it relays, for 15 minutes and 2,000 at most
//!   (`RelayedMessages`): its `trace_id`, its channel, its Slack thread and its Slack user, from its
//!   IO context. It remembers one before sending it, so an answer that comes back at once finds it.
//! - A `NOT_TAKEN` with a remembered `trace_id` posts "Not taken: <reason>", which only that user
//!   sees, where they wrote. The text is fixed by the reason: the answer's own words, its `detail`
//!   included, are never posted. A `NOT_TAKEN` is shown once.
//! - Any other system message stays ignored, as before.

use crate::SlackClients;
use fluxbee_sdk::protocol::Message as WireMessage;
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

/// The messages and clicks the node relayed lately, by `trace_id` (the module's doc).
#[derive(Default)]
pub(crate) struct RelayedMessages {
    remembered: Mutex<Remembered>,
}

#[derive(Default)]
struct Remembered {
    /// Each message's place and writer, and until when it is remembered (Unix milliseconds).
    by_trace: HashMap<String, (u64, WrittenAt)>,
    /// The `trace_id`s in the order they came, with their time: the oldest go first.
    order: VecDeque<(u64, String)>,
}

impl Remembered {
    /// Forgets the message `order` gives back, unless it was taken already.
    fn forget(&mut self, (until, trace_id): (u64, String)) {
        if self
            .by_trace
            .get(&trace_id)
            .is_some_and(|(remembered_until, _)| *remembered_until == until)
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
    /// IO context names. One with no `trace_id`, or no Slack IO context, is not remembered.
    pub(crate) fn remember(&self, msg: &WireMessage, now_ms: u64) {
        let trace_id = msg.routing.trace_id.trim();
        let Some(written_at) = WrittenAt::of_message(msg).filter(|_| !trace_id.is_empty()) else {
            return;
        };
        let until = now_ms.saturating_add(REMEMBERED_FOR_MS);
        let mut remembered = self.lock();
        remembered.forget_expired(now_ms);
        remembered
            .by_trace
            .insert(trace_id.to_string(), (until, written_at));
        remembered.order.push_back((until, trace_id.to_string()));
        while remembered.order.len() > REMEMBERED_MAX {
            if let Some(oldest) = remembered.order.pop_front() {
                remembered.forget(oldest);
            }
        }
    }

    /// Takes the message of `trace_id`, when it is remembered: an answer is shown once.
    fn take(&self, trace_id: &str, now_ms: u64) -> Option<WrittenAt> {
        let mut remembered = self.lock();
        remembered.forget_expired(now_ms);
        remembered
            .by_trace
            .remove(trace_id.trim())
            .map(|(_, written_at)| written_at)
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
/// relayed by this node, is logged and dropped.
pub(crate) async fn show(slack: &SlackClients, relayed: &RelayedMessages, msg: &WireMessage) {
    let trace_id = msg.routing.trace_id.as_str();
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
    let Some(written_at) = relayed.take(trace_id, crate::now_epoch_ms()) else {
        tracing::debug!(
            trace_id,
            reason,
            "NOT_TAKEN for a message not remembered here; dropped"
        );
        return;
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
    use io_common::io_context::{slack_inbound_io_context, wrap_in_meta_context};
    use io_common::router_message::{build_user_message, DEFAULT_TTL};
    use serde_json::json;

    const NOW: u64 = 1_793_000_000_000;
    const MINUTE_MS: u64 = 60 * 1000;

    /// A message the node relays, as io-common builds it: `user` wrote it in the Slack thread
    /// `thread_ts`, or at the channel's top level.
    fn relayed_message(trace_id: &str, user: &str, thread_ts: Option<&str>) -> WireMessage {
        let io = slack_inbound_io_context("T0WORKSPACE", user, "C0SALES", thread_ts, "Ev0001");
        build_user_message(
            "io-slack-uuid",
            None,
            DEFAULT_TTL,
            trace_id.to_string(),
            Some("ilk:person".to_string()),
            None,
            wrap_in_meta_context(&io),
            json!({"type": "text", "content": "hola"}),
        )
    }

    fn written_at(user: &str, thread_ts: Option<&str>) -> WrittenAt {
        WrittenAt {
            channel_id: "C0SALES".to_string(),
            thread_ts: thread_ts.map(str::to_string),
            user_id: user.to_string(),
        }
    }

    /// A relayed message is remembered by its `trace_id`, with where it was written and who wrote
    /// it, from its IO context: a Slack thread, or the top level. An answer takes it once.
    #[test]
    fn a_relayed_message_is_remembered_with_where_it_was_written() {
        let relayed = RelayedMessages::new();
        relayed.remember(
            &relayed_message("trace-thread", "U0JUAN", Some("1793000000.000100")),
            NOW,
        );
        relayed.remember(&relayed_message("trace-top", "U0ANA", None), NOW);

        assert_eq!(
            relayed.take("trace-thread", NOW + 1),
            Some(written_at("U0JUAN", Some("1793000000.000100")))
        );
        assert_eq!(relayed.take("trace-thread", NOW + 2), None);
        assert_eq!(
            relayed.take("trace-top", NOW + 1),
            Some(written_at("U0ANA", None))
        );
        assert_eq!(relayed.take("trace-unknown", NOW + 1), None);
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
        assert_eq!(relayed.take("trace-1", NOW), None);
        assert_eq!(relayed.take("trace-2", NOW), None);
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
        assert_eq!(relayed.take("trace-old", NOW + 15 * MINUTE_MS), None);
        assert!(relayed.take("trace-recent", NOW + 15 * MINUTE_MS).is_some());

        for index in 0..=REMEMBERED_MAX {
            relayed.remember(
                &relayed_message(&format!("trace-{index}"), "U0JUAN", None),
                NOW,
            );
        }
        assert_eq!(relayed.lock().by_trace.len(), REMEMBERED_MAX);
        assert_eq!(relayed.lock().order.len(), REMEMBERED_MAX);
        assert_eq!(relayed.take("trace-0", NOW), None);
        assert!(relayed.take("trace-1", NOW).is_some());
        assert!(relayed
            .take(&format!("trace-{REMEMBERED_MAX}"), NOW)
            .is_some());
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

    /// The engine's NOT_TAKEN for a remembered message is shown once, whether or not Slack takes
    /// the post (this test has no token for it); one for a message not remembered here is dropped.
    #[tokio::test]
    async fn a_not_taken_answer_is_shown_once_to_whoever_wrote() {
        let slack = SlackClients::new().expect("client");
        let relayed = RelayedMessages::new();
        relayed.remember(
            &relayed_message("trace-1", "U0JUAN", Some("1793000000.000100")),
            crate::now_epoch_ms(),
        );
        let answer = WireMessage {
            routing: fluxbee_sdk::protocol::Routing {
                src: "wf-uuid".to_string(),
                src_l2_name: Some("WF.sales.invoice@motherbee".to_string()),
                dst: fluxbee_sdk::protocol::Destination::Unicast("io-slack-uuid".to_string()),
                ttl: 16,
                trace_id: "trace-1".to_string(),
            },
            meta: fluxbee_sdk::protocol::Meta {
                msg_type: "system".to_string(),
                msg: Some(MSG_NOT_TAKEN.to_string()),
                thread_id: Some("thread:sha256:case".to_string()),
                ..fluxbee_sdk::protocol::Meta::default()
            },
            payload: json!({
                "ok": false,
                "error": "not_taken",
                "reason": "not_expected",
                "detail": "the instance took nothing"
            }),
        };
        show(&slack, &relayed, &answer).await;
        assert_eq!(relayed.take("trace-1", crate::now_epoch_ms()), None);
        // A second answer, or one for a message never relayed here, finds nothing.
        show(&slack, &relayed, &answer).await;
    }
}
