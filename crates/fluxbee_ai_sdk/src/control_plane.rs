//! The control plane of an AI node (ai.generic, SY.frontdesk.gov): which `system` requests it runs,
//! and from whom, and which messages it never answers.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use fluxbee_sdk::protocol::{
    is_system_kind, MSG_ANNOUNCE, MSG_CONFIG_GET, MSG_CONFIG_SET, MSG_HELLO, MSG_NODE_HELP,
    MSG_NODE_STATUS_GET, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_VAULT_SECRET_CHANGED, MSG_WITHDRAW,
    PRIMARY_HIVE_ID,
};
use serde_json::Value;
use tokio::time::Instant;

use crate::message::{Message, AI_ERROR_MSG_SUFFIX};
use crate::refusal_log::RefusalLog;

/// A liveness probe: the node's name and state.
pub const MSG_PING: &str = "PING";
/// The node's state, behavior and config versions.
pub const MSG_STATUS: &str = "STATUS";

/// A node's answer to a message this one sent, which it took nowhere: a `system` message on that
/// message's trace and thread, `{"ok": false, "error": "not_taken", "reason", "detail",
/// "retryable"}`. Nothing of the message ran there. The WF engine sends it since 0.1.94, and the Go
/// SDK since 0.1.141 for a message its queue has no room for (`node_busy`, retryable, FINDINGS
/// A-329): a busy WF's answer to an agent's answer. It is a reply, never answered (FINDINGS A-339).
const MSG_NOT_TAKEN: &str = "NOT_TAKEN";

/// How much of a `NOT_TAKEN`'s `detail` its journal line shows: the node that sent it wrote it.
const NOT_TAKEN_DETAIL_MAX_CHARS: usize = 240;

