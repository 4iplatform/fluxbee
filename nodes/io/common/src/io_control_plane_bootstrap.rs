use crate::io_control_plane::{
    IoConfigSource, IoControlPlaneErrorInfo, IoControlPlaneState, IoNodeLifecycleState,
};
use crate::io_control_plane_store::{
    load_io_control_plane_state, IoControlPlaneStateFile, IoControlPlaneStoreError,
};
use fluxbee_sdk::{managed_node_config_path, managed_node_config_path_with_root, ManagedNodeError};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_ORCHESTRATOR_NODES_ROOT: &str = "/var/lib/fluxbee/nodes";

#[derive(Debug, thiserror::Error)]
pub enum IoControlPlaneBootstrapError {
    #[error("state store error: {0}")]
    Store(#[from] IoControlPlaneStoreError),
    #[error("managed node path error: {0}")]
    ManagedNode(#[from] ManagedNodeError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn bootstrap_io_control_plane_state(
    state_dir: &Path,
    node_name: &str,
) -> Result<IoControlPlaneState, IoControlPlaneBootstrapError> {
    bootstrap_io_control_plane_state_with_nodes_root(
        state_dir,
        node_name,
        Path::new(DEFAULT_ORCHESTRATOR_NODES_ROOT),
    )
}

pub fn bootstrap_io_control_plane_state_with_nodes_root(
    state_dir: &Path,
    node_name: &str,
    nodes_root: &Path,
) -> Result<IoControlPlaneState, IoControlPlaneBootstrapError> {
    if let Some(dynamic) = load_io_control_plane_state(state_dir, node_name)? {
        return Ok(state_from_dynamic_file(dynamic));
    }

    if let Some(orchestrator_cfg) = load_effective_config_from_orchestrator(node_name, nodes_root)?
    {
        return Ok(orchestrator_cfg);
    }

    Ok(IoControlPlaneState::default())
}

fn state_from_dynamic_file(file: IoControlPlaneStateFile) -> IoControlPlaneState {
    let mut state = file.state;
    state.config_source = IoConfigSource::Dynamic;
    state
}

fn load_effective_config_from_orchestrator(
    node_name: &str,
    nodes_root: &Path,
) -> Result<Option<IoControlPlaneState>, IoControlPlaneBootstrapError> {
    let path = orchestrator_config_path(node_name, nodes_root)?;
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&path)?;
    let root: Value = serde_json::from_str(&raw)?;
    let effective = extract_effective_config(&root);
    if !effective.is_object() {
        return Ok(Some(IoControlPlaneState {
            current_state: IoNodeLifecycleState::FailedConfig,
            config_source: IoConfigSource::OrchestratorFallback,
            schema_version: 1,
            config_version: 0,
            effective_config: None,
            last_error: Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: format!(
                    "orchestrator fallback config is not an object: {}",
                    path.display()
                ),
            }),
        }));
    }

    let config_version = root
        .get("_system")
        .and_then(|v| v.get("config_version"))
        .and_then(Value::as_u64)
        .unwrap_or(1);

    Ok(Some(IoControlPlaneState {
        current_state: IoNodeLifecycleState::Configured,
        config_source: IoConfigSource::OrchestratorFallback,
        schema_version: 1,
        config_version,
        effective_config: Some(effective),
        last_error: None,
    }))
}

fn extract_effective_config(root: &Value) -> Value {
    let mut candidate = root.get("config").cloned().unwrap_or_else(|| root.clone());
    if let Some(obj) = candidate.as_object_mut() {
        obj.remove("_system");
    }
    candidate
}

fn orchestrator_config_path(
    node_name: &str,
    nodes_root: &Path,
) -> Result<PathBuf, IoControlPlaneBootstrapError> {
    if nodes_root == Path::new(DEFAULT_ORCHESTRATOR_NODES_ROOT) {
        return Ok(managed_node_config_path(node_name)?);
    }
    Ok(managed_node_config_path_with_root(node_name, nodes_root)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_control_plane::{IoConfigSource, IoNodeLifecycleState};
    use crate::io_control_plane_store::persist_io_control_plane_state;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "io-bootstrap-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn bootstrap_prefers_dynamic_state_over_orchestrator_fallback() {
        let state_dir = temp_dir("dynamic-first-state");
        let nodes_root = temp_dir("dynamic-first-nodes");
        let node_name = "IO.slack.T123@motherbee";

        let dynamic_state = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            config_source: IoConfigSource::Dynamic,
            schema_version: 1,
            config_version: 9,
            effective_config: Some(serde_json::json!({"io":{"dst_node":"resolve"}})),
            last_error: None,
        };
        persist_io_control_plane_state(&state_dir, node_name, &dynamic_state).expect("persist");

        let orchestrator_cfg_path =
            managed_node_config_path_with_root(node_name, &nodes_root).expect("managed path");
        fs::create_dir_all(orchestrator_cfg_path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &orchestrator_cfg_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "_system": {"config_version": 3},
                "config": {"io":{"dst_node":"SY.frontdesk.gov@motherbee"}}
            }))
            .expect("json"),
        )
        .expect("write orchestrator cfg");

        let boot =
            bootstrap_io_control_plane_state_with_nodes_root(&state_dir, node_name, &nodes_root)
                .expect("bootstrap");
        assert_eq!(boot.config_source, IoConfigSource::Dynamic);
        assert_eq!(boot.config_version, 9);
    }

    #[test]
    fn bootstrap_uses_orchestrator_fallback_when_dynamic_missing() {
        let state_dir = temp_dir("fallback-state");
        let nodes_root = temp_dir("fallback-nodes");
        let node_name = "IO.slack.T123@motherbee";

        let orchestrator_cfg_path =
            managed_node_config_path_with_root(node_name, &nodes_root).expect("managed path");
        fs::create_dir_all(orchestrator_cfg_path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &orchestrator_cfg_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "_system": {"config_version": 4},
                "config": {"io":{"dst_node":"SY.frontdesk.gov@motherbee"}}
            }))
            .expect("json"),
        )
        .expect("write orchestrator cfg");

        let boot =
            bootstrap_io_control_plane_state_with_nodes_root(&state_dir, node_name, &nodes_root)
                .expect("bootstrap");
        assert_eq!(boot.config_source, IoConfigSource::OrchestratorFallback);
        assert_eq!(boot.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(boot.config_version, 4);
    }

    #[test]
    fn bootstrap_returns_default_when_no_sources_exist() {
        let state_dir = temp_dir("none-state");
        let nodes_root = temp_dir("none-nodes");
        let node_name = "IO.slack.T123@motherbee";

        let boot =
            bootstrap_io_control_plane_state_with_nodes_root(&state_dir, node_name, &nodes_root)
                .expect("bootstrap");
        assert_eq!(boot.current_state, IoNodeLifecycleState::Unconfigured);
        assert_eq!(boot.config_source, IoConfigSource::None);
        assert_eq!(boot.config_version, 0);
    }
}
