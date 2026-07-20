use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use serde_json::{Map, Value};

pub const IO_API_INBOUND_FAMILY: &str = "io.api.inbound.v1";
pub const IO_API_CHANNEL_TYPE: &str = "api_channel";

pub struct IoApiAdapterConfigContract;

impl IoAdapterConfigContract for IoApiAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.api"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.edge.node",
            "config.io.api_channel_id",
            "config.io.dst_node",
            "config.ingress.subject_mode",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.edge.publish",
            "config.ingress.caller_identity.external_user_id",
            "config.ingress.caller_identity.display_name",
            "config.ingress.caller_identity.email",
            "config.io.relay.window_ms",
            "config.io.relay.max_open_sessions",
            "config.io.relay.max_fragments_per_session",
            "config.io.relay.max_bytes_per_session",
            "config.node.*",
            "config.runtime.*",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "IO.api has no HTTP listener; SY.edge is its only public HTTP frontier.",
            "The instance accepts only POST requests forwarded as io.api.inbound.v1 messages for its own ICH.",
            "The instance tenant comes from FLUXBEE_NODE_TENANT_ID injected by Orchestrator.",
            "SY.admin mints the public bearer during externalize; credentials are not part of node config.",
            "API keys, inline secrets, multipart attachments and outbound webhooks are not part of this contract.",
            "Request payloads cannot override io.dst_node.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        reject_legacy_top_level(&cfg)?;
        ensure_object_field(&mut cfg, "edge")?;
        ensure_object_field(&mut cfg, "io")?;
        ensure_object_field(&mut cfg, "ingress")?;
        ensure_optional_object_field(&cfg, "node")?;
        ensure_optional_object_field(&cfg, "runtime")?;

        let edge = cfg
            .get_mut("edge")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| IoAdapterConfigError::Internal("edge missing".to_string()))?;
        reject_fields(
            edge,
            &[
                "secret",
                "token",
                "token_ref",
                "auth_mode",
                "methods",
                "inbound_family",
                "listen",
                "upstream",
            ],
            "edge",
        )?;
        let edge_node = require_non_empty_string(edge, "node", "edge.node")?;
        if !edge_node.starts_with("SY.edge@") || edge_node.ends_with('@') {
            return Err(IoAdapterConfigError::InvalidConfig(
                "edge.node must be a fully-qualified SY.edge@<hive> name".to_string(),
            ));
        }
        match edge.get("publish") {
            None => {
                edge.insert("publish".to_string(), Value::Bool(true));
            }
            Some(Value::Bool(_)) => {}
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "edge.publish must be boolean when present".to_string(),
                ))
            }
        }

        let io = cfg
            .get_mut("io")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| IoAdapterConfigError::Internal("io missing".to_string()))?;
        let channel = require_non_empty_string(io, "api_channel_id", "io.api_channel_id")?;
        if channel.len() > 256 {
            return Err(IoAdapterConfigError::InvalidConfig(
                "io.api_channel_id must be at most 256 bytes".to_string(),
            ));
        }
        require_non_empty_string(io, "dst_node", "io.dst_node")?;
        ensure_optional_object_member(io, "relay", "io.relay")?;
        if let Some(relay) = io.get("relay").and_then(Value::as_object) {
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

        let ingress = cfg
            .get("ingress")
            .and_then(Value::as_object)
            .ok_or_else(|| IoAdapterConfigError::Internal("ingress missing".to_string()))?;
        reject_fields(
            ingress,
            &[
                "accepted_content_types",
                "max_request_bytes",
                "max_attachments_per_request",
                "max_attachment_size_bytes",
                "max_total_attachment_bytes",
                "allowed_mime_types",
            ],
            "ingress",
        )?;
        let subject_mode =
            require_non_empty_string(ingress, "subject_mode", "ingress.subject_mode")?;
        if !matches!(subject_mode, "explicit_subject" | "caller_is_subject") {
            return Err(IoAdapterConfigError::InvalidConfig(
                "ingress.subject_mode must be 'explicit_subject' or 'caller_is_subject'"
                    .to_string(),
            ));
        }
        match ingress.get("caller_identity") {
            Some(Value::Object(identity)) if subject_mode == "caller_is_subject" => {
                require_non_empty_string(
                    identity,
                    "external_user_id",
                    "ingress.caller_identity.external_user_id",
                )?;
                validate_optional_non_empty_string(
                    identity,
                    "display_name",
                    "ingress.caller_identity.display_name",
                )?;
                validate_optional_non_empty_string(
                    identity,
                    "email",
                    "ingress.caller_identity.email",
                )?;
            }
            None if subject_mode == "caller_is_subject" => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "ingress.caller_identity is required for caller_is_subject".to_string(),
                ))
            }
            Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "ingress.caller_identity must be an object when present".to_string(),
                ))
            }
            None => {}
        }

        Ok(Value::Object(cfg))
    }
}

