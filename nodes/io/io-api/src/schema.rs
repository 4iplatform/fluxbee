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
    key_count: usize,
) -> Value {
    let subject_mode =
        extract_subject_mode(Some(effective)).unwrap_or_else(|| "unknown".to_string());
    let accepted_content_types = extract_accepted_content_types(Some(effective));
    let subject = if subject_mode == "explicit_subject" {
        serde_json::json!({
            "required": true,
            "allowed": true,
            "lookup_key_field": "external_user_id",
            "tenant_source": "authenticated_api_key",
            "optional_identity_candidates": ["display_name", "email", "company_name", "attributes"]
        })
    } else {
        serde_json::json!({
            "required": false,
            "allowed": false,
            "resolution": "derived_from_authenticated_caller"
        })
    };
    let required_fields = if subject_mode == "explicit_subject" {
        serde_json::json!({
            "json": ["subject", "message"],
            "multipart": ["metadata"]
        })
    } else {
        serde_json::json!({
            "json": ["message"],
            "multipart": ["metadata"]
        })
    };
    serde_json::json!({
        "status": lifecycle_status(&state.current_state),
        "node_name": node_name,
        "runtime": "io.api",
        "contract_version": 1,
        "config_version": state.config_version,
        "auth": {
            "mode": effective.get("auth").and_then(|auth| auth.get("mode")).and_then(Value::as_str).unwrap_or("api_key"),
            "transport": "Authorization: Bearer <token>",
            "active_key_count": key_count
        },
        "ingress": {
            "subject_mode": subject_mode,
            "accepted_content_types": accepted_content_types
        },
        "required_fields": required_fields,
        "subject": subject,
        "attachments": {
            "supported": accepted_content_types.iter().any(|value| value == "multipart/form-data"),
            "mode": "multipart"
        },
        "relay": {
            "config_path": "config.io.relay.*",
            "effective": effective.get("io").and_then(|io| io.get("relay")).cloned().unwrap_or(Value::Null)
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
        "runtime": "io.api",
        "contract_version": 1,
        "effective_schema": Value::Null,
        "required_configuration": adapter_contract.required_fields(),
        "last_error": state.last_error,
    })
}

pub(crate) fn extract_accepted_content_types(effective_config: Option<&Value>) -> Vec<String> {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("accepted_content_types"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| vec!["application/json".to_string()])
}

pub(crate) fn extract_max_request_bytes(effective_config: Option<&Value>) -> usize {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("max_request_bytes"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(256 * 1024)
}

pub(crate) fn extract_subject_mode(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("subject_mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}
