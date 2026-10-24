//! The configuration contract of `IO.web`.
//!
//! # Why it exists
//!
//! io.web is a managed IO node like io.blob and io.cloud: what is set on it goes through
//! CONFIG_SET/GET, never through ENV (docs/io-web-v2.md §6). In stage 1 it serves one static page
//! to the edge, and nothing about that is the operator's to choose:
//!
//! - **the port** is the shared constant `fluxbee_sdk::mesh::IO_WEB_PORT`, the one the edge dials;
//! - **the TLS material** is the motherbee's mesh leaf, at its fixed path;
//! - **who may connect** is the hive registry's connected ingress, never a list in a config.
//!
//! So the contract has no field yet. Stage 2 (sessions) brings the first ones, the key that signs
//! sessions among them, by `vault_ref` (docs/io-web-v2.md §11.3).
//!
//! # What a config may hold
//!
//! **Nothing under `io`.** A field there is refused, so an operator who sets one learns at once
//! that it does nothing, instead of reading it back as applied. The keys outside `io` are the
//! platform's (the orchestrator's `tenant_id`), and are not carried into the effective config, as
//! in io.blob and io.cloud.
//!
//! # Who writes it
//!
//! Nobody. The orchestrator's seed alone is io.web's whole config, so io.web boots CONFIGURED on
//! these defaults (its boot, `nodes/io/io-web/src/main.rs`).

use serde_json::{json, Value};

use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};

pub struct IoWebAdapterConfigContract;

impl IoAdapterConfigContract for IoWebAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.web"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        // None, and no optional one either: io.web serves with its defaults alone.
        &[]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "io.web has no configurable field in this stage: any io.* field is refused.",
            "Its port is the shared constant IO_WEB_PORT (9200), which the edge dials; its TLS \
             certificate is the motherbee's mesh leaf; it admits only a hive the registry lists as \
             a connected ingress. None of them is config.",
            "There is deliberately NO secret field yet: the sessions of stage 2 bring the first \
             one, by vault_ref.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        match candidate.get("io") {
            None | Some(Value::Null) => {}
            Some(Value::Object(io)) if io.is_empty() => {}
            Some(Value::Object(io)) => {
                let fields: Vec<String> = io.keys().map(|key| format!("io.{key}")).collect();
                return Err(IoAdapterConfigError::InvalidConfig(format!(
                    "io.web has no configurable field in this stage; refused: {}",
                    fields.join(", ")
                )));
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io must be an object".to_string(),
                ))
            }
        }
        Ok(json!({ "io": {} }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_without_fields_is_valid_and_materializes_the_defaults() {
        for candidate in [
            json!({}),
            json!({"io": {}}),
            json!({"io": null}),
            json!({"tenant_id": "tnt:00000000-0000-0000-0000-000000000001"}),
        ] {
            let out = IoWebAdapterConfigContract
                .validate_and_materialize(&candidate)
                .unwrap_or_else(|err| panic!("{candidate}: {err}"));
            assert_eq!(out, json!({"io": {}}), "{candidate}");
        }
        assert!(IoWebAdapterConfigContract.required_fields().is_empty());
        assert!(IoWebAdapterConfigContract.optional_fields().is_empty());
    }

    /// A field under `io` is refused, never dropped: io.web has none, and an operator who sets
    /// one must not read it back as applied.
    #[test]
    fn every_io_field_is_refused_by_name() {
        let err = IoWebAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"port": 9300, "secret": "x"}}))
            .expect_err("io.web has no field");
        let message = err.to_string();
        assert_eq!(err.code(), "invalid_config");
        assert!(
            message.contains("io.port") && message.contains("io.secret"),
            "{message}"
        );

        let err = IoWebAdapterConfigContract
            .validate_and_materialize(&json!({"io": "x"}))
            .expect_err("io must be an object");
        assert!(err.to_string().contains("object"), "{err}");
    }
}