fn reject_legacy_top_level(root: &Map<String, Value>) -> Result<(), IoAdapterConfigError> {
    for field in ["listen", "auth", "integrations", "blob"] {
        if root.contains_key(field) {
            return Err(IoAdapterConfigError::InvalidConfig(format!(
                "config.{field} belongs to the removed direct-HTTP IO.api contract"
            )));
        }
    }
    Ok(())
}

fn reject_fields(
    root: &Map<String, Value>,
    fields: &[&str],
    prefix: &str,
) -> Result<(), IoAdapterConfigError> {
    if let Some(field) = fields.iter().find(|field| root.contains_key(**field)) {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{prefix}.{field} is not accepted by the Edge-native IO.api contract"
        )));
    }
    Ok(())
}

fn ensure_object_field(
    root: &mut Map<String, Value>,
    field: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        root.insert(field.to_string(), Value::Object(Map::new()));
    }
    if root.get(field).and_then(Value::as_object).is_none() {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{field} must be an object"
        )));
    }
    Ok(())
}

fn ensure_optional_object_field(
    root: &Map<String, Value>,
    field: &str,
) -> Result<(), IoAdapterConfigError> {
    if root.contains_key(field) && root.get(field).and_then(Value::as_object).is_none() {
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
    if root.contains_key(field) && root.get(field).and_then(Value::as_object).is_none() {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{label} must be an object when present"
        )));
    }
    Ok(())
}

fn require_non_empty_string<'a>(
    root: &'a Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<&'a str, IoAdapterConfigError> {
    root.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| IoAdapterConfigError::InvalidConfig(format!("{label} is required")))
}

fn validate_optional_non_empty_string(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        return Ok(());
    }
    require_non_empty_string(root, field, label).map(|_| ())
}

fn validate_optional_non_negative_integer(
    root: &Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<(), IoAdapterConfigError> {
    if !root.contains_key(field) {
        return Ok(());
    }
    if root.get(field).and_then(Value::as_u64).is_none() {
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
    if !root
        .get(field)
        .and_then(Value::as_u64)
        .is_some_and(|value| value > 0)
    {
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

    fn valid_config() -> Value {
        json!({
            "edge": {
                "node": "SY.edge@ingress-1"
            },
            "io": {
                "api_channel_id": "acme-orders",
                "dst_node": "AI.orders@worker-1"
            },
            "ingress": {
                "subject_mode": "explicit_subject"
            }
        })
    }

    #[test]
    fn accepts_edge_native_single_instance_contract() {
        let out = IoApiAdapterConfigContract
            .validate_and_materialize(&valid_config())
            .expect("valid config");
        assert_eq!(out["edge"]["publish"], json!(true));
        assert_eq!(out["io"]["api_channel_id"], "acme-orders");
    }

    #[test]
    fn accepts_disabled_publication_and_caller_subject() {
        let mut cfg = valid_config();
        cfg["edge"]["publish"] = json!(false);
        cfg["ingress"] = json!({
            "subject_mode": "caller_is_subject",
            "caller_identity": {
                "external_user_id": "partner-service",
                "display_name": "Partner service"
            }
        });
        IoApiAdapterConfigContract
            .validate_and_materialize(&cfg)
            .expect("valid config");
    }

    #[test]
    fn rejects_direct_http_and_inline_auth_legacy() {
        for (field, value) in [
            ("listen", json!({"address":"127.0.0.1","port":8080})),
            ("auth", json!({"mode":"api_key"})),
            ("integrations", json!([])),
            ("blob", json!({"path":"/tmp/blob"})),
        ] {
            let mut cfg = valid_config();
            cfg[field] = value;
            let err = IoApiAdapterConfigContract
                .validate_and_materialize(&cfg)
                .expect_err("legacy field must fail");
            assert!(err.to_string().contains("removed direct-HTTP"));
        }
    }

    #[test]
    fn rejects_edge_credentials_and_protocol_overrides() {
        for (field, value) in [
            ("secret", json!("nope")),
            ("token", json!("nope")),
            ("token_ref", json!("vault://nope")),
            ("auth_mode", json!("public")),
            ("methods", json!(["GET"])),
            ("inbound_family", json!("user")),
        ] {
            let mut cfg = valid_config();
            cfg["edge"][field] = value;
            assert!(IoApiAdapterConfigContract
                .validate_and_materialize(&cfg)
                .is_err());
        }
    }

    #[test]
    fn caller_subject_requires_configured_identity() {
        let mut cfg = valid_config();
        cfg["ingress"] = json!({"subject_mode":"caller_is_subject"});
        let err = IoApiAdapterConfigContract
            .validate_and_materialize(&cfg)
            .expect_err("caller identity required");
        assert!(err.to_string().contains("caller_identity"));
    }

    #[test]
    fn rejects_removed_multipart_fields() {
        let mut cfg = valid_config();
        cfg["ingress"]["accepted_content_types"] = json!(["multipart/form-data"]);
        let err = IoApiAdapterConfigContract
            .validate_and_materialize(&cfg)
            .expect_err("multipart field must fail");
        assert!(err.to_string().contains("Edge-native"));
    }
}
