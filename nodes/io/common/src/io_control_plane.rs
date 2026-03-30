use fluxbee_sdk::node_config::{
    build_node_config_response_message_runtime_src, parse_node_config_request,
    NodeConfigControlError, NodeConfigControlRequest, NodeConfigGetPayload, NodeConfigSetPayload,
    NODE_CONFIG_APPLY_MODE_REPLACE,
};
use fluxbee_sdk::protocol::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoNodeLifecycleState {
    #[serde(rename = "UNCONFIGURED")]
    Unconfigured,
    #[serde(rename = "CONFIGURED")]
    Configured,
    #[serde(rename = "FAILED_CONFIG")]
    FailedConfig,
}

impl IoNodeLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "UNCONFIGURED",
            Self::Configured => "CONFIGURED",
            Self::FailedConfig => "FAILED_CONFIG",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IoConfigSource {
    #[serde(rename = "dynamic")]
    Dynamic,
    #[serde(rename = "orchestrator_fallback")]
    OrchestratorFallback,
    #[serde(rename = "none")]
    None,
}

impl IoConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::OrchestratorFallback => "orchestrator_fallback",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IoControlPlaneErrorInfo {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IoControlPlaneState {
    pub current_state: IoNodeLifecycleState,
    pub config_source: IoConfigSource,
    pub schema_version: u32,
    pub config_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<IoControlPlaneErrorInfo>,
}

impl Default for IoControlPlaneState {
    fn default() -> Self {
        Self {
            current_state: IoNodeLifecycleState::Unconfigured,
            config_source: IoConfigSource::None,
            schema_version: 0,
            config_version: 0,
            effective_config: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IoControlPlaneRequest {
    Get(NodeConfigGetPayload),
    Set(NodeConfigSetPayload),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IoControlPlaneValidationError {
    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),
    #[error("missing node_name")]
    MissingNodeName,
    #[error("node_name mismatch (expected '{expected}', got '{got}')")]
    NodeNameMismatch { expected: String, got: String },
    #[error("invalid schema_version: must be >= 1")]
    InvalidSchemaVersion,
    #[error("unsupported apply_mode '{0}' (only 'replace' is supported in MVP)")]
    UnsupportedApplyMode(String),
    #[error("invalid payload.config: expected object")]
    InvalidConfigShape,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IoControlPlaneVersionError {
    #[error("config_version {incoming} must be greater than current {current}")]
    StaleConfigVersion { incoming: u64, current: u64 },
}

impl IoControlPlaneVersionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::StaleConfigVersion { .. } => "stale_config_version",
        }
    }
}

impl IoControlPlaneValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidEnvelope(_) => "invalid_envelope",
            Self::MissingNodeName => "invalid_config",
            Self::NodeNameMismatch { .. } => "invalid_config",
            Self::InvalidSchemaVersion => "invalid_config",
            Self::UnsupportedApplyMode(_) => "unsupported_apply_mode",
            Self::InvalidConfigShape => "invalid_config",
        }
    }
}

pub fn ensure_config_version_advances(
    incoming: u64,
    current: u64,
) -> Result<(), IoControlPlaneVersionError> {
    if incoming <= current {
        return Err(IoControlPlaneVersionError::StaleConfigVersion { incoming, current });
    }
    Ok(())
}

pub fn parse_and_validate_io_control_plane_request(
    msg: &Message,
    expected_node_name: &str,
) -> Result<IoControlPlaneRequest, IoControlPlaneValidationError> {
    let parsed = parse_node_config_request(msg)
        .map_err(|err| IoControlPlaneValidationError::InvalidEnvelope(map_envelope_error(err)))?;
    match parsed {
        NodeConfigControlRequest::Get(payload) => {
            validate_target_node(expected_node_name, payload.node_name.as_str())?;
            Ok(IoControlPlaneRequest::Get(payload))
        }
        NodeConfigControlRequest::Set(payload) => {
            validate_target_node(expected_node_name, payload.node_name.as_str())?;
            validate_set_payload(&payload)?;
            Ok(IoControlPlaneRequest::Set(payload))
        }
    }
}

pub fn build_io_config_get_response_payload(
    node_name: &str,
    state: &IoControlPlaneState,
    contract: Value,
) -> Value {
    let ok = state.effective_config.is_some();
    let error = if ok {
        Value::Null
    } else {
        json_error("node_not_configured", "No effective config available")
    };

    serde_json::json!({
        "ok": ok,
        "node_name": node_name,
        "state": state.current_state.as_str(),
        "config_source": state.config_source.as_str(),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "effective_config": state.effective_config,
        "contract": contract,
        "error": error,
    })
}