/// What an AI node's control plane does with a message it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRequest {
    /// One of the node's control verbs, as the router's authority gate sees it (a `system`
    /// message with the verb in its own case) and from the node that delivers it: the node runs
    /// it.
    Run(&'static str),
    /// One of the node's control verbs, which the node refuses: it is not a `system` message, the
    /// verb is in another case, or its sender does not deliver it. `verb` is the verb in its own
    /// case, `reason` says why.
    Refuse { verb: &'static str, reason: String },
    /// Anything else: no `msg`, or one that is no control verb of the node
    /// (`VAULT_SECRET_CHANGED`, a router notice).
    Other,
}

/// How the AI node `node_name` takes `msg`, a message of its control plane, when its control verbs
/// are `verbs`. A verb runs only as the router's authority gate sees it: in a `system` message
/// (`is_system_kind`, case-insensitive like the router) with the verb in its own case, which the
/// router delivers only from an origin `policy/system.rego` admits. The same verb as an `admin`
/// message or in another case passes no gate, so it is refused, and so is a verb from a sender that
/// does not deliver it (`control_caller_authorized`).
pub fn control_request(msg: &Message, node_name: &str, verbs: &[&'static str]) -> ControlRequest {
    let Some(name) = msg.meta.msg.as_deref() else {
        return ControlRequest::Other;
    };
    let Some(verb) = verbs
        .iter()
        .copied()
        .find(|verb| verb.eq_ignore_ascii_case(name))
    else {
        return ControlRequest::Other;
    };
    let refuse = |reason: String| ControlRequest::Refuse { verb, reason };
    if !is_system_kind(&msg.meta.msg_type) {
        return refuse(format!(
            "{verb} comes as a system message, not msg_type '{}'",
            msg.meta.msg_type
        ));
    }
    if name != verb {
        return refuse(format!("the verb is {verb}, not '{name}'"));
    }
    let sender = msg.routing.src_l2_name.as_deref();
    if !control_caller_authorized(verb, node_name, sender) {
        return refuse(format!(
            "{} does not send {verb} to {node_name}",
            sender.unwrap_or("a sender without a name")
        ));
    }
    ControlRequest::Run(verb)
}

/// Whether `sender`, the name the router stamps on a message (`routing.src_l2_name`), is the node
/// that delivers `verb` to the AI node `node_name` today (read in SY.admin and SY.orchestrator for
/// 0.1.86):
///
/// - **`CONFIG_GET` and `CONFIG_SET`:** SY.admin@motherbee, which sends them straight to the node,
///   on any hive (`node_control_config_get` and `node_control_config_set`). The router admits no
///   other origin for them (`policy/system.rego` rule 2), and no orchestrator sends them.
/// - **`NODE_STATUS_GET` and `NODE_HELP`:** the orchestrator of the node's own hive. SY.admin asks
///   the motherbee's orchestrator (`get_node_status`, `node_help`), which asks a node of its own
///   hive and hands a node of another hive to that hive's orchestrator (`get_node_status_flow`,
///   `node_help_flow`). SY.admin asks no AI node for either directly.
/// - **`PING` and `STATUS`,** which nothing in the code sends: either of them, as the operator's
///   `send_node_message` comes from SY.admin@motherbee.
pub fn control_caller_authorized(verb: &str, node_name: &str, sender: Option<&str>) -> bool {
    let Some((role, hive)) = sender.and_then(|sender| sender.trim().split_once('@')) else {
        return false;
    };
    let admin = role == "SY.admin" && hive == PRIMARY_HIVE_ID;
    let own_orchestrator = role == "SY.orchestrator"
        && !hive.is_empty()
        && node_name
            .split_once('@')
            .is_some_and(|(_, node_hive)| node_hive.trim() == hive);
    match verb {
        MSG_CONFIG_GET | MSG_CONFIG_SET => admin,
        MSG_NODE_STATUS_GET | MSG_NODE_HELP => own_orchestrator,
        MSG_PING | MSG_STATUS => admin || own_orchestrator,
        _ => false,
    }
}

/// Whether `msg`, a message of an AI node's control plane, is a notice or a reply rather than a
/// request, so it gets no answer: the router made it (it carries no `src_l2_name`, which the router
/// stamps on every message a node sends), as UNREACHABLE and TTL_EXCEEDED, or its name is a
/// notice's or a reply's: HELLO, ANNOUNCE, WITHDRAW, any `*_RESPONSE`, a `NOT_TAKEN` or an error
/// answer (`<msg>.error`). wf-generic (`isNotice`, `isReply`) leaves the same ones unanswered, and
/// so do SY.timer (`isInfrastructureMessage`) and the Go SDK (`IsNoticeOrReply`) from 0.1.141
/// (FINDINGS A-218, A-329). An answer to the router's own notice would go to the router's UUID,
/// which no node has: the router would answer UNREACHABLE again, and the two would bounce it
/// forever (FINDINGS A-112). Before 0.1.142 a `NOT_TAKEN` or a `<msg>.error` from a node was
/// answered `unknown_system_msg` (FINDINGS A-339). The node checks it before anything else
/// (`note_infrastructure_message` logs it).
pub fn is_infrastructure_message(msg: &Message) -> bool {
    let from_a_node = msg
        .routing
        .src_l2_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty());
    if !from_a_node {
        return true;
    }
    match msg.meta.msg.as_deref().unwrap_or_default() {
        "" | MSG_HELLO | MSG_ANNOUNCE | MSG_WITHDRAW | MSG_UNREACHABLE | MSG_TTL_EXCEEDED
        | MSG_NOT_TAKEN => true,
        name => name.ends_with("_RESPONSE") || name.ends_with(AI_ERROR_MSG_SUFFIX),
    }
}

/// Whether `msg` is SY.vault's `VAULT_SECRET_CHANGED`, a `system` message so named: the broadcast
/// of a secret's change, sent to every node at each change and, at SY.vault's start, once for each
/// secret it holds (FINDINGS A-325). It is an event, not a request, and SY.vault takes no answer
/// to it: its loop skips whatever is not a vault request. An AI node never answers it:
/// SY.frontdesk.gov reads its AI key again on it, and ai.generic, which asks SY.vault for its key
/// at each model call, leaves it alone. ai.generic used to answer each one with an
/// `unknown_system_msg` error, which SY.vault dropped.
pub fn is_vault_secret_changed(msg: &Message) -> bool {
    is_system_kind(&msg.meta.msg_type) && msg.meta.msg.as_deref() == Some(MSG_VAULT_SECRET_CHANGED)
}

/// The journal's line for `msg`, a notice or a reply that reached the control plane of the AI node
/// `node_name` (`is_infrastructure_message`), which gets no answer. Two say that a message of the
/// node's went nowhere, and are warnings with their reason: the router's notice that it was not
/// delivered, and a node's `NOT_TAKEN` (`note_not_taken`). The first of each name and reason at
/// once, then at most one every 10 s, which names how many came since its last (`notices`); the
/// ones between are debug lines. An agent answers at the rate messages come (an `ai_runtime_busy`
/// answer to each turn past its queue, FINDINGS A-257), and as many answers may come back
/// undelivered, or not taken by a busy WF.
pub fn note_infrastructure_message(node_name: &str, msg: &Message) {
    let name = msg.meta.msg.as_deref().unwrap_or_default();
    if name == MSG_NOT_TAKEN {
        note_not_taken(node_name, msg);
        return;
    }
    if matches!(name, MSG_UNREACHABLE | MSG_TTL_EXCEEDED) {
        let field = |key: &str| {
            msg.payload
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let reason = field("reason");
        let due = notice_line_due(
            &mut NOTICE_LOGS.lock().unwrap_or_else(PoisonError::into_inner),
            name,
            &reason,
            Instant::now(),
        );
        let Some(notices) = due else {
            tracing::debug!(
                node_name = %node_name,
                trace_id = %msg.routing.trace_id,
                notice = %name,
                original_dst = %field("original_dst"),
                reason = %reason,
                last_hop = %field("last_hop"),
                "router notice: a message this node sent was not delivered; not answered (counted \
                 in the next warning of its reason)"
            );
            return;
        };
        tracing::warn!(
            node_name = %node_name,
            trace_id = %msg.routing.trace_id,
            notice = %name,
            original_dst = %field("original_dst"),
            reason = %reason,
            last_hop = %field("last_hop"),
            notices,
            "router notice: a message this node sent was not delivered; not answered"
        );
        return;
    }
    tracing::info!(
        node_name = %node_name,
        trace_id = %msg.routing.trace_id,
        msg_type = %msg.meta.msg_type,
        msg = %name,
        src_l2_name = ?msg.routing.src_l2_name,
        "a notice or a reply, not a request; not answered"
    );
}

/// The warning for a node's `NOT_TAKEN` (FINDINGS A-339): `refused_by` took nowhere a message this
/// node sent on the trace, most often its answer (a WF whose queue is full, `node_busy`, for any
/// answer; one that cannot use its store, `unavailable`, for an answer with a name, while an
/// agent's usual answer has none and gets nothing back), and nothing of it ran there. It names
/// that node, the reason, whether the same message sent again may be taken (`retryable`; a
/// `NOT_TAKEN` without it, from a node before 0.1.141, is not) and the detail, as a router notice
/// is named (`notice_line_due`): a busy WF refuses each answer that comes while it is full. The
/// node does not send the message again: the runtime keeps no message it sent.
fn note_not_taken(node_name: &str, msg: &Message) {
    let reason = msg
        .payload
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let retryable = msg
        .payload
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let detail = msg
        .payload
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let detail = match detail.char_indices().nth(NOT_TAKEN_DETAIL_MAX_CHARS) {
        Some((cut, _)) => format!("{}...", &detail[..cut]),
        None => detail.to_string(),
    };
    let refused_by = msg.routing.src_l2_name.as_deref().unwrap_or_default();
    let due = notice_line_due(
        &mut NOTICE_LOGS.lock().unwrap_or_else(PoisonError::into_inner),
        MSG_NOT_TAKEN,
        reason,
        Instant::now(),
    );
    let Some(notices) = due else {
        tracing::debug!(
            node_name = %node_name,
            trace_id = %msg.routing.trace_id,
            thread_id = ?msg.meta.thread_id,
            refused_by = %refused_by,
            reason = %reason,
            retryable,
            detail = ?detail,
            "NOT_TAKEN: a message this node sent was not taken, and nothing of it ran there; not \
             answered, and not sent again (counted in the next warning of its reason)"
        );
        return;
    };
    tracing::warn!(
        node_name = %node_name,
        trace_id = %msg.routing.trace_id,
        thread_id = ?msg.meta.thread_id,
        refused_by = %refused_by,
        reason = %reason,
        retryable,
        detail = ?detail,
        notices,
        "NOT_TAKEN: a message this node sent was not taken, and nothing of it ran there; not \
         answered, and not sent again"
    );
}

/// How many kinds of notice, a name and a reason, keep a count of their own. The router has a few
/// reasons, and the nodes that send `NOT_TAKEN` a few more; a notice of any other past these counts
/// with its name's other reasons (`OTHER_REASONS`), so the counts stay bounded whatever comes.
const NOTICE_LOG_KINDS: usize = 32;
const OTHER_REASONS: &str = "*";

/// The counts of the router's notices and of the `NOT_TAKEN`s, one `RefusalLog` for each name and
/// reason (`notice_line_due`).
static NOTICE_LOGS: Mutex<BTreeMap<(String, String), RefusalLog>> = Mutex::new(BTreeMap::new());

/// When the warning for a notice named `notice` (a router notice, or a `NOT_TAKEN`) with `reason`
/// is due, how many notices of that name and reason to name in it, this one and those since its
/// last.
fn notice_line_due(
    logs: &mut BTreeMap<(String, String), RefusalLog>,
    notice: &str,
    reason: &str,
    now: Instant,
) -> Option<u64> {
    let mut key = (notice.to_string(), reason.to_string());
    if logs.len() >= NOTICE_LOG_KINDS && !logs.contains_key(&key) {
        key.1 = OTHER_REASONS.to_string();
    }
    logs.entry(key).or_default().note(now)
}

#[cfg(test)]
mod tests {
    use fluxbee_sdk::protocol::{Destination, Meta, Routing};
    use serde_json::json;

    use super::*;

    const VERBS: [&str; 6] = [
        MSG_CONFIG_GET,
        MSG_CONFIG_SET,
        MSG_NODE_STATUS_GET,
        MSG_NODE_HELP,
        MSG_PING,
        MSG_STATUS,
    ];

    fn request(msg_type: &str, msg: &str, sender: Option<&str>) -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-the-sender".to_string(),
                src_l2_name: sender.map(str::to_string),
                dst: Destination::Unicast("AI.sales@worker1".to_string()),
                ttl: 16,
                trace_id: "trace-1".to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    fn take(msg_type: &str, msg: &str, sender: Option<&str>) -> ControlRequest {
        control_request(&request(msg_type, msg, sender), "AI.sales@worker1", &VERBS)
    }

    #[test]
    fn each_verb_runs_from_the_node_that_delivers_it() {
        for (verb, sender) in [
            (MSG_CONFIG_SET, "SY.admin@motherbee"),
            (MSG_CONFIG_GET, "SY.admin@motherbee"),
            (MSG_NODE_STATUS_GET, "SY.orchestrator@worker1"),
            (MSG_NODE_HELP, "SY.orchestrator@worker1"),
            (MSG_PING, "SY.admin@motherbee"),
            (MSG_STATUS, "SY.orchestrator@worker1"),
        ] {
            assert_eq!(
                take("system", verb, Some(sender)),
                ControlRequest::Run(verb),
                "{verb} from {sender}"
            );
        }
        // The router's kind check ignores case, and so does the node's.
        assert_eq!(
            take("SYSTEM", MSG_CONFIG_SET, Some("SY.admin@motherbee")),
            ControlRequest::Run(MSG_CONFIG_SET)
        );
    }

    #[test]
    fn a_verb_past_the_routers_gate_or_from_another_sender_is_refused() {
        let refused = |msg_type: &str, msg: &str, sender: Option<&str>| {
            matches!(take(msg_type, msg, sender), ControlRequest::Refuse { .. })
        };
        // Another node, or a sender without a name.
        assert!(refused("system", MSG_CONFIG_SET, Some("AI.other@worker1")));
        assert!(refused(
            "system",
            MSG_CONFIG_SET,
            Some("IO.slack.main@motherbee")
        ));
        assert!(refused("system", MSG_CONFIG_SET, None));
        // An admin message, and a verb in another case: the router's gate never sees them.
        assert!(refused("admin", MSG_CONFIG_SET, Some("SY.admin@motherbee")));
        assert!(refused("system", "config_set", Some("SY.admin@motherbee")));
        assert!(refused(
            "system",
            "Node_Help",
            Some("SY.orchestrator@worker1")
        ));
        // The orchestrator of another hive, and an orchestrator asking for the config.
        assert!(refused(
            "system",
            MSG_NODE_STATUS_GET,
            Some("SY.orchestrator@motherbee")
        ));
        assert!(refused(
            "system",
            MSG_NODE_HELP,
            Some("SY.orchestrator@worker2")
        ));
        assert!(refused(
            "system",
            MSG_CONFIG_SET,
            Some("SY.orchestrator@worker1")
        ));
        // SY.admin asks for a node's status and help through the orchestrators, and it runs only
        // on the motherbee.
        assert!(refused(
            "system",
            MSG_NODE_STATUS_GET,
            Some("SY.admin@motherbee")
        ));
        assert!(refused("system", MSG_CONFIG_GET, Some("SY.admin@worker1")));

        assert_eq!(
            take("admin", "config_set", Some("SY.admin@motherbee")),
            ControlRequest::Refuse {
                verb: MSG_CONFIG_SET,
                reason: "CONFIG_SET comes as a system message, not msg_type 'admin'".to_string(),
            }
        );
    }

    #[test]
    fn what_is_no_control_verb_is_left_to_the_node() {
        for msg in ["VAULT_SECRET_CHANGED", "UNREACHABLE", "CONFIG_CHANGED"] {
            assert_eq!(take("system", msg, None), ControlRequest::Other, "{msg}");
        }
        let mut no_msg = request("system", MSG_CONFIG_SET, Some("SY.admin@motherbee"));
        no_msg.meta.msg = None;
        assert_eq!(
            control_request(&no_msg, "AI.sales@worker1", &VERBS),
            ControlRequest::Other
        );
        // A verb the node does not answer is not one of its control verbs.
        assert_eq!(
            control_request(
                &request("system", MSG_NODE_HELP, Some("SY.orchestrator@worker1")),
                "SY.frontdesk.gov@worker1",
                &[MSG_CONFIG_GET, MSG_CONFIG_SET]
            ),
            ControlRequest::Other
        );
    }

    /// FINDINGS A-112: what the router made (no sender's name) and what is named as a notice or a
    /// reply gets no answer; a request from a node, verb or not, is the control plane's to answer.
    #[test]
    fn a_notice_or_a_reply_is_infrastructure_and_a_nodes_request_is_not() {
        let notice = fluxbee_sdk::protocol::build_unreachable(
            "uuid-of-the-router",
            "uuid-of-the-node",
            "trace-1",
            "IO.gone@motherbee",
            "NODE_NOT_FOUND",
        );
        assert!(is_infrastructure_message(&notice));
        for (msg, sender) in [
            (MSG_CONFIG_SET, None),
            (MSG_CONFIG_SET, Some(" ")),
            (MSG_HELLO, Some("SY.admin@motherbee")),
            (MSG_WITHDRAW, Some("SY.admin@motherbee")),
            (MSG_TTL_EXCEEDED, Some("SY.admin@motherbee")),
            ("CONFIG_RESPONSE", Some("AI.other@worker1")),
            ("ADMIN_COMMAND_RESPONSE", Some("SY.admin@motherbee")),
        ] {
            assert!(
                is_infrastructure_message(&request("system", msg, sender)),
                "{msg} from {sender:?}"
            );
        }
        let mut no_msg = request("system", MSG_CONFIG_SET, Some("SY.admin@motherbee"));
        no_msg.meta.msg = None;
        assert!(is_infrastructure_message(&no_msg));

        for (msg, sender) in [
            (MSG_CONFIG_SET, "SY.admin@motherbee"),
            (MSG_NODE_STATUS_GET, "SY.orchestrator@worker1"),
            ("VAULT_SECRET_CHANGED", "SY.vault@motherbee"),
            (MSG_CONFIG_SET, "AI.other@worker1"),
        ] {
            assert!(
                !is_infrastructure_message(&request("system", msg, Some(sender))),
                "{msg} from {sender}"
            );
        }
    }

    /// FINDINGS A-339: a node's `NOT_TAKEN` (a busy WF's for an agent's answer, since 0.1.141) and
    /// an error answer (`<msg>.error`) are replies, in either kind the control plane takes, from
    /// any node: never answered. A name that only holds one of them is no reply.
    #[test]
    fn a_not_taken_and_an_error_answer_are_replies() {
        for (kind, msg) in [
            ("system", "NOT_TAKEN"),
            ("SYSTEM", "NOT_TAKEN"),
            ("admin", "NOT_TAKEN"),
            ("system", "ask.error"),
            ("admin", "CONFIG_SET.error"),
        ] {
            assert!(
                is_infrastructure_message(&request(kind, msg, Some("WF.sales@worker1"))),
                "{msg} as {kind}"
            );
        }
        for msg in ["not_taken", "NOT_TAKEN_AGAIN", "ask.error.v2", "ask.errors"] {
            assert!(
                !is_infrastructure_message(&request("system", msg, Some("WF.sales@worker1"))),
                "{msg}"
            );
        }
    }

    /// FINDINGS A-325: SY.vault's broadcast of a secret's change, in any case of its kind, from
    /// whoever names it; nothing else.
    #[test]
    fn the_vault_broadcast_is_told_apart() {
        for kind in ["system", "SYSTEM"] {
            assert!(is_vault_secret_changed(&request(
                kind,
                MSG_VAULT_SECRET_CHANGED,
                Some("SY.vault@motherbee")
            )));
        }
        assert!(is_vault_secret_changed(&request(
            "system",
            MSG_VAULT_SECRET_CHANGED,
            None
        )));
        assert!(!is_vault_secret_changed(&request(
            "user",
            MSG_VAULT_SECRET_CHANGED,
            Some("SY.vault@motherbee")
        )));
        assert!(!is_vault_secret_changed(&request(
            "system",
            "vault_secret_changed",
            Some("SY.vault@motherbee")
        )));
        assert!(!is_vault_secret_changed(&request(
            "system",
            MSG_CONFIG_SET,
            Some("SY.admin@motherbee")
        )));
    }

    /// The review of A-257: a burst of undelivered answers (a client that sent 200 turns and left)
    /// is one warning for each notice and reason, then one every 10 s with how many came since.
    #[test]
    fn the_router_notices_are_named_at_once_then_once_an_interval_for_each_reason() {
        let mut logs = BTreeMap::new();
        let start = Instant::now();
        let at = |secs: u64| start + std::time::Duration::from_secs(secs);
        assert_eq!(
            notice_line_due(&mut logs, MSG_UNREACHABLE, "NODE_NOT_FOUND", start),
            Some(1)
        );
        for _ in 0..67 {
            assert_eq!(
                notice_line_due(&mut logs, MSG_UNREACHABLE, "NODE_NOT_FOUND", at(1)),
                None
            );
        }
        // Another reason, or another notice, has its own line.
        assert_eq!(
            notice_line_due(&mut logs, MSG_UNREACHABLE, "LINK_BUSY", at(1)),
            Some(1)
        );
        assert_eq!(
            notice_line_due(&mut logs, MSG_TTL_EXCEEDED, "NODE_NOT_FOUND", at(1)),
            Some(1)
        );
        assert_eq!(
            notice_line_due(&mut logs, MSG_UNREACHABLE, "NODE_NOT_FOUND", at(10)),
            Some(68)
        );

        // Reasons past the kinds kept count together, so the counts stay bounded.
        for n in 0..100 {
            notice_line_due(&mut logs, MSG_UNREACHABLE, &format!("reason-{n}"), at(20));
        }
        assert!(logs.len() <= NOTICE_LOG_KINDS + 1, "{}", logs.len());
        assert!(logs.contains_key(&(MSG_UNREACHABLE.to_string(), OTHER_REASONS.to_string())));
    }
}
