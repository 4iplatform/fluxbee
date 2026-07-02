use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cloud_client::{AdapterEnrollResult, AdapterSyncConfig};
use crate::runtime_db::load_runtime_snapshot;

/**
 * Locally persisted adapter identity, config, and lightweight runtime state.
 */
#[derive(Debug, Serialize, Deserialize)]
pub struct AdapterState {
    #[serde(rename = "cloudBaseUrl")]
    pub cloud_base_url: String,
    #[serde(rename = "adapterId")]
    pub adapter_id: String,
    #[serde(rename = "adapterSecret")]
    pub adapter_secret: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: String,
    #[serde(rename = "adapterType", default = "default_adapter_type")]
    pub adapter_type: String,
    #[serde(rename = "adapterVersion", default = "default_adapter_version")]
    pub adapter_version: String,
    #[serde(rename = "adapterBuild", default = "default_adapter_build")]
    pub adapter_build: String,
    #[serde(rename = "syncConfig")]
    pub sync_config: AdapterSyncConfig,
    #[serde(rename = "enrolledAt")]
    pub enrolled_at: String,
    #[serde(rename = "lhRootPath", default)]
    pub lh_root_path: Option<String>,
    #[serde(rename = "pollIntervalSeconds", default = "default_poll_interval_seconds")]
    pub poll_interval_seconds: u64,
    #[serde(default)]
    pub runtime: AdapterRuntimeState,
}

/**
 * Lightweight operational snapshot persisted between adapter runs.
 */
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AdapterRuntimeState {
    #[serde(rename = "adapterStatus", default)]
    pub adapter_status: Option<String>,
    #[serde(rename = "lastSuccessfulAliveAt", default)]
    pub last_successful_alive_at: Option<String>,
    #[serde(rename = "lastSuccessfulDiscoveryAt", default)]
    pub last_successful_discovery_at: Option<String>,
    #[serde(rename = "lastScanAt", default)]
    pub last_scan_at: Option<String>,
    #[serde(rename = "lastErrorCode", default)]
    pub last_error_code: Option<String>,
    #[serde(rename = "lastErrorMessage", default)]
    pub last_error_message: Option<String>,
    #[serde(rename = "lastDiscoveryHash", default)]
    pub last_discovery_hash: Option<String>,
    #[serde(rename = "lastSeenInstancesCount", default)]
    pub last_seen_instances_count: Option<usize>,
    #[serde(rename = "lhRootStatus", default)]
    pub lh_root_status: Option<String>,
    #[serde(rename = "cloudLastResponseStatus", default)]
    pub cloud_last_response_status: Option<String>,
    #[serde(rename = "cloudDiscoveredInstances", default)]
    pub cloud_discovered_instances: Vec<AdapterCloudDiscoveredInstanceState>,
    #[serde(rename = "lastKnownDesiredStateVersion", default)]
    pub last_known_desired_state_version: Option<u64>,
    #[serde(rename = "desiredBindings", default)]
    pub desired_bindings: Vec<AdapterDesiredBindingState>,
    /// Set just before a self-update swaps the binary and re-execs; the new
    /// binary finalizes it on its first healthy alive (deletes the retained
    /// previous binary and records `last_update`). Persisted in the JSON state
    /// so it survives the re-exec.
    #[serde(rename = "pendingUpdate", default)]
    pub pending_update: Option<AdapterPendingUpdate>,
    /// Outcome of the most recent self-update attempt (success/failed/rolled_back).
    #[serde(rename = "lastUpdate", default)]
    pub last_update: Option<AdapterUpdateRecord>,
}

/**
 * In-flight self-update marker written before the binary swap + re-exec.
 */
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdapterPendingUpdate {
    #[serde(rename = "releaseId")]
    pub release_id: String,
    #[serde(rename = "fromVersion")]
    pub from_version: String,
    #[serde(rename = "toVersion")]
    pub to_version: String,
    /// Absolute path where the previous binary was retained for rollback.
    #[serde(rename = "prevBinaryPath")]
    pub prev_binary_path: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    /// Number of times the post-swap binary has booted with this marker still
    /// pending. The boot-gate increments it each boot and rolls back to the
    /// retained previous binary once it reaches the max (crash-loop guard).
    #[serde(rename = "bootAttempts", default)]
    pub boot_attempts: u32,
}

/**
 * Terminal record of a completed (or failed) self-update, surfaced in `status`.
 */
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdapterUpdateRecord {
    #[serde(rename = "releaseId")]
    pub release_id: String,
    #[serde(rename = "fromVersion")]
    pub from_version: String,
    #[serde(rename = "toVersion")]
    pub to_version: String,
    #[serde(rename = "appliedAt")]
    pub applied_at: String,
    /// One of `success`, `failed`, `rolled_back`, `version_mismatch`.
    pub result: String,
    #[serde(default)]
    pub error: Option<String>,
}

/**
 * Minimal local snapshot of the discovery entities acknowledged by Cloud.
 */
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AdapterCloudDiscoveredInstanceState {
    #[serde(rename = "discoveredInstanceId")]
    pub discovered_instance_id: String,
    #[serde(rename = "localInstanceId")]
    pub local_instance_id: String,
    pub status: String,
    #[serde(rename = "managedInstanceId", default)]
    pub managed_instance_id: Option<String>,
    #[serde(rename = "reportTo", default)]
    pub report_to: Option<String>,
}

