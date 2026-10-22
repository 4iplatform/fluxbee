//! The configuration contract of `IO.cloud`.
//!
//! # Why it exists
//!
//! io.cloud was the only IO node without a control plane: it was configured by environment
//! variables, and so it required the operator to make up a token and leave it in the process
//! environment or in a file on disk, while the system has a vault precisely for that.
//!
//! The operator's rule is: **everything set on a node goes through CONFIG_SET/GET, and nothing
//! happens without the admin**. This is that rule applied to io.cloud.
//!
//! # What is config and what is not
//!
//! Only one field is real config: **`edge_node`**, the edge io.cloud trusts for inbound traffic.
//! Everything else that comes from the environment today is either derivable from the hive itself
//! (`admin_hive`, `identity_hive`), or the channel's frozen identity (`channel_type`,
//! `channel_address`), or a fixed system design path.
//!
//! **There is no secret field, on purpose.** The endpoint token is minted by `SY.admin` and kept in
//! the vault; neither the operator nor this node chooses it or sees it. Accepting a secret here
//! would reopen the door that was just closed.
//!
//! # Who writes it
//!
//! Normally **nobody by hand**: `publish_cloud_endpoint` publishes the endpoint and, as part of
//! that same order, records the `edge_node` it just used. The operator can see it with
//! `CONFIG_GET` and change it with `CONFIG_SET` if needed, but the happy path does not ask for it.

use serde_json::{json, Value};

use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError};

pub struct IoCloudAdapterConfigContract;

/// The only field an operator chooses.
pub const FIELD_EDGE_NODE: &str = "config.io.edge_node";

impl IoAdapterConfigContract for IoCloudAdapterConfigContract {
    fn node_kind(&self) -> &'static str {
        "IO.cloud"
    }

    fn required_fields(&self) -> &'static [&'static str] {
        // None. An io.cloud without `edge_node` is a VALID node that has no public door yet: it
        // serves the mesh and trusts no edge. Requiring it would leave the node in FAILED_CONFIG
        // on every clean start, which is exactly the state that confuses the operator and adds
        // nothing.
        &[]
    }

    fn optional_fields(&self) -> &'static [&'static str] {
        &[FIELD_EDGE_NODE, "config.io.inbound_family"]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[
            "edge_node is the SY.edge this node trusts for INBOUND traffic; a message from any \
             other origin is dropped.",
            "It is normally set for you by the publish_cloud_endpoint admin action, which publishes \
             the endpoint and records the edge it used. CONFIG_SET is the manual override.",
            "There is deliberately NO secret field: the endpoint token is minted by SY.admin into \
             the vault, and the edge only ever receives a reference to it.",
        ]
    }

    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, IoAdapterConfigError> {
        let io = candidate.get("io");

        // `edge_node`, when present, has to be a usable L2 name. A blank value is not "no edge":
        // it is a typo that would leave the node silently dropping everything.
        let edge_node = match io.and_then(|io| io.get("edge_node")) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.edge_node must not be blank; omit the field to trust no edge yet"
                            .to_string(),
                    ));
                }
                if !trimmed.starts_with("SY.edge") {
                    return Err(IoAdapterConfigError::InvalidConfig(format!(
                        "io.edge_node must be an SY.edge L2 name (got '{trimmed}')"
                    )));
                }
                Some(trimmed.to_string())
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io.edge_node must be a string".to_string(),
                ))
            }
        };

        let inbound_family = match io.and_then(|io| io.get("inbound_family")) {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Err(IoAdapterConfigError::InvalidConfig(
                        "io.inbound_family must not be blank".to_string(),
                    ));
                }
                Some(trimmed.to_string())
            }
            Some(_) => {
                return Err(IoAdapterConfigError::InvalidConfig(
                    "io.inbound_family must be a string".to_string(),
                ))
            }
        };

        let mut io_out = serde_json::Map::new();
        if let Some(edge_node) = edge_node {
            io_out.insert("edge_node".to_string(), json!(edge_node));
        }
        if let Some(inbound_family) = inbound_family {
            io_out.insert("inbound_family".to_string(), json!(inbound_family));
        }
        Ok(json!({ "io": Value::Object(io_out) }))
    }
}

/// The edge this node trusts, read from the effective config. `None` = none yet.
pub fn trusted_edge_node(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("edge_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_nodo_sin_edge_es_valido_no_un_error() {
        // No public door yet: it serves the mesh and trusts nobody. Requiring the field would
        // leave it in FAILED_CONFIG on every clean start, adding nothing.
        let out = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({}))
            .expect("an io.cloud without an edge is valid");
        assert!(trusted_edge_node(Some(&out)).is_none());
        assert!(IoCloudAdapterConfigContract.required_fields().is_empty());
    }

    #[test]
    fn acepta_un_edge_valido_y_lo_expone() {
        let out = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": " SY.edge@ingress1 "}}))
            .expect("valid edge");
        assert_eq!(
            trusted_edge_node(Some(&out)).as_deref(),
            Some("SY.edge@ingress1"),
            "the whitespace is trimmed"
        );
    }

    #[test]
    fn un_edge_en_blanco_es_un_error_no_un_silencio() {
        // This is what separates "I have no door yet" from "I mistyped it". Without this check, a
        // typo would leave the node dropping ALL inbound traffic without saying why.
        let err = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": "   "}}))
            .expect_err("a blank edge has to fail");
        assert!(format!("{err}").contains("blank"), "{err}");
    }

    #[test]
    fn el_edge_tiene_que_ser_un_sy_edge() {
        let err = IoCloudAdapterConfigContract
            .validate_and_materialize(&json!({"io": {"edge_node": "IO.api@motherbee"}}))
            .expect_err("only an SY.edge can be the door");
        assert!(format!("{err}").contains("SY.edge"), "{err}");
    }

    /// The contract accepts NO secrets. Accepting them would reopen the door we closed: SY.admin
    /// mints the token into the vault and nobody else chooses it.
    #[test]
    fn no_hay_campo_de_secreto_en_el_contrato() {
        let campos: Vec<&str> = IoCloudAdapterConfigContract
            .optional_fields()
            .iter()
            .chain(IoCloudAdapterConfigContract.required_fields())
            .copied()
            .collect();
        for campo in &campos {
            let bajo = campo.to_ascii_lowercase();
            assert!(
                !bajo.contains("secret") && !bajo.contains("token") && !bajo.contains("api_key"),
                "the io.cloud contract must not have secret fields: {campo}"
            );
        }
        // And if someone sends one anyway, it is dropped on materialize (it does not propagate).
        let out = IoCloudAdapterConfigContract
            .validate_and_materialize(
                &json!({"io": {"edge_node": "SY.edge@ingress1", "secret": "x"}}),
            )
            .expect("valid");
        assert!(
            out.get("io").and_then(|io| io.get("secret")).is_none(),
            "a smuggled secret cannot survive materialization"
        );
    }
}
