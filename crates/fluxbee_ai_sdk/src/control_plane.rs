//! The control plane of an AI node (ai.generic, SY.frontdesk.gov): which `system` requests it runs,
//! and from whom.

use fluxbee_sdk::protocol::{
    is_system_kind, MSG_CONFIG_GET, MSG_CONFIG_SET, MSG_NODE_HELP, MSG_NODE_STATUS_GET,
    PRIMARY_HIVE_ID,
};

use crate::message::Message;

/// A liveness probe: the node's name and state.
pub const MSG_PING: &str = "PING";
/// The node's state, behavior and config versions.
pub const MSG_STATUS: &str = "STATUS";

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
}
