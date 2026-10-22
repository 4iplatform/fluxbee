//! The configuration contract of `IO.blob`.
//!
//! # Why it exists
//!
//! io.blob was, like io.cloud, a packaged systemd node configured by environment variables that
//! took no part in the control plane. The operator's rule is: **everything set on a node goes
//! through CONFIG_SET/GET, and nothing through ENV**. This is that rule applied to io.blob, so it
//! is a first-class citizen like the other IO nodes (io.api/io.slack/io.wapp).
//!
//! # What is config and what is not
//!
//! io.blob is a CURATOR: it takes already-authorized commands from `SY.admin`, curates bytes into
//! `public/` and keeps a local ledger/refcount. Most of what comes from the environment today is
//! NOT operator config: the roots (`blob_root`, `public_root`, `ledger_path`) are **fixed system
//! design paths** (node defaults, with the dirs created at install), and `admin_hive` is
//! **derivable from the hive itself** (the local one by default). The only things an operator might
//! want to tune:
//!
//! - **`max_bytes`**: the maximum size of a publishable blob (a policy limit).
//! - **`admin_hive`**: an override of the `SY.admin` hive it trusts (the local one by default).
//!
//! **There is no secret field, on purpose:** io.blob mints and stores no tokens; access to a public
//! artifact is governed by the edge's row (the capability in the URL), not by this node.
//!
//! # Who writes it
//!
//! Normally **nobody by hand**: an io.blob without config is a VALID curator with defaults (not
//! `FAILED_CONFIG`). `CONFIG_SET` is the override when `max_bytes` has to go up or down, or to
//! point at another `admin_hive`.

use serde_json::{json, Value};

use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};

pub struct IoBlobAdapterConfigContract;

/// An override of the `SY.admin` hive (the local one by default). Rarely touched.
pub const FIELD_ADMIN_HIVE: &str = "config.io.admin_hive";
/// The maximum size of a publishable blob, in bytes.
pub const FIELD_MAX_BYTES: &str = "config.io.max_bytes";

impl IoAdapterConfigContract for IoBlobAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.blob"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        // None. An io.blob without config is a VALID curator with defaults (design roots, local
        // admin_hive, default max_bytes). Requiring anything would leave it in FAILED_CONFIG on
        // every clean start, adding nothing.
        &[]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[FIELD_ADMIN_HIVE, FIELD_MAX_BYTES]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "io.blob is a curator: it takes already-authorized commands from SY.admin, curates bytes \
             into public/, and keeps a local publication/refcount ledger.",
            "The blob roots and ledger path are FIXED system paths (node defaults, dirs created at \
             install), NOT operator config. admin_hive defaults to the local hive.",
            "There is deliberately NO secret field: io.blob mints and stores no tokens; public-artifact \
             access is governed by the edge allowlist row (the capability lives in the URL).",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let io = candidate.get("io");

        // `admin_hive`, when present, has to be a usable hive id. A blank value is a typo, not
        // "no override".
        let admin_hive = match io.and_then(|io| io.get("admin_hive")) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.admin_hive must not be blank; omit the field to use the local hive"
                            .to_string(),
                    ));
                }
                Some(trimmed.to_string())
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io.admin_hive must be a string".to_string(),
                ))
            }
        };

        // `max_bytes`, when present, has to be a positive integer. 0 is not "no limit": it is a
        // limit that rejects EVERYTHING, almost always a mistake. Omitting the field = the node's
        // default.
        let max_bytes = match io.and_then(|io| io.get("max_bytes")) {
            None | Some(Value::Null) => None,
            Some(value) => {
                let n = value.as_u64().ok_or_else(|| {
                    IoAdapterConfigError::InvalidConfig(
                        "io.max_bytes must be a non-negative integer".to_string(),
                    )
                })?;
                if n == 0 {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.max_bytes must be > 0 (0 would reject every publish); omit to use the default"
                            .to_string(),
                    ));
                }
                Some(n)
            }
        };

        let mut io_out = serde_json::Map::new();
        if let Some(admin_hive) = admin_hive {
            io_out.insert("admin_hive".to_string(), json!(admin_hive));
        }
        if let Some(max_bytes) = max_bytes {
            io_out.insert("max_bytes".to_string(), json!(max_bytes));
        }
        Ok(json!({ "io": Value::Object(io_out) }))
    }
}

/// The `SY.admin` hive it trusts, read from the effective config. `None` = use the local one.
pub fn configured_admin_hive(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("admin_hive"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// The configured blob size cap, read from the effective config. `None` = use the default.
pub fn configured_max_bytes(effective: Option<&Value>) -> Option<u64> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("max_bytes"))
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_curador_sin_config_es_valido_no_un_error() {
        let out = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({}))
            .expect("an io.blob without config is valid");
        assert!(configured_admin_hive(Some(&out)).is_none());
        assert!(configured_max_bytes(Some(&out)).is_none());
        assert!(IoBlobAdapterConfigContract.required_fields().is_empty());
    }

    #[test]
    fn acepta_max_bytes_y_admin_hive_y_los_expone() {
        let out = IoBlobAdapterConfigContract
            .validate_and_materialize(
                &json!({"io": {"max_bytes": 1048576, "admin_hive": " motherbee "}}),
            )
            .expect("valid config");
        assert_eq!(configured_max_bytes(Some(&out)), Some(1_048_576));
        assert_eq!(
            configured_admin_hive(Some(&out)).as_deref(),
            Some("motherbee"),
            "the whitespace is trimmed"
        );
    }

    #[test]
    fn max_bytes_cero_es_un_error_no_un_silencio() {
        let err = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"max_bytes": 0}}))
            .expect_err("a cap of 0 would reject EVERY publish");
        assert!(format!("{err}").contains("> 0"), "{err}");
    }

    #[test]
    fn admin_hive_en_blanco_es_un_error() {
        let err = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"admin_hive": "   "}}))
            .expect_err("a blank admin_hive has to fail");
        assert!(format!("{err}").contains("blank"), "{err}");
    }

    /// The contract accepts NO secrets: io.blob mints and stores no tokens.
    #[test]
    fn no_hay_campo_de_secreto_en_el_contrato() {
        let campos: Vec<&str> = IoBlobAdapterConfigContract
            .optional_fields()
            .iter()
            .chain(IoBlobAdapterConfigContract.required_fields())
            .copied()
            .collect();
        for campo in &campos {
            let bajo = campo.to_ascii_lowercase();
            assert!(
                !bajo.contains("secret") && !bajo.contains("token") && !bajo.contains("api_key"),
                "the io.blob contract must not have secret fields: {campo}"
            );
        }
        let out = IoBlobAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"max_bytes": 1024, "secret": "x"}}))
            .expect("valid");
        assert!(
            out.get("io").and_then(|io| io.get("secret")).is_none(),
            "a smuggled secret cannot survive materialization"
        );
    }
}
