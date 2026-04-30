use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::{NodeSecretDescriptor, NODE_SECRET_REDACTION_TOKEN};
use serde_json::{Map, Value};

pub struct IoLinkedHelperAdapterConfigContract;

impl IoAdapterConfigContract for IoLinkedHelperAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.linkedhelper"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.listen.address",
            "config.listen.port",
            "config.adapters",
            "config.adapters[].adapter_id",
            "config.adapters[].tenant_id",
            "config.adapters[].installation_key",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.adapters[].label",
            "config.adapters[].dst_node",
            "config.http.max_request_bytes",
            "config.identity.target",
            "config.identity.timeout_ms",
            "config.node.*",
            "config.runtime.*",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "IO.linkedhelper v1 accepts adapter polls over HTTP/HTTPS.",
            "Each adapter must provide a unique adapter_id and installation_key.",
            "Inline installation keys are materialized in local node config and redacted in CONFIG_GET.",
            "Event payload schemas remain provisional in this phase; control-plane config focuses on channel bootstrap.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        ensure_object_field(&mut cfg, "listen")?;
        ensure_optional_object_field(&mut cfg, "http")?;
        ensure_optional_object_field(&mut cfg, "identity")?;
        ensure_optional_object_field(&mut cfg, "node")?;
        ensure_optional_object_field(&mut cfg, "runtime")?;

        let listen = cfg
            .get("listen")
            .and_then(Value::as_object)
            .ok_or_else(|| IoAdapterConfigError::Internal("listen missing".to_string()))?;
        require_non_empty_string(listen, "address", "listen.address")?;
        validate_port(listen)?;

        if let Some(http) = cfg.get("http").and_then(Value::as_object) {
            validate_optional_positive_integer(http, "max_request_bytes", "http.max_request_bytes")?;
        }

        if let Some(identity) = cfg.get("identity").and_then(Value::as_object) {
            validate_optional_non_empty_string(identity, "target", "identity.target")?;
            validate_optional_positive_integer(identity, "timeout_ms", "identity.timeout_ms")?;
        }

        let adapters = cfg
            .get("adapters")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig("config.adapters must be an array".to_string())
            })?;
        if adapters.is_empty() {
            return Err(IoAdapterConfigError::InvalidConfig(
                "config.adapters must not be empty".to_string(),
            ));
        }

        let mut seen_ids = std::collections::HashSet::new();
        for (idx, adapter) in adapters.iter().enumerate() {
            let adapter_obj = adapter.as_object().ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig(format!(
                    "config.adapters[{idx}] must be an object"
                ))
            })?;
            let adapter_id =
                require_non_empty_string(adapter_obj, "adapter_id", "adapters[].adapter_id")?;
            if !seen_ids.insert(adapter_id.clone()) {
                return Err(IoAdapterConfigError::InvalidConfig(format!(
                    "duplicate adapter_id '{adapter_id}' in config.adapters"
                )));
            }
            require_non_empty_string(adapter_obj, "tenant_id", "adapters[].tenant_id")?;
            require_non_empty_string(
                adapter_obj,
                "installation_key",
                "adapters[].installation_key",
            )?;
            validate_optional_non_empty_string(adapter_obj, "label", "adapters[].label")?;
            validate_optional_non_empty_string(adapter_obj, "dst_node", "adapters[].dst_node")?;
        }

        Ok(Value::Object(cfg))
    }

    fn redact_effective_config(&self, effective: &Value) -> Value {
        let mut redacted = effective.clone();
        let Some(root) = redacted.as_object_mut() else {
            return redacted;
        };
        let Some(adapters) = root.get_mut("adapters").and_then(Value::as_array_mut) else {
            return redacted;
        };
        for adapter in adapters {
            let Some(adapter_obj) = adapter.as_object_mut() else {
                continue;
            };
            redact_secret_field(adapter_obj, "installation_key");
        }
        redacted
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let Some(adapters) = effective
            .and_then(|cfg| cfg.get("adapters"))
            .and_then(Value::as_array)
        else {
            return Vec::new();
        };

        adapters
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|adapter| {
                let adapter_id = adapter.get("adapter_id").and_then(Value::as_str)?.trim();
                if adapter_id.is_empty() {
                    return None;
                }
                let mut descriptor = NodeSecretDescriptor::new(
                    format!("config.adapters[{adapter_id}].installation_key"),
                    installation_key_storage_key(adapter_id),
                );
                descriptor.required = true;
                descriptor.configured = has_non_empty_string(adapter, "installation_key");
                Some(descriptor)
            })
            .collect()
    }
}

pub fn installation_key_storage_key(adapter_id: &str) -> String {
    format!("io_linkedhelper_installation_key__{}", sanitize_storage_key_fragment(adapter_id))
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
    cfg: &mut Map<String, Value>,
    field: &str,
) -> Result<(), IoAdapterConfigError> {
    match cfg.get(field) {
        Some(Value::Object(_)) => Ok(()),
        Some(_) => Err(IoAdapterConfigError::InvalidConfig(format!(
            "{field} must be an object"
        ))),
        None => Err(IoAdapterConfigError::InvalidConfig(format!(
            "{field} is required"
        ))),
    }
}

fn ensure_optional_object_field(
    cfg: &mut Map<String, Value>,
    field: &str,
) -> Result<(), IoAdapterConfigError> {
    match cfg.get(field) {
        Some(Value::Object(_)) | None => Ok(()),
        Some(_) => Err(IoAdapterConfigError::InvalidConfig(format!(
            "{field} must be an object when present"
        ))),
    }
}

fn require_non_empty_string(
    obj: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<String, IoAdapterConfigError> {
    obj.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| IoAdapterConfigError::InvalidConfig(format!("{path} is required")))
}

fn validate_optional_non_empty_string(
    obj: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<(), IoAdapterConfigError> {
    if let Some(value) = obj.get(field) {
        let value = value.as_str().map(str::trim).filter(|v| !v.is_empty());
        if value.is_none() {
            return Err(IoAdapterConfigError::InvalidConfig(format!(
                "{path} must be a non-empty string when present"
            )));
        }
    }
    Ok(())
}

fn validate_port(obj: &Map<String, Value>) -> Result<(), IoAdapterConfigError> {
    let port = obj.get("port").and_then(Value::as_u64).ok_or_else(|| {
        IoAdapterConfigError::InvalidConfig("listen.port is required".to_string())
    })?;
    if !(1..=65535).contains(&port) {
        return Err(IoAdapterConfigError::InvalidConfig(
            "listen.port must be between 1 and 65535".to_string(),
        ));
    }
    Ok(())
}

fn validate_optional_positive_integer(
    obj: &Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<(), IoAdapterConfigError> {
    if let Some(value) = obj.get(field).and_then(Value::as_u64) {
        if value == 0 {
            return Err(IoAdapterConfigError::InvalidConfig(format!(
                "{path} must be > 0 when present"
            )));
        }
    } else if obj.contains_key(field) {
        return Err(IoAdapterConfigError::InvalidConfig(format!(
            "{path} must be a positive integer when present"
        )));
    }
    Ok(())
}

fn redact_secret_field(obj: &mut Map<String, Value>, field: &str) {
    if let Some(value) = obj.get_mut(field) {
        if value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        {
            *value = Value::String(NODE_SECRET_REDACTION_TOKEN.to_string());
        }
    }
}

fn has_non_empty_string(obj: &Map<String, Value>, field: &str) -> bool {
    obj.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
}
