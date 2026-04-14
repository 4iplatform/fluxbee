use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::{NodeSecretDescriptor, NODE_SECRET_REDACTION_TOKEN};
use serde_json::{Map, Value};

pub struct IoSlackAdapterConfigContract;

impl IoAdapterConfigContract for IoSlackAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.slack"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.slack.app_token | config.slack.app_token_ref",
            "config.slack.bot_token | config.slack.bot_token_ref",
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
            "For each Slack token, either inline token or *_ref is accepted.",
            "Secrets must be redacted in responses and logs.",
            "MVP apply mode is replace only.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        ensure_object_field(&mut cfg, "slack")?;
        ensure_optional_object_field(&mut cfg, "io")?;
        ensure_optional_object_field(&mut cfg, "identity")?;
        ensure_optional_object_field(&mut cfg, "node")?;
        ensure_optional_object_field(&mut cfg, "runtime")?;

        {
            let slack = cfg.get("slack").and_then(Value::as_object).ok_or_else(|| {
                IoAdapterConfigError::Internal(
                    "slack object missing after normalization".to_string(),
                )
            })?;

            let has_app = has_non_empty_string(slack, "app_token")
                || has_non_empty_string(slack, "slack_app_token")
                || has_non_empty_string(slack, "app_token_ref")
                || has_non_empty_string(slack, "slack_app_token_ref");
            if !has_app {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "missing required Slack app credential: slack.app_token or slack.app_token_ref"
                        .to_string(),
                ));
            }

            let has_bot = has_non_empty_string(slack, "bot_token")
                || has_non_empty_string(slack, "slack_bot_token")
                || has_non_empty_string(slack, "bot_token_ref")
                || has_non_empty_string(slack, "slack_bot_token_ref");
            if !has_bot {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "missing required Slack bot credential: slack.bot_token or slack.bot_token_ref"
                        .to_string(),
                ));
            }
        }

        if let Some(io_obj) = cfg.get_mut("io").and_then(Value::as_object_mut) {
            io_obj
                .entry("dst_node".to_string())
                .or_insert(Value::String("resolve".to_string()));
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
        }

        Ok(Value::Object(cfg))
    }

    fn redact_effective_config(&self, effective: &Value) -> Value {
        let mut redacted = effective.clone();
        let Some(root) = redacted.as_object_mut() else {
            return redacted;
        };
        let Some(slack) = root.get_mut("slack").and_then(Value::as_object_mut) else {
            return redacted;
        };

        redact_secret_field(slack, "app_token");
        redact_secret_field(slack, "slack_app_token");
        redact_secret_field(slack, "bot_token");
        redact_secret_field(slack, "slack_bot_token");
        redacted
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let app_configured = effective
            .and_then(|v| v.get("slack"))
            .and_then(Value::as_object)
            .map(has_slack_app_secret)
            .unwrap_or(false);
        let bot_configured = effective
            .and_then(|v| v.get("slack"))
            .and_then(Value::as_object)
            .map(has_slack_bot_secret)
            .unwrap_or(false);

        let mut app = NodeSecretDescriptor::new("config.slack.app_token", "slack_app_token");
        app.required = true;
        app.configured = app_configured;

        let mut bot = NodeSecretDescriptor::new("config.slack.bot_token", "slack_bot_token");
        bot.required = true;
        bot.configured = bot_configured;

        vec![app, bot]
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

fn redact_secret_field(map: &mut Map<String, Value>, key: &str) {
    if map.get(key).and_then(Value::as_str).is_some() {
        map.insert(
            key.to_string(),
            Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()),
        );
    }
}

fn has_slack_app_secret(slack: &Map<String, Value>) -> bool {
    has_non_empty_string(slack, "app_token")
        || has_non_empty_string(slack, "slack_app_token")
        || has_non_empty_string(slack, "app_token_ref")
        || has_non_empty_string(slack, "slack_app_token_ref")
}

fn has_slack_bot_secret(slack: &Map<String, Value>) -> bool {
    has_non_empty_string(slack, "bot_token")
        || has_non_empty_string(slack, "slack_bot_token")
        || has_non_empty_string(slack, "bot_token_ref")
        || has_non_empty_string(slack, "slack_bot_token_ref")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_materialize_requires_slack_credentials_and_sets_dst_default() {
        let contract = IoSlackAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({"slack":{}}))
            .expect_err("must fail missing secrets");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig(
                "missing required Slack app credential: slack.app_token or slack.app_token_ref"
                    .to_string()
            )
        );

        let out = contract
            .validate_and_materialize(&json!({
                "slack": {
                    "app_token_ref": "env:SLACK_APP_TOKEN",
                    "bot_token_ref": "env:SLACK_BOT_TOKEN"
                },
                "io": {}
            }))
            .expect("must pass");
        assert_eq!(
            out.get("io")
                .and_then(Value::as_object)
                .and_then(|io| io.get("dst_node"))
                .and_then(Value::as_str),
            Some("resolve")
        );
    }

    #[test]
    fn redact_effective_config_masks_inline_tokens() {
        let contract = IoSlackAdapterConfigContract;
        let redacted = contract.redact_effective_config(&json!({
            "slack": {
                "app_token": "xapp-123",
                "bot_token": "xoxb-123",
                "app_token_ref": "env:SLACK_APP_TOKEN"
            }
        }));
        assert_eq!(
            redacted
                .get("slack")
                .and_then(Value::as_object)
                .and_then(|s| s.get("app_token"))
                .and_then(Value::as_str),
            Some(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(
            redacted
                .get("slack")
                .and_then(Value::as_object)
                .and_then(|s| s.get("bot_token"))
                .and_then(Value::as_str),
            Some(NODE_SECRET_REDACTION_TOKEN)
        );
    }

    #[test]
    fn secret_descriptors_mark_configured_when_refs_present() {
        let contract = IoSlackAdapterConfigContract;
        let descriptors = contract.secret_descriptors(Some(&json!({
            "slack": {
                "app_token_ref": "env:SLACK_APP_TOKEN",
                "bot_token_ref": "env:SLACK_BOT_TOKEN"
            }
        })));
        assert_eq!(descriptors.len(), 2);
        assert!(descriptors[0].configured);
        assert!(descriptors[1].configured);
    }

    #[test]
    fn validate_materialize_accepts_relay_config_surface() {
        let contract = IoSlackAdapterConfigContract;
        let out = contract
            .validate_and_materialize(&json!({
                "slack": {
                    "app_token_ref": "env:SLACK_APP_TOKEN",
                    "bot_token_ref": "env:SLACK_BOT_TOKEN"
                },
                "io": {
                    "relay": {
                        "window_ms": 2500,
                        "max_open_sessions": 2000,
                        "max_fragments_per_session": 6,
                        "max_bytes_per_session": 131072
                    }
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
                "slack": {
                    "app_token_ref": "env:SLACK_APP_TOKEN",
                    "bot_token_ref": "env:SLACK_BOT_TOKEN"
                },
                "io": {
                    "relay": {
                        "max_open_sessions": 0
                    }
                }
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
