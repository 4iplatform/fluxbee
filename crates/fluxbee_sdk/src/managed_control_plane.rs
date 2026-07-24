//! Canonical control-plane for orchestrator-managed runtime nodes (AI.*, IO.*, and any future Rust
//! node kind).
//!
//! # Why this exists
//!
//! Node control-plane handling (config load at boot, CONFIG_SET/GET, the
//! Unconfigured/Configured/FAILED_CONFIG lifecycle, health reporting, logging init) was reimplemented
//! per node family. The copies drifted: `io-common` and the hand-rolled `ai.generic` runner both used
//! a **two-location** model — an authoritative "dynamic state" file under `/var/lib/fluxbee/state/*`
//! that boot preferred over the orchestrator-written `config.json`. Because that state file lives
//! OUTSIDE the node's instance dir, a `kill` + re-`run_node` left it behind and the respawned node
//! silently booted the STALE config, ignoring the fresh one the orchestrator just wrote. Only WF (Go)
//! used the correct **single-config** model and was immune.
//!
//! This module is the one canonical implementation, so a node kind can no longer diverge: it owns the
//! control-plane; a node plugs in only its config *contract* via [`ManagedNodeConfigContract`].
//!
//! # The single-config model
//!
//! `nodes/<KIND>/<node@hive>/config.json` (see [`crate::managed_node`]) is the ONE source of truth.
//! The orchestrator writes it on `run_node`/`set_node_config`; the node writes it on a direct mesh
//! `CONFIG_SET`. Both preserve the orchestrator-owned `_system` block. Consequences:
//! - **Restart** (same instance, no `run_node`): `config.json` is untouched, so the operator's last
//!   `CONFIG_SET` survives.
//! - **Respawn** (`kill` + `run_node`): `run_node` writes a fresh `config.json`, so the node resets —
//!   and the generic purge (`remove_dir_all` of the node dir) removes it. **No stale-config shadow.**

use crate::managed_node::{managed_node_config_path_with_root, ManagedNodeError};
use crate::node_secret::NodeSecretDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;

/// Default root under which the orchestrator writes managed node instance dirs.
pub const DEFAULT_MANAGED_NODES_ROOT: &str = "/var/lib/fluxbee/nodes";

/// Lifecycle state of a managed node's config plane. Distinct from the orchestrator's process-level
/// lifecycle (RUNNING/STOPPED): a process can be RUNNING while its config plane is FAILED_CONFIG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedNodeLifecycleState {
    #[serde(rename = "UNCONFIGURED")]
    Unconfigured,
    #[serde(rename = "CONFIGURED")]
    Configured,
    #[serde(rename = "FAILED_CONFIG")]
    FailedConfig,
}

impl ManagedNodeLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "UNCONFIGURED",
            Self::Configured => "CONFIGURED",
            Self::FailedConfig => "FAILED_CONFIG",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlPlaneErrorInfo {
    pub code: String,
    pub message: String,
}

/// In-memory control-plane state. `effective_config` is the node config with `_system` stripped and
/// the contract applied; it is `None` while Unconfigured or FailedConfig (so a rejected — possibly
/// secret-bearing — config is never surfaced), while `config_version`/`last_error` still identify
/// which config was rejected and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManagedControlPlaneState {
    pub current_state: ManagedNodeLifecycleState,
    pub schema_version: u32,
    pub config_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ControlPlaneErrorInfo>,
}

impl Default for ManagedControlPlaneState {
    fn default() -> Self {
        Self {
            current_state: ManagedNodeLifecycleState::Unconfigured,
            schema_version: 0,
            config_version: 0,
            effective_config: None,
            last_error: None,
        }
    }
}

/// Error returned by a node's config contract.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("invalid_config: {0}")]
    InvalidConfig(String),
    #[error("internal_error: {0}")]
    Internal(String),
}

impl ContractError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::Internal(_) => "internal_error",
        }
    }
}

/// The node-specific plug-in. The SDK owns the control-plane machinery; a node kind supplies only how
/// to validate + materialize ITS config, plus metadata for self-documentation and secret discovery.
/// Generalized from `io-common`'s `IoAdapterConfigContract` so IO and AI (and future kinds) share one
/// control-plane.
pub trait ManagedNodeConfigContract: Send + Sync {
    /// Node family, e.g. "IO", "AI".
    fn node_family(&self) -> &'static str;

