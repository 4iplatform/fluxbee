//! IO control-plane bootstrap — now a thin bridge over the SDK's canonical single-config control
//! plane (`fluxbee_sdk::managed_control_plane`). The old two-location model (a dynamic-state file
//! under `/var/lib/fluxbee/state/io-nodes/` that boot PREFERRED over the orchestrator-written
//! `config.json`) is gone: because that file lived outside the node instance dir, a kill + re-run_node
//! left it behind and the respawn booted the STALE config (BUG-4, same class ai.generic had). Now the
//! node-dir `config.json` is the ONLY source and the IO adapter plugs in via `IoAdapterConfigContract`.

use crate::io_adapter_config::{IoAdapterConfigContract, IoAdapterConfigError, IO_NODE_FAMILY};
use crate::io_control_plane::{
    IoConfigSource, IoControlPlaneErrorInfo, IoControlPlaneState, IoNodeLifecycleState,
};
use fluxbee_sdk::managed_control_plane::{
    bootstrap_managed_control_plane_with_root, ContractError, ManagedControlPlaneState,
    ManagedNodeConfigContract, ManagedNodeLifecycleState, DEFAULT_MANAGED_NODES_ROOT,
};
use fluxbee_sdk::node_secret::NodeSecretDescriptor;
use fluxbee_sdk::ManagedNodeError;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum IoControlPlaneBootstrapError {
    #[error("control-plane error: {0}")]
    ControlPlane(String),
    #[error("managed node path error: {0}")]
    ManagedNode(#[from] ManagedNodeError),
}

/// Bridges an IO adapter's `IoAdapterConfigContract` to the SDK's generic `ManagedNodeConfigContract`
/// so all IO adapters ride the one canonical control-plane. The IO family is fixed ("IO").
struct IoContractBridge<'a>(&'a dyn IoAdapterConfigContract);

impl ManagedNodeConfigContract for IoContractBridge<'_> {
    fn node_family(&self) -> &'static str {
        IO_NODE_FAMILY
    }
    fn node_kind(&self) -> &'static str {
        self.0.node_kind()
    }
    fn required_fields(&self) -> &'static [&'static str] {
        self.0.required_fields()
    }
    fn optional_fields(&self) -> &'static [&'static str] {
        self.0.optional_fields()
    }
    fn notes(&self) -> &'static [&'static str] {
        self.0.notes()
    }
    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, ContractError> {
        self.0.validate_and_materialize(candidate).map_err(|e| match e {
            IoAdapterConfigError::InvalidConfig(m) => ContractError::InvalidConfig(m),
            IoAdapterConfigError::Internal(m) => ContractError::Internal(m),
        })
    }
    fn redact_effective_config(&self, effective: &Value) -> Value {
        self.0.redact_effective_config(effective)
    }
    fn secret_descriptors(&self, effective: Option<&Value>) -> Vec<NodeSecretDescriptor> {
        self.0.secret_descriptors(effective)
    }
}

fn io_state_from_managed(cp: ManagedControlPlaneState) -> IoControlPlaneState {
    let current_state = match cp.current_state {
        ManagedNodeLifecycleState::Unconfigured => IoNodeLifecycleState::Unconfigured,
        ManagedNodeLifecycleState::Configured => IoNodeLifecycleState::Configured,
        ManagedNodeLifecycleState::FailedConfig => IoNodeLifecycleState::FailedConfig,
    };
    // Single-config model: there is exactly one source, the node-dir config.json.
    let config_source = match cp.current_state {
        ManagedNodeLifecycleState::Unconfigured => IoConfigSource::None,
        _ => IoConfigSource::OrchestratorFallback,
    };
    IoControlPlaneState {
        current_state,
        config_source,
        schema_version: cp.schema_version,
        config_version: cp.config_version,
        effective_config: cp.effective_config,
        last_error: cp.last_error.map(|e| IoControlPlaneErrorInfo {
            code: e.code,
            message: e.message,
        }),
    }
}

/// Boot the IO node control plane from the single source of truth (`config.json`) via the SDK.
pub fn bootstrap_io_control_plane_state(
    node_name: &str,
    contract: &dyn IoAdapterConfigContract,
) -> Result<IoControlPlaneState, IoControlPlaneBootstrapError> {
    bootstrap_io_control_plane_state_with_root(
        node_name,
        contract,
        Path::new(DEFAULT_MANAGED_NODES_ROOT),
    )
}

/// [`bootstrap_io_control_plane_state`] with an explicit nodes root (for tests).
pub fn bootstrap_io_control_plane_state_with_root(
    node_name: &str,
    contract: &dyn IoAdapterConfigContract,
    nodes_root: &Path,
) -> Result<IoControlPlaneState, IoControlPlaneBootstrapError> {
    let bridge = IoContractBridge(contract);
    let cp = bootstrap_managed_control_plane_with_root(node_name, &bridge, nodes_root)
        .map_err(|e| IoControlPlaneBootstrapError::ControlPlane(e.to_string()))?;
    Ok(io_state_from_managed(cp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::managed_node_config_path_with_root;
    use serde_json::json;
    use std::path::PathBuf;

    struct FakeAdapter;
    impl IoAdapterConfigContract for FakeAdapter {
        fn node_kind(&self) -> &'static str {
            "IO.fake"
        }
        fn required_fields(&self) -> &'static [&'static str] {
            &["io.dst_node"]
        }
        fn validate_and_materialize(&self, c: &Value) -> Result<Value, IoAdapterConfigError> {
            if c.get("io").and_then(|v| v.get("dst_node")).is_some() {
                Ok(c.clone())
            } else {
                Err(IoAdapterConfigError::InvalidConfig("missing io.dst_node".into()))
            }
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("io-cp-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_config(root: &Path, node: &str, body: &Value) {
        let p = managed_node_config_path_with_root(node, root).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string_pretty(body).unwrap()).unwrap();
    }

    #[test]
    fn boot_reads_only_node_dir_config_no_dynamic_shadow() {
        let root = temp_root("single");
        write_config(
            &root,
            "IO.fake@motherbee",
            &json!({"_system":{"config_version":3},"io":{"dst_node":"AI.x@motherbee"}}),
        );
        let st = bootstrap_io_control_plane_state_with_root("IO.fake@motherbee", &FakeAdapter, &root)
            .expect("boot");
        assert_eq!(st.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(st.config_version, 3);
        assert!(st.effective_config.is_some());
    }

    #[test]
    fn boot_absent_config_is_unconfigured() {
        let root = temp_root("absent");
        let st = bootstrap_io_control_plane_state_with_root("IO.fake@motherbee", &FakeAdapter, &root)
            .expect("boot");
        assert_eq!(st.current_state, IoNodeLifecycleState::Unconfigured);
    }

    #[test]
    fn boot_rejected_config_is_failed_config() {
        let root = temp_root("rejected");
        write_config(
            &root,
            "IO.fake@motherbee",
            &json!({"_system":{"config_version":4},"io":{"wrong":"x"}}),
        );
        let st = bootstrap_io_control_plane_state_with_root("IO.fake@motherbee", &FakeAdapter, &root)
            .expect("boot");
        assert_eq!(st.current_state, IoNodeLifecycleState::FailedConfig);
        assert_eq!(st.config_version, 4);
        assert!(st.effective_config.is_none());
    }
}
