use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::NodeSecretDescriptor;
use serde_json::{Map, Value};

pub struct IoSlackAdapterConfigContract;

impl IoAdapterConfigContract for IoSlackAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.slack"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        // Family pattern (mirrors io.linkedhelper): the Slack credentials are NEVER in config — config
        // carries only a vault-key REFERENCE, resolved from SY.vault at runtime via vault.get(key).
        &[
            "config.slack.auth.type",
            "config.slack.auth.resource_type",
            "config.slack.auth.key",
            "config.io.workspace_id",
            "config.io.conversation_id",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.io.dst_node",
            "config.io.relay.window_ms",
            "config.io.relay.max_open_sessions",
            "config.io.relay.max_fragments_per_session",
            "config.io.relay.max_bytes_per_session",
            "config.identity.target",
            "config.identity.timeout_ms",
            "config.io.blob.*",
            "config.node.*",
            "config.runtime.*",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "Slack credentials use slack.auth.type=vault_ref; inline tokens are not accepted. The \
             secret (an object {app_token, bot_token}) is resolved from SY.vault at runtime using \
             slack.auth.key, validating metadata.resource_type == slack.",
            "MVP apply mode is replace only.",
            "workspace_id + conversation_id identify the stable local Slack binding used for own-ICH \
             registration.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        ensure_object_field(&mut cfg, "slack")?;
        ensure_object_field(&mut cfg, "io")?;
        ensure_optional_object_field(&mut cfg, "identity")?;
        ensure_optional_object_field(&mut cfg, "node")?;
        ensure_optional_object_field(&mut cfg, "runtime")?;

        {
            let slack = cfg.get("slack").and_then(Value::as_object).ok_or_else(|| {
                IoAdapterConfigError::Internal("slack object missing after normalization".to_string())
            })?;
            let auth = slack
                .get("auth")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    IoAdapterConfigError::InvalidConfig(
                        "slack.auth is required (a vault reference: {type:\"vault_ref\", \
                         resource_type:\"slack\", key:\"<vault key>\"})"
                            .to_string(),
                    )
                })?;
            let auth_type = auth.get("type").and_then(Value::as_str).map(str::trim);
            if auth_type != Some("vault_ref") {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "slack.auth.type must be \"vault_ref\"".to_string(),
                ));
            }
            require_non_empty_string(auth, "resource_type", "slack.auth.resource_type")?;
            require_non_empty_string(auth, "key", "slack.auth.key")?;
        }

        let io_obj = cfg
            .get_mut("io")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| IoAdapterConfigError::Internal("io missing".to_string()))?;
        require_non_empty_string(io_obj, "workspace_id", "io.workspace_id")?;
        require_non_empty_string(io_obj, "conversation_id", "io.conversation_id")?;
        // NO dst_node default: absence means "let the router resolve" (None -> Destination::Resolve),
        // like io-api. A literal "resolve" string is a bogus unicast target, so it is never injected.
        ensure_optional_object_member(io_obj, "relay", "io.relay")?;
        if let Some(relay) = io_obj.get("relay").and_then(Value::as_object) {
            validate_optional_non_negative_integer(relay, "window_ms", "io.relay.window_ms")?;
            validate_optional_positive_integer(
                relay,
                "max_open_sessions",
                "io.relay.max_open_sessions",
            )?;
            validate_optional_positive_integer(
                relay,
                "max_fragments_per_session",
                "io.relay.max_fragments_per_session",
            )?;
            validate_optional_positive_integer(
                relay,
                "max_bytes_per_session",
                "io.relay.max_bytes_per_session",
            )?;
        }

        Ok(Value::Object(cfg))
    }

    fn redact_effective_config(&self, effective: &Value) -> Value {
        // Credentials never live in config anymore — slack.auth.key is a vault REFERENCE, not a
        // secret (mirrors io.linkedhelper value_redacted=false). Nothing to redact.
        effective.clone()
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let key_configured = effective
            .and_then(|v| v.get("slack"))
            .and_then(|s| s.get("auth"))
            .and_then(Value::as_object)
            .map(|auth| has_non_empty_string(auth, "key"))
            .unwrap_or(false);

        let mut cred = NodeSecretDescriptor::new("config.slack.auth.key", "slack_credentials");
        cred.required = true;
        cred.configured = key_configured;
        cred.value_redacted = false; // the field is a vault key reference, not the secret itself
        cred.persistence = "vault".to_string();
        vec![cred]
    }
}

fn ensure_object_field(
    root: &mut Map<String, Value>,
    field: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        root.insert(field.to_string(), Value::Object(Map::new()));
        return Ok(());
    }
    if root.get(field).and_then(Value::as_object).is_none() {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{field} must be an object"
        )));
    }
    Ok(())
}

