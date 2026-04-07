use std::fmt;

use serde::{Deserialize, Serialize};

use crate::protocol::{MSG_CONFIG_GET, MSG_CONFIG_SET};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    SendMessage,
    Read,
    Write,
    SystemConfig,
    TopologyChange,
    ExternalAction,
    IdentityChange,
    WorkflowStep,
    NodeLifecycle,
}

impl ActionClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SendMessage => "send_message",
            Self::Read => "read",
            Self::Write => "write",
            Self::SystemConfig => "system_config",
            Self::TopologyChange => "topology_change",
            Self::ExternalAction => "external_action",
            Self::IdentityChange => "identity_change",
            Self::WorkflowStep => "workflow_step",
            Self::NodeLifecycle => "node_lifecycle",
        }
    }
}

impl fmt::Display for ActionClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str((*self).as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionResult {
    Blocked,
    Applied,
    Failed,
}

impl ActionResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Applied => "applied",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for ActionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str((*self).as_str())
    }
}

pub fn classify_routed_message(msg_type: &str) -> Option<ActionClass> {
    match msg_type {
        "system" | "admin" | "query_response" => None,
        _ => Some(ActionClass::SendMessage),
    }
}

pub fn classify_admin_action(action: &str) -> Option<ActionClass> {
    match action {
        "list_hives"
        | "get_hive"
        | "list_nodes"
        | "get_node_status"
        | "get_node_config"
        | "get_node_state"
        | "get_versions"
        | "list_versions"
        | "inventory"
        | "opa_get_status"
        | "opa_get_policy"
        | "list_routes"
        | "list_vpns"
        | "list_runtimes"
        | "get_runtime"
        | "get_runtimes"
        | "node_control_config_get" => Some(ActionClass::Read),
        "set_node_config" | "node_control_config_set" => Some(ActionClass::Write),
        "add_route"
        | "delete_route"
        | "add_vpn"
        | "delete_vpn"
        | "opa_apply"
        | "opa_compile_apply"
        | "opa_rollback"
        | "update_policy_matrix"
        | "clear_override"
        | "sync_hint"
        | "update"
        | "set_storage"
        | "remove_runtime_version" => Some(ActionClass::SystemConfig),
        "add_hive" | "remove_hive" => Some(ActionClass::TopologyChange),
        "run_node" | "kill_node" | "remove_node_instance" => Some(ActionClass::NodeLifecycle),
        "create_tenant"
        | "update_tenant"
        | "approve_tenant"
        | "list_ilks"
        | "get_ilk"
        | "delete_ilk"
        | "list_vocabulary"
        | "add_vocabulary"
        | "deprecate_vocabulary" => Some(ActionClass::IdentityChange),
        "send_node_message" => Some(ActionClass::SendMessage),
        _ => None,
    }
}

pub fn classify_system_message(msg: &str) -> Option<ActionClass> {
    match msg {
        MSG_CONFIG_GET
        | "STATUS"
        | "PING"
        | "NODE_CONFIG_GET"
        | "NODE_STATE_GET"
        | "NODE_STATUS_GET"
        | "GET_VERSIONS"
        | "GET_RUNTIMES"
        | "LIST_NODES"
        | "GET_RUNTIME"
        | "INVENTORY_REQUEST"
        | "ILK_LIST"
        | "ILK_GET" => Some(ActionClass::Read),
        MSG_CONFIG_SET | "NODE_CONFIG_SET" => Some(ActionClass::Write),
        "RUNTIME_UPDATE"
        | "SYSTEM_UPDATE"
        | "SYSTEM_SYNC_HINT"
        | "OPA_APPLY"
        | "OPA_COMPILE_APPLY"
        | "OPA_ROLLBACK" => Some(ActionClass::SystemConfig),
        "ADD_HIVE_FINALIZE" | "REMOVE_HIVE_CLEANUP" => Some(ActionClass::TopologyChange),
        "SPAWN_NODE" | "KILL_NODE" | "REMOVE_NODE_INSTANCE" => Some(ActionClass::NodeLifecycle),
        _ => None,
    }
}

pub const fn action_class_requires_result(action_class: ActionClass) -> bool {
    !matches!(action_class, ActionClass::SendMessage | ActionClass::Read)
}

pub fn derive_action_outcome(
    action_class: Option<ActionClass>,
    status: Option<&str>,
    error_code: Option<&str>,
) -> (Option<ActionResult>, Option<&'static str>) {
    let Some(_action_class) = action_class.filter(|class| action_class_requires_result(*class)) else {
        return (None, None);
    };

    if status.is_some_and(|value| value.eq_ignore_ascii_case("ok")) {
        return (Some(ActionResult::Applied), None);
    }

    if status.is_some_and(|value| value.eq_ignore_ascii_case("blocked"))
        || error_code.is_some_and(|value| value.eq_ignore_ascii_case("FORBIDDEN"))
    {
        return (Some(ActionResult::Blocked), Some("node"));
    }

    if status.is_some() {
        return (Some(ActionResult::Failed), None);
    }

    (None, None)
}

#[cfg(test)]
mod tests {
    use super::{
        action_class_requires_result, classify_admin_action, classify_routed_message,
        classify_system_message, derive_action_outcome, ActionClass, ActionResult,
    };

    #[test]
    fn classifies_known_admin_actions() {
        assert_eq!(classify_admin_action("run_node"), Some(ActionClass::NodeLifecycle));
        assert_eq!(
            classify_admin_action("set_node_config"),
            Some(ActionClass::Write)
        );
        assert_eq!(
            classify_admin_action("send_node_message"),
            Some(ActionClass::SendMessage)
        );
    }

    #[test]
    fn classifies_known_system_messages() {
        assert_eq!(classify_system_message("SPAWN_NODE"), Some(ActionClass::NodeLifecycle));
        assert_eq!(classify_system_message("NODE_CONFIG_GET"), Some(ActionClass::Read));
        assert_eq!(classify_system_message("CONFIG_SET"), Some(ActionClass::Write));
    }

    #[test]
    fn classifies_routed_messages_only_when_evaluable() {
        assert_eq!(classify_routed_message("user"), Some(ActionClass::SendMessage));
        assert_eq!(classify_routed_message("query"), Some(ActionClass::SendMessage));
        assert_eq!(classify_routed_message("system"), None);
        assert_eq!(classify_routed_message("admin"), None);
        assert_eq!(classify_routed_message("query_response"), None);
    }

    #[test]
    fn marks_only_evaluable_classes_as_result_driven() {
        assert!(!action_class_requires_result(ActionClass::SendMessage));
        assert!(!action_class_requires_result(ActionClass::Read));
        assert!(action_class_requires_result(ActionClass::SystemConfig));
    }

    #[test]
    fn derives_applied_blocked_and_failed_outcomes() {
        assert_eq!(
            derive_action_outcome(Some(ActionClass::SystemConfig), Some("ok"), None),
            (Some(ActionResult::Applied), None)
        );
        assert_eq!(
            derive_action_outcome(
                Some(ActionClass::NodeLifecycle),
                Some("error"),
                Some("FORBIDDEN")
            ),
            (Some(ActionResult::Blocked), Some("node"))
        );
        assert_eq!(
            derive_action_outcome(
                Some(ActionClass::TopologyChange),
                Some("error"),
                Some("IO_ERROR")
            ),
            (Some(ActionResult::Failed), None)
        );
        assert_eq!(
            derive_action_outcome(Some(ActionClass::Read), Some("ok"), None),
            (None, None)
        );
    }
}
