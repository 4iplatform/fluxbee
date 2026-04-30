use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const LINKEDHELPER_STATE_SUBDIR: &str = "io-linkedhelper";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkedHelperStateFile {
    pub node_name: String,
    pub updated_at_ms: u64,
    #[serde(flatten)]
    pub state: LinkedHelperDurableState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LinkedHelperDurableState {
    #[serde(default)]
    pub adapters: HashMap<String, AdapterStateRecord>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileStateRecord>,
    #[serde(default)]
    pub own_ichs: HashMap<String, IchStateRecord>,
    #[serde(default)]
    pub pending_deliveries: HashMap<String, Vec<PendingDeliveryRecord>>,
}

#[derive(Debug, Clone)]
pub struct AdapterSnapshot {
    pub adapter_id: String,
    pub tenant_id: String,
    pub label: Option<String>,
    pub dst_node: Option<String>,
    pub installation_key_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdapterStateRecord {
    pub adapter_id: String,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst_node: Option<String>,
    pub installation_key_ref: String,
    #[serde(default)]
    pub config_present: bool,
    #[serde(default)]
    pub known_profile_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_poll_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request_id: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileStateRecord {
    pub external_profile_id: String,
    pub adapter_id: String,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ich_id: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IchStateRecord {
    pub ich_id: String,
    pub adapter_id: String,
    pub external_profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ilk_id: Option<String>,
    pub automation_enabled: bool,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingDeliveryRecord {
    pub delivery_id: String,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_key: Option<String>,
    #[serde(flatten)]
    pub item: StoredResponseItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum StoredResponseItem {
    #[serde(rename = "ack")]
    Ack {
        response_id: String,
        adapter_id: String,
        event_id: String,
    },
    #[serde(rename = "result")]
    Result {
        response_id: String,
        adapter_id: String,
        event_id: String,
        status: String,
        result_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retryable: Option<bool>,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat {
        response_id: String,
        adapter_id: String,
        timestamp: String,
    },
}

impl LinkedHelperDurableState {
    pub fn sync_adapters(&mut self, snapshots: &[AdapterSnapshot]) {
        let now_ms = now_epoch_ms();
        let active_ids: HashSet<&str> = snapshots.iter().map(|snapshot| snapshot.adapter_id.as_str()).collect();

        for snapshot in snapshots {
            let record = self
                .adapters
                .entry(snapshot.adapter_id.clone())
                .or_insert_with(|| AdapterStateRecord {
                    adapter_id: snapshot.adapter_id.clone(),
                    tenant_id: snapshot.tenant_id.clone(),
                    label: snapshot.label.clone(),
                    dst_node: snapshot.dst_node.clone(),
                    installation_key_ref: snapshot.installation_key_ref.clone(),
                    config_present: true,
                    known_profile_ids: Vec::new(),
                    last_poll_at_ms: None,
                    last_request_id: None,
                    updated_at_ms: now_ms,
                });
            record.tenant_id = snapshot.tenant_id.clone();
            record.label = snapshot.label.clone();
            record.dst_node = snapshot.dst_node.clone();
            record.installation_key_ref = snapshot.installation_key_ref.clone();
            record.config_present = true;
            record.updated_at_ms = now_ms;
        }

        for (adapter_id, record) in &mut self.adapters {
            if !active_ids.contains(adapter_id.as_str()) {
                record.config_present = false;
                record.updated_at_ms = now_ms;
            }
        }
    }

    pub fn mark_adapter_poll(&mut self, adapter_id: &str, request_id: &str) {
        let now_ms = now_epoch_ms();
        let record = self
            .adapters
            .entry(adapter_id.to_string())
            .or_insert_with(|| AdapterStateRecord {
                adapter_id: adapter_id.to_string(),
                tenant_id: String::new(),
                label: None,
                dst_node: None,
                installation_key_ref: String::new(),
                config_present: false,
                known_profile_ids: Vec::new(),
                last_poll_at_ms: None,
                last_request_id: None,
                updated_at_ms: now_ms,
            });
        record.last_poll_at_ms = Some(now_ms);
        record.last_request_id = if request_id.is_empty() {
            None
        } else {
            Some(request_id.to_string())
        };
        record.updated_at_ms = now_ms;
    }

    pub fn drain_pending_deliveries(&mut self, adapter_id: &str) -> Vec<StoredResponseItem> {
        self.pending_deliveries
            .remove(adapter_id)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.item)
            .collect()
    }

    pub fn enqueue_pending_delivery(
        &mut self,
        adapter_id: &str,
        delivery_id: String,
        collapse_key: Option<String>,
        item: StoredResponseItem,
    ) {
        let queue = self
            .pending_deliveries
            .entry(adapter_id.to_string())
            .or_default();
        if let Some(key) = collapse_key.as_deref() {
            if let Some(existing) = queue
                .iter_mut()
                .find(|entry| entry.collapse_key.as_deref() == Some(key))
            {
                existing.delivery_id = delivery_id;
                existing.created_at_ms = now_epoch_ms();
                existing.item = item;
                return;
            }
        }
        queue.push(PendingDeliveryRecord {
            delivery_id,
            created_at_ms: now_epoch_ms(),
            collapse_key,
            item,
        });
    }

    pub fn upsert_profile(
        &mut self,
        adapter_id: &str,
        tenant_id: &str,
        external_profile_id: &str,
        ilk_id: Option<String>,
        ich_id: Option<String>,
        status: &str,
        display_name: Option<String>,
        metadata: Option<Value>,
    ) {
        let now_ms = now_epoch_ms();
        let profile = self
            .profiles
            .entry(external_profile_id.to_string())
            .or_insert_with(|| ProfileStateRecord {
                external_profile_id: external_profile_id.to_string(),
                adapter_id: adapter_id.to_string(),
                tenant_id: tenant_id.to_string(),
                ilk_id: None,
                ich_id: None,
                status: status.to_string(),
                display_name: display_name.clone(),
                metadata: metadata.clone(),
                updated_at_ms: now_ms,
            });
        profile.adapter_id = adapter_id.to_string();
        profile.tenant_id = tenant_id.to_string();
        profile.ilk_id = ilk_id;
        profile.ich_id = ich_id;
        profile.status = status.to_string();
        profile.display_name = display_name;
        profile.metadata = metadata;
        profile.updated_at_ms = now_ms;

        let adapter = self
            .adapters
            .entry(adapter_id.to_string())
            .or_insert_with(|| AdapterStateRecord {
                adapter_id: adapter_id.to_string(),
                tenant_id: tenant_id.to_string(),
                label: None,
                dst_node: None,
                installation_key_ref: String::new(),
                config_present: false,
                known_profile_ids: Vec::new(),
                last_poll_at_ms: None,
                last_request_id: None,
                updated_at_ms: now_ms,
            });
        if !adapter
            .known_profile_ids
            .iter()
            .any(|known| known == external_profile_id)
        {
            adapter
                .known_profile_ids
                .push(external_profile_id.to_string());
        }
        adapter.updated_at_ms = now_ms;
    }
}

pub fn linkedhelper_state_dir(base_state_dir: &Path) -> PathBuf {
    base_state_dir.join(LINKEDHELPER_STATE_SUBDIR)
}

pub fn linkedhelper_state_path(base_state_dir: &Path, node_name: &str) -> anyhow::Result<PathBuf> {
    Ok(linkedhelper_state_dir(base_state_dir).join(format!(
        "{}.json",
        normalize_node_name_for_filename(node_name)?
    )))
}

pub fn load_linkedhelper_state(
    base_state_dir: &Path,
    node_name: &str,
) -> anyhow::Result<Option<LinkedHelperStateFile>> {
    let path = linkedhelper_state_path(base_state_dir, node_name)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str::<LinkedHelperStateFile>(&raw)?))
}

pub fn persist_linkedhelper_state(
    base_state_dir: &Path,
    node_name: &str,
    state: &LinkedHelperDurableState,
) -> anyhow::Result<PathBuf> {
    let path = linkedhelper_state_path(base_state_dir, node_name)?;
    let payload = LinkedHelperStateFile {
        node_name: node_name.trim().to_string(),
        updated_at_ms: now_epoch_ms(),
        state: state.clone(),
    };
    write_json_atomic(&path, &payload)?;
    Ok(path)
}

fn write_json_atomic(path: &Path, payload: &LinkedHelperStateFile) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid linkedhelper state path"))?;
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

fn normalize_node_name_for_filename(node_name: &str) -> anyhow::Result<String> {
    let trimmed = node_name.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("invalid node_name"));
    }
    Ok(trimmed.replace(['/', '\\'], "_"))
}

pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