fn ensure_optional_object_field(
    root: &mut Map<String, Value>,
    field: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        return Ok(());
    }
    if root.get(field).and_then(Value::as_object).is_none() {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{field} must be an object when present"
        )));
    }
    Ok(())
}

fn has_non_empty_string(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn require_non_empty_string(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if has_non_empty_string(root, field) {
        Ok(())
    } else {
        Err(IoAdapterConfigError::InvalidConfig(format!(
            "{label} is required"
        )))
    }
}

fn ensure_optional_object_member(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        return Ok(());
    }
    if root.get(field).and_then(Value::as_object).is_none() {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{label} must be an object when present"
        )));
    }
    Ok(())
}

fn validate_optional_non_negative_integer(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        return Ok(());
    }
    if !matches!(root.get(field), Some(Value::Number(number)) if number.as_u64().is_some()) {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{label} must be a non-negative integer"
        )));
    }
    Ok(())
}

fn validate_optional_positive_integer(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        return Ok(());
    }
    let is_positive = root
        .get(field)
        .and_then(Value::as_u64)
        .is_some_and(|value| value > 0);
    if !is_positive {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{label} must be a positive integer"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_auth() -> Value {
        json!({"type":"vault_ref","resource_type":"slack","key":"slack/IO.slack@motherbee"})
    }

    #[test]
    fn validate_materialize_requires_vault_ref_auth_and_binding() {
        let contract = IoSlackAdapterConfigContract;
        // missing auth
        let err = contract
            .validate_and_materialize(&json!({"slack":{}, "io":{"workspace_id":"T1","conversation_id":"C1"}}))
            .expect_err("must fail missing auth");
        assert!(matches!(err, IoAdapterConfigError::InvalidConfig(m) if m.contains("slack.auth is required")));

        // wrong auth type
        let err = contract
            .validate_and_materialize(&json!({
                "slack":{"auth":{"type":"inline","resource_type":"slack","key":"k"}},
                "io":{"workspace_id":"T1","conversation_id":"C1"}
            }))
            .expect_err("must reject non vault_ref");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig("slack.auth.type must be \"vault_ref\"".to_string())
        );

        // valid, and NO dst_node is injected
        let out = contract
            .validate_and_materialize(&json!({
                "slack":{"auth": valid_auth()},
                "io":{"workspace_id":"T123","conversation_id":"C456"}
            }))
            .expect("must pass");
        assert!(out
            .get("io")
            .and_then(Value::as_object)
            .and_then(|io| io.get("dst_node"))
            .is_none());
    }

    #[test]
    fn redact_effective_config_is_noop_for_vault_ref() {
        let contract = IoSlackAdapterConfigContract;
        let cfg = json!({"slack":{"auth": valid_auth()}, "io":{"workspace_id":"T1"}});
        assert_eq!(contract.redact_effective_config(&cfg), cfg);
    }

    #[test]
    fn secret_descriptor_is_vault_key_ref() {
        let contract = IoSlackAdapterConfigContract;
        let d = contract.secret_descriptors(Some(&json!({"slack":{"auth": valid_auth()}})));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].field, "config.slack.auth.key");
        assert!(d[0].configured);
        assert!(!d[0].value_redacted);
        assert_eq!(d[0].persistence, "vault");
    }

    #[test]
    fn validate_materialize_accepts_relay_config_surface() {
        let contract = IoSlackAdapterConfigContract;
        let out = contract
            .validate_and_materialize(&json!({
                "slack":{"auth": valid_auth()},
                "io": {
                    "workspace_id": "T123",
                    "conversation_id": "C456",
                    "relay": {"window_ms": 2500, "max_open_sessions": 2000, "max_fragments_per_session": 6, "max_bytes_per_session": 131072}
                }
            }))
            .expect("must accept relay config");
        assert_eq!(
            out.get("io")
                .and_then(Value::as_object)
                .and_then(|io| io.get("relay"))
                .and_then(Value::as_object)
                .and_then(|relay| relay.get("window_ms"))
                .and_then(Value::as_u64),
            Some(2500)
        );
    }

    #[test]
    fn validate_materialize_rejects_invalid_relay_limits() {
        let contract = IoSlackAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({
                "slack":{"auth": valid_auth()},
                "io": {"workspace_id": "T123", "conversation_id": "C456", "relay": {"max_open_sessions": 0}}
            }))
            .expect_err("must reject zero max_open_sessions");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig(
                "io.relay.max_open_sessions must be a positive integer".to_string()
            )
        );
    }
}
