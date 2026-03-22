use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashMap;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::fs;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::{AiSdkError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStateRecord {
    pub thread_id: String,
    pub data: Value,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedThreadStateRecord {
    thread_id: String,
    data: Value,
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<u64>,
}

#[async_trait]
pub trait ThreadStateStore: Send + Sync {
    async fn get(&self, thread_id: &str) -> Result<Option<ThreadStateRecord>>;
    async fn put(&self, thread_id: &str, data: Value, ttl_seconds: Option<u64>) -> Result<()>;
    async fn delete(&self, thread_id: &str) -> Result<()>;
}

/// MVP concrete adapter for thread state under a LanceDB-compatible node-local path.
/// In this phase, records are persisted as one JSON file per thread inside `<root>/threads/`.
#[derive(Debug, Clone)]
pub struct LanceDbThreadStateStore {
    root_dir: PathBuf,
    thread_gates: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl LanceDbThreadStateStore {
    pub fn path_for_node(state_dir: impl AsRef<Path>, node_name: &str) -> PathBuf {
        state_dir
            .as_ref()
            .join("ai-nodes")
            .join(sanitize_node_name(node_name))
            .join("lancedb")
    }

    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            thread_gates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    fn threads_dir(&self) -> PathBuf {
        self.root_dir.join("threads")
    }

    fn thread_file_path(&self, thread_id: &str) -> PathBuf {
        self.threads_dir()
            .join(format!("{}.json", sanitize_thread_id(thread_id)))
    }

    fn thread_lock_key(thread_id: &str) -> String {
        sanitize_thread_id(thread_id)
    }

    async fn lock_thread(&self, thread_id: &str) -> OwnedMutexGuard<()> {
        let key = Self::thread_lock_key(thread_id);
        let gate = {
            let mut gates = self.thread_gates.lock().await;
            gates
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        gate.lock_owned().await
    }

    pub async fn ensure_ready(&self) -> Result<()> {
        fs::create_dir_all(self.threads_dir())
            .await
            .map_err(|e| AiSdkError::Protocol(format!("thread state init failed: {e}")))?;
        Ok(())
    }
}

fn sanitize_node_name(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "ai-node".to_string()
    } else {
        output
    }
}

#[async_trait]
impl ThreadStateStore for LanceDbThreadStateStore {
    async fn get(&self, thread_id: &str) -> Result<Option<ThreadStateRecord>> {
        if thread_id.trim().is_empty() {
            return Err(AiSdkError::Protocol(
                "thread state get: empty thread_id".to_string(),
            ));
        }

        let _guard = self.lock_thread(thread_id).await;
        let path = self.thread_file_path(thread_id);
        let raw = match fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(AiSdkError::Protocol(format!(
                    "thread state get read failed: {e}"
                )));
            }
        };
        let stored: PersistedThreadStateRecord = serde_json::from_str(&raw)?;
        if stored.thread_id != thread_id {
            return Err(AiSdkError::Protocol(format!(
                "thread state get: key mismatch (requested='{thread_id}', stored='{}')",
                stored.thread_id
            )));
        }
        Ok(Some(ThreadStateRecord {
            thread_id: stored.thread_id,
            data: stored.data,
            updated_at: stored.updated_at,
            ttl_seconds: stored.ttl_seconds,
        }))
    }

    async fn put(&self, thread_id: &str, data: Value, ttl_seconds: Option<u64>) -> Result<()> {
        if thread_id.trim().is_empty() {
            return Err(AiSdkError::Protocol(
                "thread state put: empty thread_id".to_string(),
            ));
        }

        let _guard = self.lock_thread(thread_id).await;
        self.ensure_ready().await?;
        let path = self.thread_file_path(thread_id);
        let payload = PersistedThreadStateRecord {
            thread_id: thread_id.to_string(),
            data,
            updated_at: Utc::now().to_rfc3339(),
            ttl_seconds,
        };
        let json = serde_json::to_string_pretty(&payload)?;
        fs::write(path, json)
            .await
            .map_err(|e| AiSdkError::Protocol(format!("thread state put write failed: {e}")))?;
        Ok(())
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        if thread_id.trim().is_empty() {
            return Err(AiSdkError::Protocol(
                "thread state delete: empty thread_id".to_string(),
            ));
        }

        let _guard = self.lock_thread(thread_id).await;
        let path = self.thread_file_path(thread_id);
        match fs::remove_file(path).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AiSdkError::Protocol(format!(
                "thread state delete failed: {e}"
            ))),
        }
    }
}

fn sanitize_thread_id(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "thread".to_string()
    } else {
        output
    }
}
