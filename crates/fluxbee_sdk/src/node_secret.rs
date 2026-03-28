use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::managed_node::{
    managed_node_instance_dir, managed_node_instance_dir_with_root, ManagedNodeError,
};

pub const NODE_SECRET_FILE_NAME: &str = "secrets.json";
pub const NODE_SECRET_SCHEMA_VERSION: u32 = 1;
pub const NODE_SECRET_REDACTION_TOKEN: &str = "***REDACTED***";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeSecretRecord {
    #[serde(default = "default_node_secret_schema_version")]
    pub schema_version: u32,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by_ilk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub secrets: Map<String, Value>,
}

impl Default for NodeSecretRecord {
    fn default() -> Self {
        Self {
            schema_version: NODE_SECRET_SCHEMA_VERSION,
            updated_at: Utc::now().to_rfc3339(),
            updated_by_ilk: None,
            updated_by_label: None,
            trace_id: None,
            secrets: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeSecretWriteOptions {
    pub updated_by_ilk: Option<String>,
    pub updated_by_label: Option<String>,
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeSecretDescriptor {
    pub field: String,
    pub storage_key: String,
    pub required: bool,
    pub configured: bool,
    pub value_redacted: bool,
    pub persistence: String,
}

impl NodeSecretDescriptor {
    pub fn new(field: impl Into<String>, storage_key: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            storage_key: storage_key.into(),
            required: false,
            configured: false,
            value_redacted: true,
            persistence: "local_file".to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NodeSecretError {
    #[error(transparent)]
    ManagedNode(#[from] ManagedNodeError),
    #[error("invalid secret path '{0}'")]
    InvalidPath(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn node_secret_path(node_name: &str) -> Result<PathBuf, NodeSecretError> {
    Ok(managed_node_instance_dir(node_name)?.join(NODE_SECRET_FILE_NAME))
}

pub fn node_secret_path_with_root(
    node_name: &str,
    root: impl Into<PathBuf>,
) -> Result<PathBuf, NodeSecretError> {
    Ok(managed_node_instance_dir_with_root(node_name, root)?.join(NODE_SECRET_FILE_NAME))
}

pub fn ensure_node_secret_dir(node_name: &str) -> Result<PathBuf, NodeSecretError> {
    let dir = managed_node_instance_dir(node_name)?;
    ensure_dir_0700(&dir)?;
    Ok(dir)
}

pub fn ensure_node_secret_dir_with_root(
    node_name: &str,
    root: impl Into<PathBuf>,
) -> Result<PathBuf, NodeSecretError> {
    let dir = managed_node_instance_dir_with_root(node_name, root)?;
    ensure_dir_0700(&dir)?;
    Ok(dir)
}

pub fn load_node_secret_record(node_name: &str) -> Result<NodeSecretRecord, NodeSecretError> {
    load_node_secret_record_from_path(node_secret_path(node_name)?)
}

pub fn load_node_secret_record_with_root(
    node_name: &str,
    root: impl Into<PathBuf>,
) -> Result<NodeSecretRecord, NodeSecretError> {
    load_node_secret_record_from_path(node_secret_path_with_root(node_name, root)?)
}

pub fn load_node_secret_record_from_path(
    path: impl AsRef<Path>,
) -> Result<NodeSecretRecord, NodeSecretError> {
    let path = path.as_ref();
    let mut buf = String::new();
    File::open(path)?.read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf)?)
}

pub fn save_node_secret_record(
    node_name: &str,
    record: &NodeSecretRecord,
) -> Result<PathBuf, NodeSecretError> {
    let path = node_secret_path(node_name)?;
    save_node_secret_record_to_path(path, record)?;
    Ok(node_secret_path(node_name)?)
}

pub fn save_node_secret_record_with_root(
    node_name: &str,
    root: impl Into<PathBuf>,
    record: &NodeSecretRecord,
) -> Result<PathBuf, NodeSecretError> {
    let path = node_secret_path_with_root(node_name, root)?;
    save_node_secret_record_to_path(&path, record)?;
    Ok(path)
}

pub fn save_node_secret_record_to_path(
    path: impl AsRef<Path>,
    record: &NodeSecretRecord,
) -> Result<(), NodeSecretError> {
    let path = path.as_ref();
    let parent = path
        .parent()
        .ok_or_else(|| NodeSecretError::InvalidPath(path.display().to_string()))?;
    ensure_dir_0700(parent)?;

    let temp_path = temporary_secret_path(path)?;
    let json = serde_json::to_vec_pretty(record)?;
    {
        let mut file = open_0600_for_write(&temp_path)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&temp_path, path)?;
    set_file_mode_0600(path)?;
    Ok(())
}

pub fn build_node_secret_record(
    secrets: Map<String, Value>,
    options: &NodeSecretWriteOptions,
) -> NodeSecretRecord {
    NodeSecretRecord {
        schema_version: NODE_SECRET_SCHEMA_VERSION,
        updated_at: Utc::now().to_rfc3339(),
        updated_by_ilk: options.updated_by_ilk.clone(),
        updated_by_label: options.updated_by_label.clone(),
        trace_id: options.trace_id.clone(),
        secrets,
    }
}

pub fn redact_secret_value(_value: &Value) -> Value {
    Value::String(NODE_SECRET_REDACTION_TOKEN.to_string())
}

pub fn redact_secret_map(secrets: &Map<String, Value>) -> Map<String, Value> {
    secrets
        .iter()
        .map(|(key, value)| (key.clone(), redact_secret_value(value)))
        .collect()
}

pub fn redacted_node_secret_record(record: &NodeSecretRecord) -> NodeSecretRecord {
    NodeSecretRecord {
        schema_version: record.schema_version,
        updated_at: record.updated_at.clone(),
        updated_by_ilk: record.updated_by_ilk.clone(),
        updated_by_label: record.updated_by_label.clone(),
        trace_id: record.trace_id.clone(),
        secrets: redact_secret_map(&record.secrets),
    }
}

fn default_node_secret_schema_version() -> u32 {
    NODE_SECRET_SCHEMA_VERSION
}

fn temporary_secret_path(path: &Path) -> Result<PathBuf, NodeSecretError> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| NodeSecretError::InvalidPath(path.display().to_string()))?;
    Ok(path.with_file_name(format!(".{file_name}.tmp")))
}

fn open_0600_for_write(path: &Path) -> Result<File, NodeSecretError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    set_file_mode_0600(path)?;
    Ok(file)
}

fn ensure_dir_0700(path: &Path) -> Result<(), NodeSecretError> {
    fs::create_dir_all(path)?;
    set_dir_mode_0700(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_dir_mode_0700(path: &Path) -> Result<(), NodeSecretError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_dir_mode_0700(_path: &Path) -> Result<(), NodeSecretError> {
    Ok(())
}

#[cfg(unix)]
fn set_file_mode_0600(path: &Path) -> Result<(), NodeSecretError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_file_mode_0600(_path: &Path) -> Result<(), NodeSecretError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("fluxbee-sdk-node-secret-{nanos}"))
    }

    #[test]
    fn node_secret_path_uses_kind_and_full_name() {
        let path =
            node_secret_path_with_root("AI.chat@motherbee", "/tmp/fluxbee-secret-test").unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/fluxbee-secret-test/AI/AI.chat@motherbee/secrets.json")
        );
    }

    #[test]
    fn ensure_secret_dir_and_file_enforce_permissions() {
        let root = temp_root();
        let dir = ensure_node_secret_dir_with_root("AI.chat@motherbee", &root).unwrap();
        let mut secrets = Map::new();
        secrets.insert(
            "openai_api_key".to_string(),
            Value::String("sk-test".to_string()),
        );
        let path = save_node_secret_record_with_root(
            "AI.chat@motherbee",
            &root,
            &build_node_secret_record(secrets, &NodeSecretWriteOptions::default()),
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let root = temp_root();
        let path = node_secret_path_with_root("AI.chat@motherbee", &root).unwrap();

        let mut first = Map::new();
        first.insert(
            "openai_api_key".to_string(),
            Value::String("sk-first".to_string()),
        );
        save_node_secret_record_with_root(
            "AI.chat@motherbee",
            &root,
            &build_node_secret_record(first, &NodeSecretWriteOptions::default()),
        )
        .unwrap();

        let mut second = Map::new();
        second.insert(
            "openai_api_key".to_string(),
            Value::String("sk-second".to_string()),
        );
        save_node_secret_record_with_root(
            "AI.chat@motherbee",
            &root,
            &build_node_secret_record(second, &NodeSecretWriteOptions::default()),
        )
        .unwrap();

        let loaded = load_node_secret_record_with_root("AI.chat@motherbee", &root).unwrap();
        assert_eq!(
            loaded.secrets.get("openai_api_key"),
            Some(&Value::String("sk-second".to_string()))
        );
        assert!(!path.with_file_name(".secrets.json.tmp").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_and_save_roundtrip_preserves_metadata() {
        let root = temp_root();
        let mut secrets = Map::new();
        secrets.insert(
            "slack_bot_token".to_string(),
            Value::String("xoxb-1".to_string()),
        );
        let record = build_node_secret_record(
            secrets,
            &NodeSecretWriteOptions {
                updated_by_ilk: Some("ilk:test".to_string()),
                updated_by_label: Some("archi".to_string()),
                trace_id: Some("trace-1".to_string()),
            },
        );

        save_node_secret_record_with_root("IO.slack.T123@motherbee", &root, &record).unwrap();
        let loaded = load_node_secret_record_with_root("IO.slack.T123@motherbee", &root).unwrap();

        assert_eq!(loaded.schema_version, NODE_SECRET_SCHEMA_VERSION);
        assert_eq!(loaded.updated_by_ilk.as_deref(), Some("ilk:test"));
        assert_eq!(loaded.updated_by_label.as_deref(), Some("archi"));
        assert_eq!(loaded.trace_id.as_deref(), Some("trace-1"));
        assert_eq!(
            loaded.secrets.get("slack_bot_token"),
            Some(&Value::String("xoxb-1".to_string()))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn redaction_output_masks_secret_values() {
        let mut secrets = Map::new();
        secrets.insert(
            "openai_api_key".to_string(),
            Value::String("sk-secret".to_string()),
        );
        secrets.insert("numeric".to_string(), Value::Number(7.into()));
        let redacted = redact_secret_map(&secrets);

        assert_eq!(
            redacted.get("openai_api_key"),
            Some(&Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()))
        );
        assert_eq!(
            redacted.get("numeric"),
            Some(&Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()))
        );
    }

    #[test]
    fn node_secret_descriptor_defaults_to_redacted_local_file() {
        let descriptor = NodeSecretDescriptor::new("behavior.openai.api_key", "openai_api_key");
        assert_eq!(descriptor.field, "behavior.openai.api_key");
        assert_eq!(descriptor.storage_key, "openai_api_key");
        assert!(!descriptor.required);
        assert!(!descriptor.configured);
        assert!(descriptor.value_redacted);
        assert_eq!(descriptor.persistence, "local_file");
    }
}
