use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::NodeSecretDescriptor;
use serde_json::{Map, Value};

/// Config contract for IO.wapp (WhatsApp Cloud API). Mirrors io.slack's family vault_ref pattern:
/// credentials are NEVER in config — config carries only a vault-key REFERENCE (`wapp.auth.key`),
/// resolved from SY.vault at runtime and validated `metadata.resource_type == "whatsapp"`. The secret
/// at that key is an object `{access_token, app_secret, verify_token}`. See docs/io-wapp-design.md.
pub struct IoWappAdapterConfigContract;

impl IoAdapterConfigContract for IoWappAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.wapp"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.wapp.auth.type",
            "config.wapp.auth.resource_type",
            "config.wapp.auth.key",
            "config.io.phone_number_id",
            "config.io.waba_id",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.io.dst_node",
            "config.io.graph_api_version",
            "config.io.blob.*",
            "config.identity.target",
            "config.identity.timeout_ms",
            "config.node.*",
            "config.runtime.*",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "WhatsApp credentials use wapp.auth.type=vault_ref; inline tokens are not accepted. The \
             secret (an object {access_token, app_secret, verify_token}) is resolved from SY.vault at \
             runtime using wapp.auth.key, validating metadata.resource_type == whatsapp.",
            "MVP apply mode is replace only.",
            "phone_number_id + waba_id identify the WhatsApp Business number/account this instance \
             sends from and owns the webhook for; they are the stable binding (not secrets).",
            "dst_node is where inbound messages relay (absent => router resolve), like io.slack.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        ensure_object_field(&mut cfg, "wapp")?;
        ensure_object_field(&mut cfg, "io")?;
        ensure_optional_object_field(&mut cfg, "identity")?;
        ensure_optional_object_field(&mut cfg, "node")?;
        ensure_optional_object_field(&mut cfg, "runtime")?;

        {
            let wapp = cfg.get("wapp").and_then(Value::as_object).ok_or_else(|| {
                IoAdapterConfigError::Internal("wapp object missing after normalization".to_string())
            })?;
            let auth = wapp.get("auth").and_then(Value::as_object).ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig(
                    "wapp.auth is required (a vault reference: {type:\"vault_ref\", \
                     resource_type:\"whatsapp\", key:\"<vault key>\"})"
                        .to_string(),
                )
            })?;
            let auth_type = auth.get("type").and_then(Value::as_str).map(str::trim);
            if auth_type != Some("vault_ref") {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "wapp.auth.type must be \"vault_ref\"".to_string(),
                ));
            }
            require_non_empty_string(auth, "resource_type", "wapp.auth.resource_type")?;
            require_non_empty_string(auth, "key", "wapp.auth.key")?;
        }

        let io_obj = cfg
            .get_mut("io")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| IoAdapterConfigError::Internal("io missing".to_string()))?;
        require_non_empty_string(io_obj, "phone_number_id", "io.phone_number_id")?;
        require_non_empty_string(io_obj, "waba_id", "io.waba_id")?;
        // NO dst_node default: absence means "let the router resolve" (None -> Destination::Resolve),
        // like io.api / io.slack. A literal "resolve" string is a bogus unicast target, so it is never
        // injected. graph_api_version, when present, must be a non-empty string.
        if io_obj.contains_key("graph_api_version") {
            require_non_empty_string(io_obj, "graph_api_version", "io.graph_api_version")?;
        }

        Ok(Value::Object(cfg))
    }

    fn redact_effective_config(&self, effective: &Value) -> Value {
        // Credentials never live in config — wapp.auth.key is a vault REFERENCE, not a secret
        // (mirrors io.slack value_redacted=false). Nothing to redact.
        effective.clone()
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let key_configured = effective
            .and_then(|v| v.get("wapp"))
            .and_then(|s| s.get("auth"))
            .and_then(Value::as_object)
            .map(|auth| has_non_empty_string(auth, "key"))
            .unwrap_or(false);

        let mut cred = NodeSecretDescriptor::new("config.wapp.auth.key", "whatsapp_credentials");
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_auth() -> Value {
        json!({"type":"vault_ref","resource_type":"whatsapp","key":"wapp/IO.wapp@motherbee"})
    }

    #[test]
    fn validate_materialize_requires_vault_ref_auth_and_binding() {
        let contract = IoWappAdapterConfigContract;
        // missing auth
        let err = contract
            .validate_and_materialize(&json!({"wapp":{}, "io":{"phone_number_id":"1","waba_id":"2"}}))
            .expect_err("must fail missing auth");
        assert!(matches!(err, IoAdapterConfigError::InvalidConfig(m) if m.contains("wapp.auth is required")));

        // wrong auth type
        let err = contract
            .validate_and_materialize(&json!({
                "wapp":{"auth":{"type":"inline","resource_type":"whatsapp","key":"k"}},
                "io":{"phone_number_id":"1","waba_id":"2"}
            }))
            .expect_err("must reject non vault_ref");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig("wapp.auth.type must be \"vault_ref\"".to_string())
        );

        // missing binding
        let err = contract
            .validate_and_materialize(&json!({"wapp":{"auth": valid_auth()}, "io":{"waba_id":"2"}}))
            .expect_err("must require phone_number_id");
        assert!(matches!(err, IoAdapterConfigError::InvalidConfig(m) if m.contains("io.phone_number_id")));

        // valid, and NO dst_node is injected
        let out = contract
            .validate_and_materialize(&json!({
                "wapp":{"auth": valid_auth()},
                "io":{"phone_number_id":"15550001111","waba_id":"9876543210"}
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
        let contract = IoWappAdapterConfigContract;
        let cfg = json!({"wapp":{"auth": valid_auth()}, "io":{"phone_number_id":"1"}});
        assert_eq!(contract.redact_effective_config(&cfg), cfg);
    }

    #[test]
    fn secret_descriptor_is_vault_key_ref() {
        let contract = IoWappAdapterConfigContract;
        let d = contract.secret_descriptors(Some(&json!({"wapp":{"auth": valid_auth()}})));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].field, "config.wapp.auth.key");
        assert!(d[0].configured);
        assert!(!d[0].value_redacted);
        assert_eq!(d[0].persistence, "vault");
    }

    #[test]
    fn validate_materialize_rejects_empty_graph_api_version() {
        let contract = IoWappAdapterConfigContract;
        let err = contract
            .validate_and_materialize(&json!({
                "wapp":{"auth": valid_auth()},
                "io":{"phone_number_id":"1","waba_id":"2","graph_api_version":"  "}
            }))
            .expect_err("must reject blank graph_api_version");
        assert!(matches!(err, IoAdapterConfigError::InvalidConfig(m) if m.contains("io.graph_api_version")));
    }
}
