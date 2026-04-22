use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::{NodeSecretDescriptor, NODE_SECRET_REDACTION_TOKEN};
use serde_json::{Map, Value};

pub struct IoApiAdapterConfigContract;

impl IoAdapterConfigContract for IoApiAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.api"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.listen.address",
            "config.listen.port",
            "config.auth.mode",
            "config.auth.api_keys",
            "config.ingress.subject_mode",
            "config.ingress.accepted_content_types",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.io.dst_node",
            "config.io.relay.window_ms",
            "config.io.relay.max_open_sessions",
            "config.io.relay.max_fragments_per_session",
            "config.io.relay.max_bytes_per_session",
            "config.auth.api_keys[].integration_id",
            "config.ingress.max_request_bytes",
            "config.ingress.max_attachments_per_request",
            "config.ingress.max_attachment_size_bytes",
            "config.ingress.max_total_attachment_bytes",
            "config.ingress.allowed_mime_types",
            "config.auth.api_keys[].caller_identity.external_user_id",
            "config.auth.api_keys[].caller_identity.display_name",
            "config.auth.api_keys[].caller_identity.email",
            "config.integrations[].integration_id",
            "config.integrations[].tenant_id",
            "config.integrations[].final_reply_required",
            "config.integrations[].webhook.enabled",
            "config.integrations[].webhook.url",
            "config.integrations[].webhook.secret",
            "config.integrations[].webhook.secret_ref",
            "config.integrations[].webhook.timeout_ms",
            "config.integrations[].webhook.max_retries",
            "config.integrations[].webhook.initial_backoff_ms",
            "config.integrations[].webhook.max_backoff_ms",
            "config.node.*",
            "config.runtime.*",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "API keys may arrive inline in CONFIG_SET or via token_ref.",
            "Inline API keys must be materialized to local secrets.json by IO.api runtime.",
            "CONFIG_GET and CONFIG_RESPONSE must redact secret-bearing fields.",
            "v1 supports auth.mode=api_key only.",
            "Each API key must declare a tenant_id; IO.api derives the effective tenant from the authenticated key.",
            "Each API key must declare an integration_id; IO.api resolves outbound webhook policy from that integration.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        ensure_object_field(&mut cfg, "listen")?;
        ensure_object_field(&mut cfg, "auth")?;
        ensure_object_field(&mut cfg, "ingress")?;
        ensure_optional_object_field(&mut cfg, "io")?;
        ensure_optional_object_field(&mut cfg, "node")?;
        ensure_optional_object_field(&mut cfg, "runtime")?;

        let listen = cfg
            .get("listen")
            .and_then(Value::as_object)
            .ok_or_else(|| IoAdapterConfigError::Internal("listen missing".to_string()))?;
        require_non_empty_string(listen, "address", "listen.address")?;
        validate_port(listen)?;

        let auth = cfg
            .get("auth")
            .and_then(Value::as_object)
            .ok_or_else(|| IoAdapterConfigError::Internal("auth missing".to_string()))?;
        let auth_mode = auth
            .get("mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig("auth.mode is required".to_string())
            })?;
        if auth_mode != "api_key" {
            return Err(IoAdapterConfigError::InvalidConfig(
                "auth.mode must be 'api_key' in v1".to_string(),
            ));
        }
        validate_api_keys(auth, cfg.get("ingress").and_then(Value::as_object))?;
        validate_integrations(auth, &cfg)?;

        let ingress = cfg
            .get_mut("ingress")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| IoAdapterConfigError::Internal("ingress missing".to_string()))?;
        let subject_mode = ingress
            .get("subject_mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig("ingress.subject_mode is required".to_string())
            })?;
        if !matches!(subject_mode, "explicit_subject" | "caller_is_subject") {
            return Err(IoAdapterConfigError::InvalidConfig(
                "ingress.subject_mode must be 'explicit_subject' or 'caller_is_subject'"
                    .to_string(),
            ));
        }
        validate_accepted_content_types(ingress)?;

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
        let Some(auth) = root.get_mut("auth").and_then(Value::as_object_mut) else {
            return redacted;
        };
        let Some(api_keys) = auth.get_mut("api_keys").and_then(Value::as_array_mut) else {
            return redacted;
        };
        for entry in api_keys {
            let Some(obj) = entry.as_object_mut() else {
                continue;
            };
            redact_secret_field(obj, "token");
        }
        if let Some(integrations) = root.get_mut("integrations").and_then(Value::as_array_mut) {
            for entry in integrations {
                let Some(obj) = entry.as_object_mut() else {
                    continue;
                };
                let Some(webhook) = obj.get_mut("webhook").and_then(Value::as_object_mut) else {
                    continue;
                };
                redact_secret_field(webhook, "secret");
            }
        }
        redacted
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let Some(api_keys) = effective
            .and_then(|cfg| cfg.get("auth"))
            .and_then(|auth| auth.get("api_keys"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };

        let mut descriptors: Vec<_> = api_keys
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|entry| {
                let key_id = entry.get("key_id").and_then(Value::as_str)?.trim();
                if key_id.is_empty() {
                    return None;
                }
                let mut descriptor = NodeSecretDescriptor::new(
                    format!("config.auth.api_keys[{key_id}].token"),
                    api_key_storage_key(key_id),
                );
                descriptor.required = true;
                descriptor.configured = has_non_empty_string(entry, "token")
                    || has_non_empty_string(entry, "token_ref");
                Some(descriptor)
            })
            .collect();

        if let Some(integrations) = effective
            .and_then(|cfg| cfg.get("integrations"))
            .and_then(Value::as_array)
        {
            descriptors.extend(
                integrations
                    .iter()
                    .filter_map(Value::as_object)
                    .filter_map(|entry| {
                        let integration_id =
                            entry.get("integration_id").and_then(Value::as_str)?.trim();
                        if integration_id.is_empty() {
                            return None;
                        }
                        let webhook = entry.get("webhook").and_then(Value::as_object)?;
                        let mut descriptor = NodeSecretDescriptor::new(
                            format!("config.integrations[{integration_id}].webhook.secret"),
                            webhook_secret_storage_key(integration_id),
                        );
                        descriptor.required = webhook
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        descriptor.configured = has_non_empty_string(webhook, "secret")
                            || has_non_empty_string(webhook, "secret_ref");
                        Some(descriptor)
                    }),
            );
        }

        descriptors
    }
}

