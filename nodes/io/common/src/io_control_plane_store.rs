use crate::io_control_plane::IoControlPlaneState;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_STATE_DIR: &str = "/var/lib/fluxbee/state";
pub const IO_NODES_STATE_SUBDIR: &str = "io-nodes";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IoControlPlaneStateFile {
    pub node_name: String,
    pub updated_at_ms: u64,
    #[serde(flatten)]
    pub state: IoControlPlaneState,
}

#[derive(Debug, thiserror::Error)]
pub enum IoControlPlaneStoreError {
    #[error("invalid node_name")]
    InvalidNodeName,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn default_state_dir() -> PathBuf {
    PathBuf::from(DEFAULT_STATE_DIR)
}

pub fn io_nodes_state_dir(base_state_dir: &Path) -> PathBuf {
    base_state_dir.join(IO_NODES_STATE_SUBDIR)
}

pub fn io_dynamic_state_path(
    base_state_dir: &Path,
    node_name: &str,
) -> Result<PathBuf, IoControlPlaneStoreError> {
    let normalized = normalize_node_name_for_filename(node_name)?;
    Ok(io_nodes_state_dir(base_state_dir).join(format!("{normalized}.json")))
}

pub fn load_io_control_plane_state(
    base_state_dir: &Path,
    node_name: &str,
) -> Result<Option<IoControlPlaneStateFile>, IoControlPlaneStoreError> {
    let path = io_dynamic_state_path(base_state_dir, node_name)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    let parsed = serde_json::from_str::<IoControlPlaneStateFile>(&raw)?;
    Ok(Some(parsed))
}

pub fn persist_io_control_plane_state(
    base_state_dir: &Path,
    node_name: &str,
    state: &IoControlPlaneState,
) -> Result<PathBuf, IoControlPlaneStoreError> {
    let path = io_dynamic_state_path(base_state_dir, node_name)?;
    let payload = IoControlPlaneStateFile {
        node_name: node_name.trim().to_string(),
        updated_at_ms: now_epoch_ms(),
        state: state.clone(),
    };
    write_json_atomic(&path, &payload)?;
    Ok(path)
}

fn write_json_atomic(
    path: &Path,
    payload: &IoControlPlaneStateFile,
) -> Result<(), IoControlPlaneStoreError> {
    let parent = path
        .parent()
        .ok_or(IoControlPlaneStoreError::InvalidNodeName)?;
    fs::create_dir_all(parent)?;

    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("state"),
        std::process::id(),
        now_epoch_ms()
    );
    let tmp_path = parent.join(tmp_name);

    let json = serde_json::to_vec_pretty(payload)?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)?;
    file.write_all(&json)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn normalize_node_name_for_filename(node_name: &str) -> Result<String, IoControlPlaneStoreError> {
    let trimmed = node_name.trim();
    if trimmed.is_empty() {
        return Err(IoControlPlaneStoreError::InvalidNodeName);
    }
    Ok(trimmed.replace(['/', '\\'], "_"))
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_control_plane::{IoConfigSource, IoNodeLifecycleState};

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "io-control-plane-store-{label}-{}-{}",
            std::process::id(),
            now_epoch_ms()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn dynamic_state_path_uses_io_nodes_subdir_and_sanitized_node_name() {
        let base = PathBuf::from("/tmp/fluxbee-state");
        let path = io_dynamic_state_path(&base, "IO.slack/T123@motherbee").expect("path");
        assert_eq!(
            path,
            PathBuf::from("/tmp/fluxbee-state/io-nodes/IO.slack_T123@motherbee.json")
        );
    }

    #[test]
    fn persist_and_load_roundtrip() {
        let base = temp_dir("roundtrip");
        let state = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            config_source: IoConfigSource::Dynamic,
            schema_version: 1,
            config_version: 7,
            effective_config: Some(serde_json::json!({"io":{"dst_node":"resolve"}})),
            last_error: None,
        };

        let path =
            persist_io_control_plane_state(&base, "IO.slack.T123@motherbee", &state).expect("save");
        assert!(path.exists());

        let loaded = load_io_control_plane_state(&base, "IO.slack.T123@motherbee")
            .expect("load")
            .expect("state exists");
        assert_eq!(loaded.node_name, "IO.slack.T123@motherbee");
        assert_eq!(loaded.state, state);
    }

    #[test]
    fn load_returns_none_when_missing() {
        let base = temp_dir("missing");
        let loaded = load_io_control_plane_state(&base, "IO.slack.T123@motherbee").expect("load");
        assert!(loaded.is_none());
    }
}
