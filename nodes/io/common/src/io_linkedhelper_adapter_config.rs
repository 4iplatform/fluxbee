use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};
use fluxbee_sdk::node_secret::NodeSecretDescriptor;
use serde_json::{Map, Value};

pub struct IoLinkedHelperAdapterConfigContract;

impl IoAdapterConfigContract for IoLinkedHelperAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.linkedhelper"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        &[
            "config.managed_instance_id",
            "config.tenant_id",
            "config.listen.address",
            "config.listen.port",
            "config.adapter",
            "config.adapter.adapter_id",
            "config.adapter.auth.type",
            "config.adapter.auth.resource_type",
            "config.adapter.auth.key",
        ]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "config.adapter.local_instance_id",
            "config.adapter.label",
            "config.adapter.dst_node",
            "config.http.max_request_bytes",
            "config.identity.target",
            "config.identity.timeout_ms",
            "config.mode",
            "config.node.*",
            "config.runtime.*",
        ]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "IO.linkedhelper intermediate mode is 1 node = 1 managed_instance_id = 1 adapter binding.",
            "Adapter authentication must use adapter.auth.type=vault_ref; inline installation keys are no longer accepted.",
            "The adapter secret is resolved from SY.vault at runtime using adapter.auth.key.",
            "Event payload schemas remain provisional in this phase; control-plane config focuses on direct node bootstrap.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let mut cfg = candidate.as_object().cloned().ok_or_else(|| {
            IoAdapterConfigError::InvalidConfig("config must be an object".to_string())
        })?;

        ensure_object_field(&mut cfg, "listen")?;
        ensure_object_field(&mut cfg, "adapter")?;
        ensure_optional_object_field(&mut cfg, "http")?;
        ensure_optional_object_field(&mut cfg, "identity")?;
        ensure_optional_object_field(&mut cfg, "node")?;
        ensure_optional_object_field(&mut cfg, "runtime")?;

        require_non_empty_string(&cfg, "managed_instance_id", "managed_instance_id")?;
        require_non_empty_string(&cfg, "tenant_id", "tenant_id")?;

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

        if let Some(mode) = cfg.get("mode") {
            let mode = mode.as_str().map(str::trim).filter(|value| !value.is_empty());
            if mode.is_none() {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "mode must be a non-empty string when present".to_string(),
                ));
            }
        } else {
            cfg.insert(
                "mode".to_string(),
                Value::String("direct_http_intermediate".to_string()),
            );
        }

        let adapter = cfg
            .get("adapter")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig("config.adapter must be an object".to_string())
            })?;
        require_non_empty_string(adapter, "adapter_id", "adapter.adapter_id")?;
        validate_optional_non_empty_string(adapter, "local_instance_id", "adapter.local_instance_id")?;
        validate_optional_non_empty_string(adapter, "label", "adapter.label")?;
        validate_optional_non_empty_string(adapter, "dst_node", "adapter.dst_node")?;

        let auth = adapter
            .get("auth")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                IoAdapterConfigError::InvalidConfig("adapter.auth must be an object".to_string())
            })?;
        let auth_type = require_non_empty_string(auth, "type", "adapter.auth.type")?;
        if auth_type != "vault_ref" {
            return Err(IoAdapterConfigError::InvalidConfig(
                "adapter.auth.type must be 'vault_ref'".to_string(),
            ));
        }
        let resource_type =
            require_non_empty_string(auth, "resource_type", "adapter.auth.resource_type")?;
        if resource_type != "linkedhelper_adapter" {
            return Err(IoAdapterConfigError::InvalidConfig(
                "adapter.auth.resource_type must be 'linkedhelper_adapter'".to_string(),
            ));
        }
        require_non_empty_string(auth, "key", "adapter.auth.key")?;

        Ok(Value::Object(cfg))
    }

    fn redact_effective_config(&self, effective: &Value) -> Value {
        effective.clone()
    }

    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        let Some(auth) = effective
            .and_then(|cfg| cfg.get("adapter"))
            .and_then(|adapter| adapter.get("auth"))
            .and_then(Value::as_object)
        else {
            return Vec::new();
        };

        let key = auth
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut descriptor = NodeSecretDescriptor::new(
            "config.adapter.auth.key",
            key.unwrap_or("linkedhelper/adapters/<adapter_id>"),
        );
        descriptor.required = true;
        descriptor.configured = key.is_some();
        descriptor.value_redacted = false;
        descriptor.persistence = "vault".to_string();
        vec![descriptor]
    }
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