pub fn api_key_storage_key(key_id: &str) -> String {
    format!("io_api_key__{}", sanitize_storage_key_fragment(key_id))
}

pub fn webhook_secret_storage_key(integration_id: &str) -> String {
    format!(
        "io_api_webhook__{}",
        sanitize_storage_key_fragment(integration_id)
    )
}

fn sanitize_storage_key_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
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

fn require_non_empty_string(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if has_non_empty_string(root, field) {
        return Ok(());
    }
    Err(IoAdapterConfigError::InvalidConfig(format!(
        "{label} is required"
    )))
}

fn validate_port(root: &Map<String, Value>) -> Result<(), IoAdapterConfigError> {
    let Some(port) = root.get("port").and_then(Value::as_u64) else {
        return Err(IoAdapterConfigError::InvalidConfig(
            "listen.port must be an integer between 1 and 65535".to_string(),
        ));
    };
    if (1..=65535).contains(&port) {
        return Ok(());
    }
    Err(IoAdapterConfigError::InvalidConfig(
        "listen.port must be an integer between 1 and 65535".to_string(),
    ))
}

fn validate_api_keys(
    auth: &Map<String, Value>,
    ingress: Option<&Map<String, Value>>,
) -> Result<(), IoAdapterConfigError> {
    let Some(entries) = auth.get("api_keys").and_then(Value::as_array) else {
        return Err(IoAdapterConfigError::InvalidConfig(
            "auth.api_keys must be a non-empty array".to_string(),
        ));
    };
    if entries.is_empty() {
        return Err(IoAdapterConfigError::InvalidConfig(
            "auth.api_keys must be a non-empty array".to_string(),
        ));
    }
    let subject_mode = ingress
        .and_then(|obj| obj.get("subject_mode"))
        .and_then(Value::as_str)
        .unwrap_or("");
    for entry in entries {
        let Some(entry_obj) = entry.as_object() else {
            return Err(IoAdapterConfigError::InvalidConfig(
                "auth.api_keys entries must be objects".to_string(),
            ));
        };
        require_non_empty_string(entry_obj, "key_id", "auth.api_keys[].key_id")?;
        require_non_empty_string(entry_obj, "tenant_id", "auth.api_keys[].tenant_id")?;
        require_non_empty_string(
            entry_obj,
            "integration_id",
            "auth.api_keys[].integration_id",
        )?;
        let has_secret = has_non_empty_string(entry_obj, "token")
            || has_non_empty_string(entry_obj, "token_ref");
        if !has_secret {
            return Err(IoAdapterConfigError::InvalidConfig(
                "auth.api_keys[] requires token or token_ref".to_string(),
            ));
        }
        if subject_mode == "caller_is_subject" {
            let caller_identity = entry_obj
                .get("caller_identity")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    IoAdapterConfigError::InvalidConfig(
                        "auth.api_keys[].caller_identity is required for caller_is_subject"
                            .to_string(),
                    )
                })?;
            require_non_empty_string(
                caller_identity,
                "external_user_id",
                "auth.api_keys[].caller_identity.external_user_id",
            )?;
        } else if entry_obj.contains_key("caller_identity")
            && entry_obj
                .get("caller_identity")
                .and_then(Value::as_object)
                .is_none()
        {
            return Err(IoAdapterConfigError::InvalidConfig(
                "auth.api_keys[].caller_identity must be an object when present".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_integrations(
    auth: &Map<String, Value>,
    root: &Map<String, Value>,
) -> Result<(), IoAdapterConfigError> {
    let mut integrations_by_id: Map<String, Value> = Map::new();
    if let Some(entries) = root.get("integrations") {
        let entries = entries.as_array().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("integrations must be an array when present".to_string())
        })?;
        for entry in entries {
            let entry_obj = entry.as_object().ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig(
                    "integrations entries must be objects".to_string(),
                )
            })?;
            require_non_empty_string(entry_obj, "integration_id", "integrations[].integration_id")?;
            require_non_empty_string(entry_obj, "tenant_id", "integrations[].tenant_id")?;
            if !matches!(entry_obj.get("final_reply_required"), Some(Value::Bool(_))) {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "integrations[].final_reply_required is required and must be boolean"
                        .to_string(),
                ));
            }
            let integration_id = entry_obj
                .get("integration_id")
                .and_then(Value::as_str)
                .expect("validated integration_id")
                .trim()
                .to_string();
            if integrations_by_id.contains_key(&integration_id) {
                return Err(IoAdapterConfigError::InvalidConfig(format!(
                    "duplicate integrations[].integration_id '{integration_id}'"
                )));
            }
            validate_webhook_block(entry_obj)?;
            integrations_by_id.insert(integration_id, Value::Object(entry_obj.clone()));
        }
    }

    let api_keys = auth
        .get("api_keys")
        .and_then(Value::as_array)
        .expect("validated api_keys");
    for entry in api_keys {
        let entry_obj = entry.as_object().expect("validated api key object");
        let integration_id = entry_obj
            .get("integration_id")
            .and_then(Value::as_str)
            .expect("validated integration_id")
            .trim();
        let tenant_id = entry_obj
            .get("tenant_id")
            .and_then(Value::as_str)
            .expect("validated tenant_id")
            .trim();
        let integration = integrations_by_id.get(integration_id).ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig(format!(
                "auth.api_keys[].integration_id '{integration_id}' does not exist in integrations[]"
            ))
        })?;
        let integration_obj = integration.as_object().expect("integration object");
        let integration_tenant = integration_obj
            .get("tenant_id")
            .and_then(Value::as_str)
            .expect("validated integration tenant")
            .trim();
        if integration_tenant != tenant_id {
            return Err(IoAdapterConfigError::InvalidConfig(format!(
                "integration '{integration_id}' tenant_id does not match auth.api_keys[].tenant_id"
            )));
        }
        let final_reply_required = integration_obj
            .get("final_reply_required")
            .and_then(Value::as_bool)
            .expect("validated final_reply_required");
        let webhook_enabled = integration_obj
            .get("webhook")
            .and_then(Value::as_object)
            .and_then(|webhook| webhook.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if final_reply_required && !webhook_enabled {
            return Err(IoAdapterConfigError::InvalidConfig(format!(
                "integration '{integration_id}' has final_reply_required=true but webhook.enabled=false"
            )));
        }
    }

    Ok(())
}

