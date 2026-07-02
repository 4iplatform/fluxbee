use io_common::io_adapter_config::{build_io_adapter_contract_payload, IoAdapterConfigContract};
use io_common::io_control_plane::{IoControlPlaneState, IoNodeLifecycleState};
use serde_json::Value;

pub(crate) fn lifecycle_status(state: &IoNodeLifecycleState) -> &'static str {
    match state {
        IoNodeLifecycleState::Unconfigured => "unconfigured",
        IoNodeLifecycleState::Configured => "configured",
        IoNodeLifecycleState::FailedConfig => "failed_config",
    }
}

pub(crate) fn build_configured_schema(
    node_name: &str,
    state: &IoControlPlaneState,
    effective: &Value,
    adapter_contract: &dyn IoAdapterConfigContract,
    adapter_count: usize,
) -> Value {
    serde_json::json!({
        "status": lifecycle_status(&state.current_state),
        "node_name": node_name,
        "runtime": "io.linkedhelper",
        "contract_version": 1,
        "config_version": state.config_version,
        "transport": {
            "endpoint": "POST /v1/poll",
            "protocol": "http",
            "auth": "Authorization: Bearer <adapter_secret>",
            "adapter_header": "X-Fluxbee-Adapter-Id",
            "payload_binding": ["adapter_id", "managed_instance_id", "local_instance_id"]
        },
        "channel": {
            "mode": effective.get("mode").cloned().unwrap_or(Value::String("direct_http_intermediate".to_string())),
            "response_families": ["ack", "result", "heartbeat"],
            "active_adapter_count": adapter_count
        },
        "binding": {
            "managed_instance_id": effective.get("managed_instance_id").cloned().unwrap_or(Value::Null),
            "tenant_id": effective.get("tenant_id").cloned().unwrap_or(Value::Null),
            "adapter": effective.get("adapter").cloned().unwrap_or(Value::Null),
        },
        "ingress": {
            "listen": effective.get("listen").cloned().unwrap_or(Value::Null),
            "http": effective.get("http").cloned().unwrap_or(Value::Null),
        },
        "secrets": build_io_adapter_contract_payload(adapter_contract, Some(effective))
            .get("secrets")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        "last_error": state.last_error.clone(),
    })
}

pub(crate) fn build_unconfigured_schema(
    node_name: &str,
    state: &IoControlPlaneState,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> Value {
    serde_json::json!({
        "status": lifecycle_status(&state.current_state),
        "node_name": node_name,
        "runtime": "io.linkedhelper",
        "contract_version": 1,
        "effective_schema": Value::Null,
        "required_configuration": adapter_contract.required_fields(),
        "last_error": state.last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_common::io_linkedhelper_adapter_config::IoLinkedHelperAdapterConfigContract;
    use serde_json::json;

    fn effective_config() -> Value {
        json!({
            "managed_instance_id": "lhmi_test",
            "tenant_id": "tnt:00000000-0000-0000-0000-000000000001",
            "adapter": {
                "adapter_id": "adp_test",
                "local_instance_id": "288037",
                "auth": {
                    "type": "vault_ref",
                    "resource_type": "linkedhelper_adapter",
                    "key": "linkedhelper:adapters:adp_test"
                }
            },
            "listen": { "address": "0.0.0.0", "port": 19091 },
            "mode": "direct_http_intermediate"
        })
    }

    #[test]
    fn configured_schema_advertises_plain_http_and_binding() {
        let mut state = IoControlPlaneState::default();
        state.current_state = IoNodeLifecycleState::Configured;
        state.config_version = 7;
        let effective = effective_config();
        let schema = build_configured_schema(
            "IO.linkedhelper.lhmi_test@motherbee",
            &state,
            &effective,
            &IoLinkedHelperAdapterConfigContract,
            1,
        );

        assert_eq!(schema["status"], "configured");
        assert_eq!(schema["runtime"], "io.linkedhelper");
        assert_eq!(schema["config_version"], 7);
        // Regression guard for the contract-drift fix: the node serves plain
        // HTTP in the intermediate path, so the schema must advertise "http".
        assert_eq!(schema["transport"]["protocol"], "http");
        assert_eq!(schema["transport"]["endpoint"], "POST /v1/poll");
        assert_eq!(schema["channel"]["active_adapter_count"], 1);
        assert_eq!(schema["binding"]["managed_instance_id"], "lhmi_test");
        assert_eq!(
            schema["binding"]["tenant_id"],
            "tnt:00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(schema["ingress"]["listen"]["port"], 19091);
        assert!(schema["secrets"].is_array());
    }

    #[test]
    fn unconfigured_schema_has_null_effective_and_required_fields() {
        let state = IoControlPlaneState::default();
        let schema = build_unconfigured_schema(
            "IO.linkedhelper.local",
            &state,
            &IoLinkedHelperAdapterConfigContract,
        );
        assert_eq!(schema["runtime"], "io.linkedhelper");
        assert_eq!(schema["status"], "unconfigured");
        assert_eq!(schema["effective_schema"], Value::Null);
        assert!(schema.get("required_configuration").is_some());
    }

    #[test]
    fn lifecycle_status_maps_all_states() {
        assert_eq!(
            lifecycle_status(&IoNodeLifecycleState::Unconfigured),
            "unconfigured"
        );
        assert_eq!(
            lifecycle_status(&IoNodeLifecycleState::Configured),
            "configured"
        );
        assert_eq!(
            lifecycle_status(&IoNodeLifecycleState::FailedConfig),
            "failed_config"
        );
    }
}
