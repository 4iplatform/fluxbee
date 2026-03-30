use fluxbee_sdk::node_secret::NodeSecretDescriptor;
use serde_json::{json, Value};

pub const IO_NODE_FAMILY: &str = "IO";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IoAdapterConfigError {
    #[error("invalid_config: {0}")]
    InvalidConfig(String),
    #[error("internal_error: {0}")]
    Internal(String),
}

impl IoAdapterConfigError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::Internal(_) => "internal_error",
        }
    }
}

pub trait IoAdapterConfigContract: Send + Sync {
    fn node_kind(&self) -> &'static str;

    fn required_fields(&self) -> &'static [&'static str];

    fn optional_fields(&self) -> &'static [&'static str] {
        &[]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError>;

    fn redact_effective_config(&self, effective: &Value) -> Value {
        effective.clone()
    }

    fn secret_descriptors(&self, _effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        Vec::new()
    }
}

pub fn apply_adapter_config_replace(
    contract: &dyn IoAdapterConfigContract,
    candidate: &Value,
) -> Result<Value, IoAdapterConfigError> {
    if !candidate.is_object() {
        return Err(IoAdapterConfigError::InvalidConfig(
            "payload.config must be an object".to_string(),
        ));
    }
    contract.validate_and_materialize(candidate)
}

pub fn build_io_adapter_contract_payload(
    contract: &dyn IoAdapterConfigContract,
    effective: Option<&Value>,
) -> Value {
    json!({
        "node_family": IO_NODE_FAMILY,
        "node_kind": contract.node_kind(),
        "supports": ["CONFIG_GET", "CONFIG_SET"],
        "required_fields": contract.required_fields(),
        "optional_fields": contract.optional_fields(),
        "secrets": contract.secret_descriptors(effective),
        "notes": contract.notes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAdapter;

    impl IoAdapterConfigContract for FakeAdapter {
        fn node_kind(&self) -> &'static str {
            "IO.fake"
        }

        fn required_fields(&self) -> &'static [&'static str] {
            &["config.fake.required"]
        }

        fn optional_fields(&self) -> &'static [&'static str] {
            &["config.fake.optional"]
        }

        fn notes(&self) -> &'static [&'static str] {
            &["fake adapter for tests"]
        }

        fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
            let has_required = candidate
                .get("fake")
                .and_then(|v| v.get("required"))
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .is_some();
            if !has_required {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "missing fake.required".to_string(),
                ));
            }
            Ok(candidate.clone())
        }

        fn secret_descriptors(&self, _effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
            let mut descriptor = NodeSecretDescriptor::new(
                "config.secrets.fake.token",
                "fake_token",
            );
            descriptor.required = true;
            descriptor.configured = false;
            vec![descriptor]
        }
    }

    #[test]
    fn replace_rejects_non_object_payload() {
        let adapter = FakeAdapter;
        let err = apply_adapter_config_replace(&adapter, &Value::String("x".to_string()))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig("payload.config must be an object".to_string())
        );
    }

    #[test]
    fn replace_validates_with_adapter_schema() {
        let adapter = FakeAdapter;
        let err = apply_adapter_config_replace(&adapter, &json!({"fake":{}}))
            .expect_err("must fail");
        assert_eq!(
            err,
            IoAdapterConfigError::InvalidConfig("missing fake.required".to_string())
        );

        let out = apply_adapter_config_replace(&adapter, &json!({"fake":{"required":"ok"}}))
            .expect("must pass");
        assert_eq!(out, json!({"fake":{"required":"ok"}}));
    }

    #[test]
    fn contract_payload_exposes_common_surface() {
        let adapter = FakeAdapter;
        let payload = build_io_adapter_contract_payload(&adapter, None);
        assert_eq!(payload.get("node_family"), Some(&Value::String("IO".to_string())));
        assert_eq!(payload.get("node_kind"), Some(&Value::String("IO.fake".to_string())));
        assert_eq!(
            payload
                .get("supports")
                .and_then(Value::as_array)
                .map(|arr| arr.len()),
            Some(2)
        );
        assert_eq!(
            payload
                .get("secrets")
                .and_then(Value::as_array)
                .map(|arr| arr.len()),
            Some(1)
        );
    }
}