/**
 * Minimal local snapshot of the Cloud desired bindings currently applied by the adapter.
 */
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AdapterDesiredBindingState {
    #[serde(rename = "localInstanceId")]
    pub local_instance_id: String,
    #[serde(rename = "managedInstanceId")]
    pub managed_instance_id: String,
    pub status: String,
    /// Where to report events for this binding (intermediate path:
    /// `direct_node_http`). Persisted so it survives `alive` cycles that return
    /// `desiredState: null` (unchanged version) and adapter restarts.
    #[serde(rename = "reportToKind", default)]
    pub report_to_kind: Option<String>,
    #[serde(rename = "reportToUrl", default)]
    pub report_to_url: Option<String>,
}

impl AdapterState {
    /**
     * Builds one persisted adapter state from the Cloud enrollment response.
     */
    pub fn from_enroll_result(cloud_base_url: &str, result: AdapterEnrollResult) -> Self {
        Self {
            cloud_base_url: cloud_base_url.to_string(),
            adapter_id: result.adapter_id,
            adapter_secret: result.adapter_secret,
            tenant_id: result.tenant_id,
            adapter_type: default_adapter_type(),
            adapter_version: default_adapter_version(),
            adapter_build: default_adapter_build(),
            sync_config: result.sync_config,
            enrolled_at: current_unix_timestamp_string(),
            lh_root_path: None,
            poll_interval_seconds: default_poll_interval_seconds(),
            runtime: AdapterRuntimeState::default(),
        }
    }

    /**
     * Resolves the current alive URL using the enrolled adapter identity.
     */
    pub fn alive_url(&self) -> String {
        format!(
            "{}/api/adapters/{}/alive",
            self.cloud_base_url.trim_end_matches('/'),
            self.adapter_id
        )
    }
}

/**
 * Reads the local adapter state JSON file.
 */
pub fn read_adapter_state(path: &Path) -> Result<AdapterState, Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err(format!(
            "Adapter state file does not exist at [{}]. Run enroll first or pass --state-file.",
            path.display()
        )
        .into());
    }

    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/**
 * Reads the local adapter bootstrap state and hydrates runtime fields from SQLite when present.
 */
pub fn read_adapter_state_with_runtime(path: &Path) -> Result<AdapterState, Box<dyn std::error::Error>> {
    let mut state = read_adapter_state(path)?;
    let db_path = runtime_db_path_from_state_path(path);

    if let Some(snapshot) = load_runtime_snapshot(&db_path)? {
        state.runtime.adapter_status = derive_optional_string(&snapshot.runtime_meta, "adapter_status");
        state.runtime.last_successful_alive_at =
            derive_optional_string(&snapshot.runtime_meta, "last_successful_alive_at");
        state.runtime.last_successful_discovery_at =
            derive_optional_string(&snapshot.runtime_meta, "last_successful_discovery_at");
        state.runtime.last_scan_at = derive_optional_string(&snapshot.runtime_meta, "last_scan_at");
        state.runtime.last_error_code = derive_optional_string(&snapshot.runtime_meta, "last_error_code");
        state.runtime.last_error_message =
            derive_optional_string(&snapshot.runtime_meta, "last_error_message");
        state.runtime.last_discovery_hash =
            derive_optional_string(&snapshot.runtime_meta, "last_discovery_hash");
        state.runtime.lh_root_status = derive_optional_string(&snapshot.runtime_meta, "lh_root_status");
        state.runtime.cloud_last_response_status =
            derive_optional_string(&snapshot.runtime_meta, "cloud_last_response_status");
        state.runtime.last_known_desired_state_version =
            derive_optional_u64(&snapshot.runtime_meta, "last_known_desired_state_version");
        state.runtime.last_seen_instances_count =
            derive_optional_usize(&snapshot.runtime_meta, "last_seen_instances_count");
        state.runtime.desired_bindings = snapshot.desired_bindings;
    }

    Ok(state)
}

/**
 * Persists the local adapter state JSON file.
 */
pub fn write_adapter_state(
    path: &Path,
    state: &AdapterState,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(path, format!("{}\n", serde_json::to_string_pretty(state)?))?;
    Ok(())
}

/**
 * Derives the colocated runtime SQLite path from the configured state file path.
 */
pub fn runtime_db_path_from_state_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(".linkedhelper-adapter-state.json");

    let db_file_name = file_name
        .strip_suffix(".json")
        .map(|value| format!("{}-runtime.db", value))
        .unwrap_or_else(|| format!("{}-runtime.db", file_name));

    path.with_file_name(db_file_name)
}

fn current_unix_timestamp_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    format!("{}", now)
}

fn default_adapter_type() -> String {
    String::from("linkedhelper")
}

fn default_adapter_version() -> String {
    String::from("0.1.0-rust-service")
}

fn default_adapter_build() -> String {
    String::from("dev")
}

fn default_poll_interval_seconds() -> u64 {
    60
}

fn derive_optional_string(
    values: &std::collections::BTreeMap<String, Option<String>>,
    key: &str,
) -> Option<String> {
    values.get(key).cloned().flatten().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn derive_optional_u64(
    values: &std::collections::BTreeMap<String, Option<String>>,
    key: &str,
) -> Option<u64> {
    derive_optional_string(values, key).and_then(|value| value.parse::<u64>().ok())
}

fn derive_optional_usize(
    values: &std::collections::BTreeMap<String, Option<String>>,
    key: &str,
) -> Option<usize> {
    derive_optional_string(values, key).and_then(|value| value.parse::<usize>().ok())
}