fn validate_webhook_block(entry_obj: &Map<String, Value>) -> Result<(), IoAdapterConfigError> {
    let final_reply_required = entry_obj
        .get("final_reply_required")
        .and_then(Value::as_bool)
        .expect("validated final_reply_required");

    let Some(webhook_value) = entry_obj.get("webhook") else {
        if final_reply_required {
            return Err(IoAdapterConfigError::InvalidConfig(
                "integrations[].webhook is required when final_reply_required=true".to_string(),
            ));
        }
        return Ok(());
    };
    let webhook = webhook_value.as_object().ok_or_else(|| {
        IoAdapterConfigError::InvalidConfig(
            "integrations[].webhook must be an object when present".to_string(),
        )
    })?;
    if !matches!(webhook.get("enabled"), Some(Value::Bool(_))) {
        return Err(IoAdapterConfigError::InvalidConfig(
            "integrations[].webhook.enabled is required and must be boolean".to_string(),
        ));
    }
    let enabled = webhook
        .get("enabled")
        .and_then(Value::as_bool)
        .expect("validated webhook enabled");
    if !enabled {
        return Ok(());
    }
    require_non_empty_string(webhook, "url", "integrations[].webhook.url")?;
    let url = webhook
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .expect("validated webhook url");
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(IoAdapterConfigError::InvalidConfig(
            "integrations[].webhook.url must be absolute".to_string(),
        ));
    }
    let has_secret = has_non_empty_string(webhook, "secret")
        || has_non_empty_string(webhook, "secret_ref");
    if !has_secret {
        return Err(IoAdapterConfigError::InvalidConfig(
            "integrations[].webhook requires secret or secret_ref".to_string(),
        ));
    }
    validate_optional_positive_integer(
        webhook,
        "timeout_ms",
        "integrations[].webhook.timeout_ms",
    )?;
    validate_optional_non_negative_integer(
        webhook,
        "max_retries",
        "integrations[].webhook.max_retries",
    )?;
    validate_optional_positive_integer(
        webhook,
        "initial_backoff_ms",
        "integrations[].webhook.initial_backoff_ms",
    )?;
    validate_optional_positive_integer(
        webhook,
        "max_backoff_ms",
        "integrations[].webhook.max_backoff_ms",
    )?;
    Ok(())
}