pub fn build_io_config_set_ok_payload(node_name: &str, state: &IoControlPlaneState) -> Value {
    serde_json::json!({
        "ok": true,
        "node_name": node_name,
        "state": state.current_state.as_str(),
        "config_source": state.config_source.as_str(),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "effective_config": state.effective_config,
        "error": Value::Null,
    })
}

pub fn build_io_config_set_error_payload(
    node_name: &str,
    state: &IoControlPlaneState,
    code: &str,
    message: impl Into<String>,
) -> Value {
    serde_json::json!({
        "ok": false,
        "node_name": node_name,
        "state": state.current_state.as_str(),
        "config_source": state.config_source.as_str(),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "effective_config": Value::Null,
        "error": json_error(code, message),
    })
}

pub fn build_io_config_response_message(incoming: &Message, payload: Value) -> Message {
    build_node_config_response_message_runtime_src(incoming, payload)
}

fn map_envelope_error(err: NodeConfigControlError) -> String {
    match err {
        NodeConfigControlError::InvalidRequest(msg)
        | NodeConfigControlError::InvalidMessage(msg) => msg,
        NodeConfigControlError::Json(json_err) => json_err.to_string(),
    }
}

fn validate_target_node(
    expected_node_name: &str,
    request_node_name: &str,
) -> Result<(), IoControlPlaneValidationError> {
    let expected = expected_node_name.trim();
    let requested = request_node_name.trim();
    if requested.is_empty() {
        return Err(IoControlPlaneValidationError::MissingNodeName);
    }
    if node_name_matches(expected, requested) {
        return Ok(());
    }
    Err(IoControlPlaneValidationError::NodeNameMismatch {
        expected: expected.to_string(),
        got: requested.to_string(),
    })
}

fn validate_set_payload(
    payload: &NodeConfigSetPayload,
) -> Result<(), IoControlPlaneValidationError> {
    if payload.schema_version == 0 {
        return Err(IoControlPlaneValidationError::InvalidSchemaVersion);
    }
    if payload.apply_mode != NODE_CONFIG_APPLY_MODE_REPLACE {
        return Err(IoControlPlaneValidationError::UnsupportedApplyMode(
            payload.apply_mode.clone(),
        ));
    }
    if !payload.config.is_object() {
        return Err(IoControlPlaneValidationError::InvalidConfigShape);
    }
    Ok(())
}

fn node_name_matches(expected: &str, requested: &str) -> bool {
    if expected == requested {
        return true;
    }

    let expected_local = expected.split('@').next().unwrap_or(expected);
    let requested_local = requested.split('@').next().unwrap_or(requested);
    expected_local == requested_local
}

