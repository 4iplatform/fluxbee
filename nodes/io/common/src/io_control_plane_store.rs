//! IO control-plane persistence — now a thin wrapper over the SDK's single-config persist. The old
//! dynamic-state file under `/var/lib/fluxbee/state/io-nodes/` is gone (it was the two-location model
//! that caused the stale-config shadow on respawn, BUG-4). CONFIG_SET now writes back to the node-dir
//! `config.json`, preserving the orchestrator-owned `_system` block.

use crate::io_control_plane::IoControlPlaneState;
use fluxbee_sdk::managed_control_plane::{persist_effective_config_with_root, DEFAULT_MANAGED_NODES_ROOT};
use std::path::Path;

/// DEPRECATED vestigial helper: returns the legacy `/var/lib/fluxbee/state` base. Nothing is stored
/// there anymore (single-config model persists to the node-dir config.json). Kept only so adapters
/// that still carry a now-unused `state_dir` field compile; remove with that field.
pub fn default_state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/var/lib/fluxbee/state")
}

#[derive(Debug, thiserror::Error)]
pub enum IoControlPlaneStoreError {
    #[error("no effective config to persist")]
    NoEffectiveConfig,
    #[error("control-plane error: {0}")]
    ControlPlane(String),
}

/// Persist the current effective config back to the node-dir `config.json` (SDK single-config model,
/// preserving `_system`). No-op-safe: if there is no effective config (Unconfigured / FAILED_CONFIG)
/// there is nothing operator-authored to persist.
pub fn persist_io_control_plane_state(
    node_name: &str,
    state: &IoControlPlaneState,
) -> Result<(), IoControlPlaneStoreError> {
    persist_io_control_plane_state_with_root(
        node_name,
        state,
        Path::new(DEFAULT_MANAGED_NODES_ROOT),
    )
}

/// [`persist_io_control_plane_state`] with an explicit nodes root (for tests).
pub fn persist_io_control_plane_state_with_root(
    node_name: &str,
    state: &IoControlPlaneState,
    nodes_root: &Path,
) -> Result<(), IoControlPlaneStoreError> {
    let effective = state
        .effective_config
        .as_ref()
        .ok_or(IoControlPlaneStoreError::NoEffectiveConfig)?;
    persist_effective_config_with_root(node_name, state.config_version, effective, nodes_root)
        .map_err(|e| IoControlPlaneStoreError::ControlPlane(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_control_plane::{IoConfigSource, IoNodeLifecycleState};
    use fluxbee_sdk::managed_node_config_path_with_root;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("io-store-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn persist_writes_node_dir_config_preserving_system() {
        let root = temp_root("persist");
        let node = "IO.fake@motherbee";
        let path = managed_node_config_path_with_root(node, &root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({"_system":{"config_version":1,"ilk_id":"ilk:x"},"io":{"dst_node":"a"}}))
                .unwrap(),
        )
        .unwrap();

        let state = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            config_source: IoConfigSource::OrchestratorFallback,
            schema_version: 1,
            config_version: 2,
            effective_config: Some(json!({"io":{"dst_node":"AI.x@motherbee"}})),
            last_error: None,
        };
        persist_io_control_plane_state_with_root(node, &state, &root).expect("persist");

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // _system preserved (ilk_id kept, version bumped); the new effective config written.
        assert_eq!(
            saved.get("_system").and_then(|s| s.get("ilk_id")),
            Some(&json!("ilk:x"))
        );
        assert_eq!(
            saved.get("_system").and_then(|s| s.get("config_version")),
            Some(&json!(2))
        );
        assert_eq!(
            saved.get("io").and_then(|i| i.get("dst_node")),
            Some(&json!("AI.x@motherbee"))
        );
    }

    #[test]
    fn persist_without_effective_config_is_rejected() {
        let root = temp_root("noeff");
        let state = IoControlPlaneState::default();
        let err = persist_io_control_plane_state_with_root("IO.fake@motherbee", &state, &root)
            .expect_err("no effective config");
        assert!(matches!(err, IoControlPlaneStoreError::NoEffectiveConfig));
    }
}
