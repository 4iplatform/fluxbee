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
            "protocol": "http_over_https",
            "auth": "Authorization: Bearer <installation_key>",
            "adapter_header": "X-Fluxbee-Adapter-Id"
        },
        "channel": {
            "mode": "polling",
            "response_families": ["ack", "result", "heartbeat"],
            "active_adapter_count": adapter_count
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