fn json_error(code: &str, message: impl Into<String>) -> Value {
    serde_json::json!({
        "code": code,
        "message": message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::node_config::{
        build_node_config_get_message, build_node_config_set_message, NodeConfigEnvelopeOptions,
    };
    use serde_json::json;

    #[test]
    fn parse_get_accepts_fully_qualified_node_name() {
        let msg = build_node_config_get_message(
            "src-1",
            "IO.slack.T123@motherbee",
            &NodeConfigGetPayload {
                node_name: "IO.slack.T123@motherbee".to_string(),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-1",
        )
        .expect("build get");

        let parsed = parse_and_validate_io_control_plane_request(&msg, "IO.slack.T123@motherbee")
            .expect("validated");
        assert!(matches!(parsed, IoControlPlaneRequest::Get(_)));
    }

    #[test]
    fn parse_set_accepts_local_name_when_runtime_uses_fq_name() {
        let msg = build_node_config_set_message(
            "src-1",
            "IO.slack.T123@motherbee",
            &NodeConfigSetPayload {
                node_name: "IO.slack.T123".to_string(),
                schema_version: 1,
                config_version: 7,
                apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
                config: json!({"io":{"dst_node":"AI.frontdesk.gov@motherbee"}}),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-2",
        )
        .expect("build set");

        let parsed = parse_and_validate_io_control_plane_request(&msg, "IO.slack.T123@motherbee")
            .expect("validated");
        assert!(matches!(parsed, IoControlPlaneRequest::Set(_)));
    }

    #[test]
    fn parse_set_rejects_unsupported_apply_mode() {
        let msg = build_node_config_set_message(
            "src-1",
            "IO.slack.T123@motherbee",
            &NodeConfigSetPayload {
                node_name: "IO.slack.T123@motherbee".to_string(),
                schema_version: 1,
                config_version: 7,
                apply_mode: "merge_patch".to_string(),
                config: json!({"io":{"dst_node":"resolve"}}),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-3",
        )
        .expect("build set");

        let err = parse_and_validate_io_control_plane_request(&msg, "IO.slack.T123@motherbee")
            .expect_err("must fail");
        assert_eq!(
            err,
            IoControlPlaneValidationError::UnsupportedApplyMode("merge_patch".to_string())
        );
        assert_eq!(err.code(), "unsupported_apply_mode");
    }

    #[test]
    fn parse_set_rejects_non_object_config() {
        let msg = build_node_config_set_message(
            "src-1",
            "IO.slack.T123@motherbee",
            &NodeConfigSetPayload {
                node_name: "IO.slack.T123@motherbee".to_string(),
                schema_version: 1,
                config_version: 9,
                apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
                config: Value::String("invalid".to_string()),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-4",
        )
        .expect("build set");

        let err = parse_and_validate_io_control_plane_request(&msg, "IO.slack.T123@motherbee")
            .expect_err("must fail");
        assert_eq!(err, IoControlPlaneValidationError::InvalidConfigShape);
        assert_eq!(err.code(), "invalid_config");
    }

    #[test]
    fn parse_set_rejects_node_name_mismatch() {
        let msg = build_node_config_set_message(
            "src-1",
            "IO.slack.T123@motherbee",
            &NodeConfigSetPayload {
                node_name: "IO.slack.T999@motherbee".to_string(),
                schema_version: 1,
                config_version: 7,
                apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
                config: json!({}),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-5",
        )
        .expect("build set");

        let err = parse_and_validate_io_control_plane_request(&msg, "IO.slack.T123@motherbee")
            .expect_err("must fail");
        assert!(matches!(
            err,
            IoControlPlaneValidationError::NodeNameMismatch { .. }
        ));
    }

    #[test]
    fn config_get_response_reports_unconfigured_when_missing_effective_config() {
        let state = IoControlPlaneState::default();
        let payload = build_io_config_get_response_payload(
            "IO.slack.T123@motherbee",
            &state,
            serde_json::json!({"supports":["CONFIG_GET","CONFIG_SET"]}),
        );

        assert_eq!(payload.get("ok"), Some(&Value::Bool(false)));
        assert_eq!(
            payload.get("state"),
            Some(&Value::String("UNCONFIGURED".to_string()))
        );
        assert_eq!(
            payload
                .get("error")
                .and_then(|v| v.get("code"))
                .and_then(Value::as_str),
            Some("node_not_configured")
        );
    }

    #[test]
    fn config_response_message_keeps_trace_id_and_sets_runtime_src_empty() {
        let incoming = build_node_config_get_message(
            "caller-1",
            "IO.slack.T123@motherbee",
            &NodeConfigGetPayload {
                node_name: "IO.slack.T123@motherbee".to_string(),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-9",
        )
        .expect("build incoming");

        let payload = serde_json::json!({"ok":true,"node_name":"IO.slack.T123@motherbee"});
        let response = build_io_config_response_message(&incoming, payload);

        assert_eq!(response.routing.trace_id, "trace-9");
        assert_eq!(response.meta.msg.as_deref(), Some("CONFIG_RESPONSE"));
    }

    #[test]
    fn config_version_accepts_strictly_greater_values() {
        assert!(ensure_config_version_advances(8, 7).is_ok());
    }

    #[test]
    fn config_version_rejects_equal_as_stale_replay() {
        let err = ensure_config_version_advances(7, 7).expect_err("must be stale");
        assert_eq!(
            err,
            IoControlPlaneVersionError::StaleConfigVersion {
                incoming: 7,
                current: 7,
            }
        );
        assert_eq!(err.code(), "stale_config_version");
    }

    #[test]
    fn config_version_rejects_lower_values() {
        let err = ensure_config_version_advances(6, 7).expect_err("must be stale");
        assert_eq!(
            err,
            IoControlPlaneVersionError::StaleConfigVersion {
                incoming: 6,
                current: 7,
            }
        );
        assert_eq!(err.code(), "stale_config_version");
    }
}