fn validate_accepted_content_types(
    ingress: &Map<String, Value>,
) -> Result<(), IoAdapterConfigError> {
    let Some(content_types) = ingress
        .get("accepted_content_types")
        .and_then(Value::as_array)
    else {
        return Err(IoAdapterConfigError::InvalidConfig(
            "ingress.accepted_content_types must be a non-empty array".to_string(),
        ));
    };
    if content_types.is_empty() {
        return Err(IoAdapterConfigError::InvalidConfig(
            "ingress.accepted_content_types must be a non-empty array".to_string(),
        ));
    }
    for content_type in content_types {
        let Some(value) = content_type.as_str().map(str::trim) else {
            return Err(IoAdapterConfigError::InvalidConfig(
                "ingress.accepted_content_types entries must be strings".to_string(),
            ));
        };
        if !matches!(value, "application/json" | "multipart/form-data") {
            return Err(IoAdapterConfigError::InvalidConfig(format!(
                "unsupported ingress.accepted_content_types entry '{value}'"
            )));
        }
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

fn has_non_empty_string(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn redact_secret_field(map: &mut Map<String, Value>, key: &str) {
    if map.get(key).and_then(Value::as_str).is_some() {
        map.insert(
            key.to_string(),
            Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_materialize_requires_minimum_shape() {
        let contract = IoApiAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({}))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig("listen.address is required".to_string())
        );
    }

    #[test]
    fn validate_materialize_accepts_explicit_subject_api_key_mode() {
        let contract = IoApiAdapterConfigContract;
        let out = contract
            .validate_and_materialize(&json!({
                "listen": { "address": "127.0.0.1", "port": 8080 },
                "auth": {
                    "mode": "api_key",
                    "api_keys": [
                        {
                            "key_id": "partner1",
                            "tenant_id": "tnt:partner1",
                            "integration_id": "int_partner1",
                            "token_ref": "env:PARTNER1_KEY"
                        }
                    ]
                },
                "ingress": {
                    "subject_mode": "explicit_subject",
                    "accepted_content_types": ["application/json", "multipart/form-data"]
                },
                "integrations": [
                    {
                        "integration_id": "int_partner1",
                        "tenant_id": "tnt:partner1",
                        "final_reply_required": false,
                        "webhook": { "enabled": false }
                    }
                ],
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
    fn validate_materialize_requires_tenant_id_per_api_key() {
        let contract = IoApiAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({
                "listen": { "address": "127.0.0.1", "port": 8080 },
                "auth": {
                    "mode": "api_key",
                    "api_keys": [
                        { "key_id": "partner1", "token_ref": "env:PARTNER1_KEY" }
                    ]
                },
                "ingress": {
                    "subject_mode": "explicit_subject",
                    "accepted_content_types": ["application/json"]
                }
            }))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig(
                "auth.api_keys[].tenant_id is required".to_string()
            )
        );
    }

    #[test]
    fn validate_materialize_requires_integration_id_per_api_key() {
        let contract = IoApiAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({
                "listen": { "address": "127.0.0.1", "port": 8080 },
                "auth": {
                    "mode": "api_key",
                    "api_keys": [
                        { "key_id": "partner1", "tenant_id": "tnt:partner1", "token_ref": "env:PARTNER1_KEY" }
                    ]
                },
                "ingress": {
                    "subject_mode": "explicit_subject",
                    "accepted_content_types": ["application/json"]
                },
                "integrations": [
                    {
                        "integration_id": "int_partner1",
                        "tenant_id": "tnt:partner1",
                        "final_reply_required": false,
                        "webhook": { "enabled": false }
                    }
                ]
            }))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig(
                "auth.api_keys[].integration_id is required".to_string()
            )
        );
    }

    #[test]
    fn validate_materialize_requires_matching_integration() {
        let contract = IoApiAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({
                "listen": { "address": "127.0.0.1", "port": 8080 },
                "auth": {
                    "mode": "api_key",
                    "api_keys": [
                        {
                            "key_id": "partner1",
                            "tenant_id": "tnt:partner1",
                            "integration_id": "int_missing",
                            "token_ref": "env:PARTNER1_KEY"
                        }
                    ]
                },
                "ingress": {
                    "subject_mode": "explicit_subject",
                    "accepted_content_types": ["application/json"]
                },
                "integrations": [
                    {
                        "integration_id": "int_partner1",
                        "tenant_id": "tnt:partner1",
                        "final_reply_required": false,
                        "webhook": { "enabled": false }
                    }
                ]
            }))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig(
                "auth.api_keys[].integration_id 'int_missing' does not exist in integrations[]".to_string()
            )
        );
    }

    #[test]
    fn validate_materialize_accepts_integrations_with_enabled_webhook() {
        let contract = IoApiAdapterConfigContract;
        contract
            .validate_and_materialize(&json!({
                "listen": { "address": "127.0.0.1", "port": 8080 },
                "auth": {
                    "mode": "api_key",
                    "api_keys": [
                        {
                            "key_id": "partner1",
                            "tenant_id": "tnt:partner1",
                            "integration_id": "int_partner1",
                            "token_ref": "env:PARTNER1_KEY"
                        }
                    ]
                },
                "ingress": {
                    "subject_mode": "explicit_subject",
                    "accepted_content_types": ["application/json"]
                },
                "integrations": [
                    {
                        "integration_id": "int_partner1",
                        "tenant_id": "tnt:partner1",
                        "final_reply_required": true,
                        "webhook": {
                            "enabled": true,
                            "url": "https://example.com/webhook",
                            "secret_ref": "local_file:io_api_webhook__partner1",
                            "timeout_ms": 5000,
                            "max_retries": 5,
                            "initial_backoff_ms": 1000,
                            "max_backoff_ms": 30000
                        }
                    }
                ]
            }))
            .expect("must pass");
    }

    #[test]
    fn validate_materialize_requires_caller_identity_for_caller_is_subject() {
        let contract = IoApiAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({
                "listen": { "address": "127.0.0.1", "port": 8080 },
                "auth": {
                    "mode": "api_key",
                    "api_keys": [
                        {
                            "key_id": "caller1",
                            "tenant_id": "tnt:caller1",
                            "integration_id": "int_caller1",
                            "token_ref": "env:CALLER1_KEY"
                        }
                    ]
                },
                "ingress": {
                    "subject_mode": "caller_is_subject",
                    "accepted_content_types": ["application/json"]
                },
                "integrations": [
                    {
                        "integration_id": "int_caller1",
                        "tenant_id": "tnt:caller1",
                        "final_reply_required": false,
                        "webhook": { "enabled": false }
                    }
                ]
            }))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig(
                "auth.api_keys[].caller_identity is required for caller_is_subject".to_string()
            )
        );
    }

    #[test]
    fn redact_effective_config_masks_inline_tokens() {
        let contract = IoApiAdapterConfigContract;
        let redacted = contract.redact_effective_config(&json!({
            "auth": {
                "api_keys": [
                    { "key_id": "partner1", "token": "secret-1" }
                ]
            }
        }));
        assert_eq!(
            redacted
                .get("auth")
                .and_then(|auth| auth.get("api_keys"))
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_object)
                .and_then(|entry| entry.get("token"))
                .and_then(Value::as_str),
            Some(NODE_SECRET_REDACTION_TOKEN)
        );
    }

    #[test]
    fn secret_descriptors_track_configured_keys() {
        let contract = IoApiAdapterConfigContract;
        let descriptors = contract.secret_descriptors(Some(&json!({
            "auth": {
                "api_keys": [
                    { "key_id": "partner1", "token_ref": "local_file:io_api_key__partner1" },
                    { "key_id": "partner2", "token": "***REDACTED***" }
                ]
            }
        })));
        assert_eq!(descriptors.len(), 2);
        assert!(descriptors.iter().all(|item| item.configured));
        assert_eq!(descriptors[0].storage_key, "io_api_key__partner1");
    }
}