    /// Fully-qualified node kind, e.g. "IO.api", "AI.chat".
    fn node_kind(&self) -> &'static str;

    /// Required config field paths (for self-documenting `CONFIG_GET`/autohelp).
    fn required_fields(&self) -> &'static [&'static str];

    fn optional_fields(&self) -> &'static [&'static str] {
        &[]
    }

    fn notes(&self) -> &'static [&'static str] {
        &[]
    }

    /// Validate the candidate effective config (already `_system`-stripped) and return its
    /// materialized form (defaults applied). Returning `Err` sends the node to FAILED_CONFIG — a
    /// config that is syntactically JSON but semantically invalid is a rejection, NOT "absent".
    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, ContractError>;

    /// Redact secret-bearing fields before surfacing the effective config in a `CONFIG_GET` reply.
    fn redact_effective_config(&self, effective: &Value) -> Value {
        effective.clone()
    }

    /// Secrets this node depends on (for `CONFIG_GET` contract discovery). Never returns values.
    fn secret_descriptors(&self, _effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        Vec::new()
    }
}

/// Self-describing contract payload for a `CONFIG_GET` reply / autohelp.
pub fn build_contract_payload(
    contract: &dyn ManagedNodeConfigContract,
    effective: Option<&Value>,
) -> Value {
    json!({
        "node_family": contract.node_family(),
        "node_kind": contract.node_kind(),
        "supports": ["CONFIG_GET", "CONFIG_SET"],
        "required_fields": contract.required_fields(),
        "optional_fields": contract.optional_fields(),
        "secrets": contract.secret_descriptors(effective),
        "notes": contract.notes(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ControlPlaneError {
    #[error("managed node path error: {0}")]
    ManagedNode(#[from] ManagedNodeError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Extract the effective config (contract-facing) from a node-dir `config.json` root. Accepts both a
/// flat document (`{_system, ...fields}`) and a wrapped one (`{_system, config: {...}}`); either way
/// the orchestrator-owned `_system` block is removed.
pub fn extract_effective_config(root: &Value) -> Value {
    let mut candidate = root.get("config").cloned().unwrap_or_else(|| root.clone());
    if let Some(obj) = candidate.as_object_mut() {
        obj.remove("_system");
    }
    candidate
}

fn config_version_from_root(root: &Value) -> u64 {
    root.get("_system")
        .and_then(|v| v.get("config_version"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

/// Boot the control plane from the single source of truth (`config.json`). No separate dynamic-state
/// file, so a stale one can never shadow a fresh `run_node`.
pub fn bootstrap_managed_control_plane(
    node_name: &str,
    contract: &dyn ManagedNodeConfigContract,
) -> Result<ManagedControlPlaneState, ControlPlaneError> {
    bootstrap_managed_control_plane_with_root(
        node_name,
        contract,
        Path::new(DEFAULT_MANAGED_NODES_ROOT),
    )
}

/// [`bootstrap_managed_control_plane`] with an explicit nodes root (for tests).
pub fn bootstrap_managed_control_plane_with_root(
    node_name: &str,
    contract: &dyn ManagedNodeConfigContract,
    nodes_root: &Path,
) -> Result<ManagedControlPlaneState, ControlPlaneError> {
    let path = managed_node_config_path_with_root(node_name, nodes_root)?;
    if !path.exists() {
        return Ok(ManagedControlPlaneState::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    // Non-JSON on disk is unrecoverable garbage, not a rejected-but-identifiable config: treat as
    // FAILED_CONFIG with no version rather than silently Unconfigured, so the operator sees it.
    let root: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            return Ok(failed_config(
                0,
                "invalid_config",
                format!("config.json is not valid JSON: {err}"),
            ));
        }
    };
    let config_version = config_version_from_root(&root);
    let effective = extract_effective_config(&root);
    if !effective.is_object() {
        return Ok(failed_config(
            config_version,
            "invalid_config",
            format!("config is not an object: {}", path.display()),
        ));
    }
    match contract.validate_and_materialize(&effective) {
        Ok(materialized) => Ok(ManagedControlPlaneState {
            current_state: ManagedNodeLifecycleState::Configured,
            schema_version: 1,
            config_version,
            effective_config: Some(materialized),
            last_error: None,
        }),
        Err(err) => Ok(failed_config(config_version, err.code(), err.to_string())),
    }
}

fn failed_config(
    config_version: u64,
    code: &str,
    message: impl Into<String>,
) -> ManagedControlPlaneState {
    ManagedControlPlaneState {
        current_state: ManagedNodeLifecycleState::FailedConfig,
        schema_version: 1,
        config_version,
        effective_config: None,
        last_error: Some(ControlPlaneErrorInfo {
            code: code.to_string(),
            message: message.into(),
        }),
    }
}

/// Persist a new effective config to the node-dir `config.json`, PRESERVING the orchestrator-owned
/// `_system` block (only bumping `config_version`). This is the node-side write for a direct mesh
/// `CONFIG_SET`; it writes the same file + shape the orchestrator owns, so the two converge.
pub fn persist_effective_config_with_root(
    node_name: &str,
    config_version: u64,
    effective: &Value,
    nodes_root: &Path,
) -> Result<(), ControlPlaneError> {
    let path = managed_node_config_path_with_root(node_name, nodes_root)?;
    // Preserve _system from the current file (orchestrator-owned); default to a minimal one.
    let mut system = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|root| root.get("_system").cloned())
        .unwrap_or_else(|| json!({}));
    if let Some(obj) = system.as_object_mut() {
        obj.insert("config_version".to_string(), json!(config_version));
    }
    // Write the flat document shape the orchestrator uses: {_system, ...effective fields}.
    let mut root = effective.clone();
    if let Some(obj) = root.as_object_mut() {
        obj.insert("_system".to_string(), system);
    } else {
        root = json!({ "_system": system, "config": effective });
    }
    write_json_atomic(&path, &serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn write_json_atomic(path: &Path, content: &str) -> Result<(), ControlPlaneError> {
    let parent = path.parent().ok_or_else(|| {
        ControlPlaneError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "config path has no parent directory",
        ))
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp: PathBuf = parent.join(format!(
        ".{}.tmp.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("config"),
        std::process::id()
    ));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `CONFIG_GET` reply payload.
pub fn build_config_get_response_payload(
    node_name: &str,
    state: &ManagedControlPlaneState,
    contract: &dyn ManagedNodeConfigContract,
) -> Value {
    let ok = state.effective_config.is_some();
    let redacted = state
        .effective_config
        .as_ref()
        .map(|cfg| contract.redact_effective_config(cfg));
    json!({
        "ok": ok,
        "node_name": node_name,
        "state": state.current_state.as_str(),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "effective_config": redacted,
        "contract": build_contract_payload(contract, state.effective_config.as_ref()),
        "error": if ok { Value::Null } else { json!({"code":"node_not_configured","message":"No effective config available"}) },
    })
}

pub fn build_config_set_ok_payload(
    node_name: &str,
    state: &ManagedControlPlaneState,
    contract: &dyn ManagedNodeConfigContract,
) -> Value {
    let redacted = state
        .effective_config
        .as_ref()
        .map(|cfg| contract.redact_effective_config(cfg));
    json!({
        "ok": true,
        "node_name": node_name,
        "state": state.current_state.as_str(),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "effective_config": redacted,
        "error": Value::Null,
    })
}

pub fn build_config_set_error_payload(
    node_name: &str,
    state: &ManagedControlPlaneState,
    code: &str,
    message: impl Into<String>,
) -> Value {
    json!({
        "ok": false,
        "node_name": node_name,
        "state": state.current_state.as_str(),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "effective_config": Value::Null,
        "error": {"code": code, "message": message.into()},
    })
}

/// Health state derived from the config-plane lifecycle: a node that failed to load its config, or
/// was never configured, refuses work and is NOT healthy. Fixes the class of bug where a node
/// reported HEALTHY while silently rejecting every request. DEGRADED is a soft signal (it does not,
/// by itself, drive an orchestrator restart — which would not fix a bad config anyway).
pub fn managed_health_state(state: &ManagedControlPlaneState) -> &'static str {
    match state.current_state {
        ManagedNodeLifecycleState::Configured => "HEALTHY",
        _ => "DEGRADED",
    }
}

/// Initialize tracing for a managed node so it actually logs to journald. Managed nodes are launched
/// by transient systemd units that do NOT set `RUST_LOG`, and `EnvFilter::from_default_env()` with an
/// unset `RUST_LOG` emits NOTHING — which turned every misconfiguration into a blind debug. Default to
/// INFO; `RUST_LOG` still overrides when set. Idempotent-safe to call once at startup.
pub fn init_managed_node_logging(default_directives: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(default_directives)),
        )
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FakeContract;
    impl ManagedNodeConfigContract for FakeContract {
        fn node_family(&self) -> &'static str {
            "AI"
        }
        fn node_kind(&self) -> &'static str {
            "AI.fake"
        }
        fn required_fields(&self) -> &'static [&'static str] {
            &["behavior.model"]
        }
        fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, ContractError> {
            let has_model = candidate
                .get("behavior")
                .and_then(|b| b.get("model"))
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .is_some();
            if !has_model {
                return Err(ContractError::InvalidConfig("missing behavior.model".into()));
            }
            let mut materialized = candidate.clone();
            if let Some(obj) = materialized.as_object_mut() {
                obj.insert("_materialized".to_string(), json!(true));
            }
            Ok(materialized)
        }
    }

    fn write_config(root: &Path, node: &str, body: &Value) {
        let path = managed_node_config_path_with_root(node, root).expect("path");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(body).unwrap()).unwrap();
    }

    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcp-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn boot_absent_config_is_unconfigured() {
        let root = temp_root("absent");
        let st =
            bootstrap_managed_control_plane_with_root("AI.fake@motherbee", &FakeContract, &root)
                .expect("boot");
        assert_eq!(st.current_state, ManagedNodeLifecycleState::Unconfigured);
        assert!(st.effective_config.is_none());
    }

    #[test]
    fn boot_valid_config_is_configured_and_materialized() {
        let root = temp_root("valid");
        write_config(
            &root,
            "AI.fake@motherbee",
            &json!({"_system":{"config_version":5},"behavior":{"model":"gpt-4o-mini"}}),
        );
        let st =
            bootstrap_managed_control_plane_with_root("AI.fake@motherbee", &FakeContract, &root)
                .expect("boot");
        assert_eq!(st.current_state, ManagedNodeLifecycleState::Configured);
        assert_eq!(st.config_version, 5);
        assert_eq!(
            st.effective_config.as_ref().unwrap().get("_materialized"),
            Some(&json!(true))
        );
        assert_eq!(managed_health_state(&st), "HEALTHY");
    }

    #[test]
    fn boot_rejected_config_is_failed_and_withholds_effective() {
        let root = temp_root("rejected");
        write_config(
            &root,
            "AI.fake@motherbee",
            &json!({"_system":{"config_version":9},"behavior":{"instructions":"hi"}}),
        );
        let st =
            bootstrap_managed_control_plane_with_root("AI.fake@motherbee", &FakeContract, &root)
                .expect("boot");
        assert_eq!(st.current_state, ManagedNodeLifecycleState::FailedConfig);
        assert_eq!(st.config_version, 9); // identifies which version was rejected
        assert!(st.effective_config.is_none()); // never surface a rejected (maybe secret-bearing) config
        assert_eq!(managed_health_state(&st), "DEGRADED");
    }

    #[test]
    fn persist_preserves_system_and_reboots_configured() {
        let root = temp_root("persist");
        write_config(
            &root,
            "AI.fake@motherbee",
            &json!({"_system":{"config_version":1,"ilk_id":"ilk:abc"},"behavior":{"model":"a"}}),
        );
        persist_effective_config_with_root(
            "AI.fake@motherbee",
            2,
            &json!({"behavior":{"model":"gpt-4o-mini"}}),
            &root,
        )
        .expect("persist");
        // _system preserved (ilk_id kept, version bumped) and the new config reboots Configured.
        let path = managed_node_config_path_with_root("AI.fake@motherbee", &root).unwrap();
        let root_val: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            root_val.get("_system").and_then(|s| s.get("ilk_id")),
            Some(&json!("ilk:abc"))
        );
        assert_eq!(
            root_val.get("_system").and_then(|s| s.get("config_version")),
            Some(&json!(2))
        );
        let st =
            bootstrap_managed_control_plane_with_root("AI.fake@motherbee", &FakeContract, &root)
                .expect("reboot");
        assert_eq!(st.current_state, ManagedNodeLifecycleState::Configured);
        assert_eq!(st.config_version, 2);
    }
}
