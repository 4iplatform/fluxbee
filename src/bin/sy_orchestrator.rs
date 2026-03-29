use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

use chrono::{SecondsFormat, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Mutex;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::blob::{
    BlobConfig as SdkBlobConfig, BlobGcOptions, BlobToolkit, BLOB_ACTIVE_RETAIN_DAYS,
    BLOB_NAME_MAX_CHARS, BLOB_STAGING_TTL_HOURS,
};
use fluxbee_sdk::identity::{identity_system_call_ok, IdentityError, IdentitySystemRequest};
use fluxbee_sdk::nats::{request_local, NatsRequestEnvelope, NatsResponseEnvelope};
use fluxbee_sdk::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED,
    MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SCOPE_GLOBAL, SYSTEM_KIND,
};
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender, NodeUuidMode};
use json_router::{
    runtime_manifest::{
        load_runtime_manifest_from_paths as load_runtime_manifest_paths_shared,
        parse_runtime_manifest_file as parse_runtime_manifest_file_shared,
        runtime_manifest_write_v2_gate_enabled_from_env, write_runtime_manifest_file_atomic,
        RuntimeManifest, RuntimeManifestEntry,
    },
    shm::{
        now_epoch_ms, LsaRegionReader, LsaSnapshot, NodeEntry, RemoteHiveEntry, RemoteNodeEntry,
        RouterRegionReader, ShmSnapshot, FLAG_DELETED, FLAG_STALE, HEARTBEAT_STALE_MS,
        HIVE_FLAG_SELF,
    },
};

type OrchestratorError = Box<dyn std::error::Error + Send + Sync>;

const BOOTSTRAP_SSH_USER: &str = "administrator";
const BOOTSTRAP_SSH_PASS: &str = "magicAI";
const MOTHERBEE_SSH_KEY_PATH: &str = "/var/lib/fluxbee/ssh/motherbee.key";
const PRIMARY_HIVE_ID: &str = "motherbee";
const ORCH_SUDOERS_PATH: &str = "/etc/sudoers.d/fluxbee-orchestrator";
const ORCH_SSH_GATE_PATH: &str = "/usr/local/bin/fluxbee-ssh-gate.sh";
const RUNTIME_VERIFY_INTERVAL_SECS: u64 = 300;
const NATS_BOOTSTRAP_TIMEOUT_SECS: u64 = 20;
const STORAGE_BOOTSTRAP_TIMEOUT_SECS: u64 = 30;
const STORAGE_DB_READINESS_TIMEOUT_SECS: u64 = 30;
const STORAGE_DB_READINESS_REQUEST_TIMEOUT_SECS: u64 = 3;
const SUBJECT_STORAGE_METRICS_GET: &str = "storage.metrics.get";
const MSG_ILK_REGISTER: &str = "ILK_REGISTER";
const MSG_ILK_UPDATE: &str = "ILK_UPDATE";
const SY_NODES_BOOTSTRAP_TIMEOUT_SECS: u64 = 60;
const ADD_HIVE_SOCKET_READY_PROBE_TIMEOUT_SECS: u64 = 10;
const SYNCTHING_SERVICE_NAME: &str = "fluxbee-syncthing";
const SYNCTHING_BOOTSTRAP_TIMEOUT_SECS: u64 = 30;
const SYNCTHING_HEALTH_TIMEOUT_SECS: u64 = 2;
const SYNCTHING_INSTALL_USER: &str = "fluxbee";
const SYNCTHING_SYNC_PORT_TCP: u16 = 22000;
const SYNCTHING_SYNC_PORT_UDP: u16 = 22000;
const SYNCTHING_DISCOVERY_PORT_UDP: u16 = 21027;
const DIST_SYNC_PROBE_TIMEOUT_SECS: u64 = 45;
const REMOVE_HIVE_SOCKET_CLEANUP_TIMEOUT_SECS: u64 = 8;
const SSH_HARDEN_VERIFY_RETRIES: usize = 6;
const SSH_HARDEN_VERIFY_DELAY_MS: u64 = 1000;
const SYNCTHING_FOLDER_BLOB_ID: &str = "fluxbee-blob";
const SYNCTHING_FOLDER_DIST_ID: &str = "fluxbee-dist";
const SYSTEM_SYNC_HINT_DEFAULT_TIMEOUT_MS: u64 = 30_000;
const SYSTEM_SYNC_HINT_MAX_TIMEOUT_MS: u64 = 300_000;
const SYSTEM_SYNC_HINT_POLL_INTERVAL_MS: u64 = 250;
const SYNCTHING_INSTALL_PATH: &str = "/var/lib/fluxbee/vendor/bin/syncthing";
const DIST_ROOT_DIR: &str = "/var/lib/fluxbee/dist";
const DIST_RUNTIME_ROOT_DIR: &str = "/var/lib/fluxbee/dist/runtimes";
const DIST_RUNTIME_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/runtimes/manifest.json";
const DIST_CORE_BIN_SOURCE_DIR: &str = "/var/lib/fluxbee/dist/core/bin";
const DIST_CORE_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/core/manifest.json";
const DIST_VENDOR_ROOT_DIR: &str = "/var/lib/fluxbee/dist/vendor";
const DIST_VENDOR_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/vendor/manifest.json";
const DIST_SYNCTHING_VENDOR_SOURCE_PATH: &str = "/var/lib/fluxbee/dist/vendor/syncthing/syncthing";
const CORE_SERVICE_HEALTH_TIMEOUT_SECS: u64 = 30;
const NODE_STATUS_SCHEMA_VERSION: &str = "1";
const NODE_STATUS_FORWARD_TIMEOUT_SECS: u64 = 2;
const DEPLOYMENT_HISTORY_MAX_LIMIT: usize = 500;
const DRIFT_ALERT_MAX_LIMIT: usize = 500;
const CORE_SYNC_RESTART_ORDER: &[&str] = &[
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-identity",
    "sy-admin",
    "sy-architect",
    "sy-storage",
    "ai-frontdesk-gov",
    "sy-orchestrator",
];
const WORKER_MIN_CORE_COMPONENTS: [&str; 5] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-identity",
    "sy-orchestrator",
];
const WORKER_BOOTSTRAP_CORE_COMPONENTS: [&str; 5] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-identity",
    "sy-orchestrator",
];
const DEFAULT_BLOB_ENABLED: bool = true;
const DEFAULT_BLOB_PATH: &str = "/var/lib/fluxbee/blob";
const DEFAULT_BLOB_SYNC_ENABLED: bool = false;
const DEFAULT_BLOB_SYNC_TOOL: &str = "syncthing";
const DEFAULT_BLOB_SYNC_API_PORT: u16 = 8384;
const DEFAULT_BLOB_SYNC_DATA_DIR: &str = "/var/lib/fluxbee/syncthing";
const DEFAULT_BLOB_SYNC_SERVICE_USER: &str = SYNCTHING_INSTALL_USER;
const DEFAULT_BLOB_SYNC_ALLOW_ROOT_FALLBACK: bool = true;
const DEFAULT_BLOB_GC_ENABLED: bool = false;
const DEFAULT_BLOB_GC_INTERVAL_SECS: u64 = 3600;
const DEFAULT_BLOB_GC_APPLY: bool = false;
const DEFAULT_BLOB_GC_STAGING_TTL_HOURS: u64 = BLOB_STAGING_TTL_HOURS;
const DEFAULT_BLOB_GC_ACTIVE_RETAIN_DAYS: u64 = BLOB_ACTIVE_RETAIN_DAYS;
const DEFAULT_DIST_PATH: &str = DIST_ROOT_DIR;
const DEFAULT_DIST_SYNC_ENABLED: bool = true;
const DEFAULT_DIST_SYNC_TOOL: &str = "syncthing";
const DEFAULT_IDENTITY_SYNC_PORT: u16 = 9100;
const IDENTITY_NODE_ILK_MAP_FILE: &str = "identity-node-ilk-map.json";
const NODE_FILES_ROOT: &str = "/var/lib/fluxbee/nodes";

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    nats: Option<NatsSection>,
    storage: Option<StorageSection>,
    blob: Option<BlobSection>,
    dist: Option<DistSection>,
    identity: Option<IdentitySection>,
    government: Option<GovernmentSection>,
}

#[derive(Debug, Deserialize)]
struct IdentitySection {
    sync: Option<IdentitySyncSection>,
}

#[derive(Debug, Deserialize)]
struct IdentitySyncSection {
    port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct IdentityFile {
    shm: IdentityShm,
}

#[derive(Debug, Deserialize)]
struct IdentityShm {
    name: String,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    gateway_name: Option<String>,
    listen: Option<String>,
    authorized_hives: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    mode: Option<String>,
    port: Option<u16>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GovernmentSection {
    identity_frontdesk: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlobSection {
    enabled: Option<bool>,
    path: Option<String>,
    sync: Option<BlobSyncSection>,
    gc: Option<BlobGcSection>,
}

#[derive(Debug, Deserialize)]
struct BlobSyncSection {
    enabled: Option<bool>,
    tool: Option<String>,
    api_port: Option<u16>,
    data_dir: Option<String>,
    service_user: Option<String>,
    allow_root_fallback: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BlobGcSection {
    enabled: Option<bool>,
    interval_secs: Option<u64>,
    apply: Option<bool>,
    staging_ttl_hours: Option<u64>,
    active_retain_days: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DistSection {
    path: Option<String>,
    sync: Option<DistSyncSection>,
}

#[derive(Debug, Deserialize)]
struct DistSyncSection {
    enabled: Option<bool>,
    tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobRuntimeConfig {
    enabled: bool,
    path: PathBuf,
    sync_enabled: bool,
    sync_tool: String,
    sync_api_port: u16,
    sync_data_dir: PathBuf,
    sync_service_user: String,
    sync_allow_root_fallback: bool,
    gc_enabled: bool,
    gc_interval_secs: u64,
    gc_apply: bool,
    gc_staging_ttl_hours: u64,
    gc_active_retain_days: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DistRuntimeConfig {
    path: PathBuf,
    sync_enabled: bool,
    sync_tool: String,
}

#[derive(Debug, Deserialize)]
struct StorageSection {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CoreManifest {
    schema_version: u64,
    components: std::collections::BTreeMap<String, CoreManifestComponent>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct CoreManifestComponent {
    service: String,
    version: String,
    build_id: String,
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct VendorManifest {
    schema_version: u64,
    #[serde(default)]
    version: u64,
    #[serde(default)]
    updated_at: Option<String>,
    components: std::collections::BTreeMap<String, VendorManifestComponent>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct VendorManifestComponent {
    upstream_version: String,
    #[serde(alias = "sha256")]
    hash: String,
    #[serde(default)]
    size: Option<u64>,
    path: String,
}

#[derive(Debug, Deserialize, Default)]
struct RuntimePackageMetadata {
    #[serde(default)]
    config_template: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DeploymentWorkerOutcome {
    hive_id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_hash_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_hash_after: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DeploymentHistoryEntry {
    deployment_id: String,
    category: String,
    trigger: String,
    actor: String,
    started_at: u64,
    finished_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<String>,
    target_hives: Vec<String>,
    result: String,
    workers: Vec<DeploymentWorkerOutcome>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DriftAlertEntry {
    alert_id: String,
    detected_at: u64,
    category: String,
    trigger: String,
    hive_id: String,
    severity: String,
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_hash_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_hash_after: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
struct RuntimeRetentionStats {
    removed_runtime_dirs: u64,
    removed_version_dirs: u64,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct IdentityNodeIlkMap {
    #[serde(default)]
    nodes: HashMap<String, String>,
}

struct OrchestratorState {
    hive_id: String,
    is_motherbee: bool,
    started_at: Instant,
    config_dir: PathBuf,
    state_dir: PathBuf,
    gateway_name: String,
    storage_path: Mutex<String>,
    wan_listen: Option<String>,
    wan_authorized_hives: Vec<String>,
    tracked_nodes: Mutex<HashSet<String>>,
    system_allowed_origins: HashSet<String>,
    runtime_manifest: Mutex<Option<RuntimeManifest>>,
    runtime_lifecycle_lock: Mutex<()>,
    last_runtime_verify: Mutex<Instant>,
    last_blob_gc: Mutex<Instant>,
    nats_endpoint: String,
    identity_sync_port: u16,
    blob: BlobRuntimeConfig,
    dist: DistRuntimeConfig,
    blob_sync_last_desired: Mutex<BlobRuntimeConfig>,
}

#[derive(Debug, Clone)]
struct PersistedManagedNode {
    node_name: String,
    kind: String,
    runtime: String,
    runtime_version: String,
    config_path: PathBuf,
    relaunch_on_boot: bool,
}

const MOTHERBEE_CRITICAL_SERVICES: [&str; 8] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-identity",
    "sy-admin",
    "sy-architect",
    "sy-storage",
    "ai-frontdesk-gov",
];
const WORKER_CRITICAL_SERVICES: [&str; 4] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-identity",
];

#[tokio::main]
async fn main() -> Result<(), OrchestratorError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_orchestrator supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let run_dir = json_router::paths::run_dir();
    let socket_dir = json_router::paths::router_socket_dir();

    let hive = load_hive(&config_dir)?;
    let is_motherbee = is_mother_role(hive.role.as_deref());
    let is_worker = is_worker_role(hive.role.as_deref());
    if !is_motherbee && !is_worker {
        tracing::warn!(
            role = ?hive.role,
            "SY.orchestrator supports only role=motherbee|worker; exiting"
        );
        return Ok(());
    }
    if is_motherbee && hive.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: role=motherbee requires hive_id='{}' (got '{}')",
            PRIMARY_HIVE_ID, hive.hive_id
        )
        .into());
    }
    if is_worker && hive.hive_id == PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: hive_id='{}' is reserved for role=motherbee",
            PRIMARY_HIVE_ID
        )
        .into());
    }
    let gateway_name = hive
        .wan
        .as_ref()
        .and_then(|wan| wan.gateway_name.clone())
        .unwrap_or_else(|| "RT.gateway".to_string());
    let wan_listen = hive.wan.as_ref().and_then(|wan| wan.listen.clone());
    let wan_authorized_hives = hive
        .wan
        .as_ref()
        .and_then(|wan| wan.authorized_hives.clone())
        .unwrap_or_default();
    let nats_endpoint = nats_endpoint_from_hive(&hive);
    let identity_sync_port = identity_sync_port_from_hive(&hive);
    let blob_runtime = blob_runtime_from_hive(&hive);
    let dist_runtime = dist_runtime_from_hive(&hive);
    let storage_path = storage_path_from_hive(&hive);
    let runtime_manifest = load_runtime_manifest();
    let system_allowed_origins = load_system_allowed_origins(&hive.hive_id);
    tracing::info!(allowed = ?system_allowed_origins, "system message origin allowlist loaded");
    let state = OrchestratorState {
        hive_id: hive.hive_id.clone(),
        is_motherbee,
        started_at: Instant::now(),
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        gateway_name,
        storage_path: Mutex::new(storage_path),
        wan_listen,
        wan_authorized_hives,
        tracked_nodes: Mutex::new(HashSet::new()),
        system_allowed_origins,
        runtime_manifest: Mutex::new(runtime_manifest),
        runtime_lifecycle_lock: Mutex::new(()),
        last_runtime_verify: Mutex::new(Instant::now()),
        last_blob_gc: Mutex::new(Instant::now()),
        nats_endpoint,
        identity_sync_port,
        blob: blob_runtime.clone(),
        dist: dist_runtime,
        blob_sync_last_desired: Mutex::new(blob_runtime),
    };
    tracing::info!(
        blob_enabled = state.blob.enabled,
        blob_path = %state.blob.path.display(),
        blob_sync_enabled = state.blob.sync_enabled,
        blob_sync_tool = %state.blob.sync_tool,
        blob_sync_api_port = state.blob.sync_api_port,
        blob_sync_data_dir = %state.blob.sync_data_dir.display(),
        blob_gc_enabled = state.blob.gc_enabled,
        blob_gc_interval_secs = state.blob.gc_interval_secs,
        blob_gc_apply = state.blob.gc_apply,
        blob_gc_staging_ttl_hours = state.blob.gc_staging_ttl_hours,
        blob_gc_active_retain_days = state.blob.gc_active_retain_days,
        dist_path = %state.dist.path.display(),
        dist_sync_enabled = state.dist.sync_enabled,
        dist_sync_tool = %state.dist.sync_tool,
        identity_sync_port = state.identity_sync_port,
        "blob/dist runtime config loaded"
    );
    ensure_dirs(&config_dir, &state_dir, &run_dir, &state.blob, &state.dist)?;
    write_pid(&run_dir)?;

    bootstrap_local(&state, &socket_dir).await?;

    let node_config = NodeConfig {
        name: "SY.orchestrator".to_string(),
        router_socket: socket_dir.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };

    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!("connected to router");
    tracing::info!(hive = %hive.hive_id, "hive ready");
    tracing::info!(
        mode = if state.is_motherbee {
            "motherbee-control-plane"
        } else {
            "worker-agent"
        },
        "orchestrator role mode"
    );

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut watchdog = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = watchdog.tick() => {
                watchdog_tick(&state).await;
            }
            _ = sigterm.recv() => {
                tracing::warn!("SIGTERM received; shutting down");
                shutdown_sequence(&state).await;
                return Ok(());
            }
            _ = sigint.recv() => {
                tracing::warn!("SIGINT received; shutting down");
                shutdown_sequence(&state).await;
                return Ok(());
            }
            msg = receiver.recv() => {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!("recv error: {err} (reconnecting)");
                        let (new_sender, new_receiver) = connect_with_retry(
                            &node_config,
                            Duration::from_secs(1),
                        ).await?;
                        sender = new_sender;
                        receiver = new_receiver;
                        continue;
                    }
                };
                match msg.meta.msg_type.as_str() {
                    "admin" => {
                        if let Err(err) = handle_admin(&sender, &msg, &state).await {
                            tracing::warn!("admin action error: {err}");
                        }
                    }
                    SYSTEM_KIND => {
                        if let Err(err) = handle_system_message(&sender, &msg, &state).await {
                            tracing::warn!("system message error: {err}");
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn bootstrap_local(
    state: &OrchestratorState,
    socket_dir: &Path,
) -> Result<(), OrchestratorError> {
    let core_manifest = load_core_manifest()?;
    let core_bins = core_bin_paths_for_role(&core_manifest, state.is_motherbee)?;
    validate_core_manifest_for_bins(&core_bins)?;

    tracing::info!("starting rt-gateway");
    systemd_start("rt-gateway")?;
    wait_for_router_ready(state, socket_dir, Duration::from_secs(30)).await?;
    wait_for_nats_ready(
        &state.nats_endpoint,
        Duration::from_secs(NATS_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;
    ensure_core_firewall_local(state);
    let startup_sync = effective_syncthing_runtime_config(&state.blob, &state.dist);
    if startup_sync.sync_enabled {
        if let Err(err) = ensure_blob_sync_runtime(&state.blob, &state.dist).await {
            tracing::warn!(
                error = %err,
                "blob sync runtime bootstrap failed; continuing startup and relying on watchdog retries"
            );
        }
    } else {
        disable_blob_sync_runtime_local()?;
    }

    let mut services = if state.is_motherbee {
        vec![
            "sy-config-routes",
            "sy-opa-rules",
            "sy-admin",
            "sy-architect",
            "sy-storage",
            "ai-frontdesk-gov",
        ]
    } else {
        vec!["sy-config-routes", "sy-opa-rules"]
    };
    if identity_available() {
        services.push("sy-identity");
    }
    for service in services {
        tracing::info!(service = service, "starting service");
        if let Err(err) = systemd_start(service) {
            tracing::warn!(service = service, error = %err, "failed to start service");
        }
    }

    if let Err(err) =
        wait_for_sy_nodes(state, Duration::from_secs(SY_NODES_BOOTSTRAP_TIMEOUT_SECS)).await
    {
        tracing::warn!(
            error = %err,
            "sy nodes did not fully bootstrap before timeout; continuing and relying on watchdog restarts"
        );
    }
    if state.is_motherbee {
        wait_for_service_active(
            "sy-storage",
            Duration::from_secs(STORAGE_BOOTSTRAP_TIMEOUT_SECS),
        )
        .await?;
        wait_for_storage_db_ready(
            &state.config_dir,
            Duration::from_secs(STORAGE_DB_READINESS_TIMEOUT_SECS),
        )
        .await?;
    }
    if let Err(err) = reconcile_persisted_custom_nodes(state).await {
        tracing::warn!(
            error = %err,
            "persisted custom node reconcile failed during bootstrap"
        );
    }
    Ok(())
}

async fn wait_for_storage_db_ready(
    config_dir: &Path,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    let started = Instant::now();
    let mut attempts: u64 = 0;
    loop {
        attempts = attempts.saturating_add(1);
        let trace_id = Uuid::new_v4().to_string();
        let reply_subject = format!("storage.metrics.reply.orchestrator.{trace_id}");
        let sid = 40_019;
        let request = NatsRequestEnvelope::<serde_json::Value>::new(
            SUBJECT_STORAGE_METRICS_GET,
            trace_id.clone(),
            reply_subject.clone(),
            None,
        );
        let request_body = serde_json::to_vec(&request)?;

        match request_local(
            config_dir,
            SUBJECT_STORAGE_METRICS_GET,
            &request_body,
            &reply_subject,
            sid,
            Duration::from_secs(STORAGE_DB_READINESS_REQUEST_TIMEOUT_SECS),
        )
        .await
        {
            Ok(body) => {
                match serde_json::from_slice::<NatsResponseEnvelope<serde_json::Value>>(&body) {
                    Ok(response) if response.status == "ok" => {
                        tracing::info!(
                            attempts = attempts,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "sy-storage DB readiness confirmed via storage.metrics.get"
                        );
                        return Ok(());
                    }
                    Ok(response) => {
                        tracing::warn!(
                            attempts = attempts,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            status = %response.status,
                            error_code = ?response.error_code,
                            error_detail = ?response.error_detail,
                            "sy-storage readiness probe returned non-ok status; retrying"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            attempts = attempts,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            error = %err,
                            "sy-storage readiness probe decode failed; retrying"
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    attempts = attempts,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    error = %err,
                    "sy-storage readiness probe failed; retrying"
                );
            }
        }

        if started.elapsed() >= timeout {
            return Err(format!(
                "sy-storage DB readiness timeout after {}s (attempts={})",
                timeout.as_secs(),
                attempts
            )
            .into());
        }
        time::sleep(Duration::from_millis(250)).await;
    }
}

async fn reconcile_persisted_custom_nodes(
    state: &OrchestratorState,
) -> Result<(), OrchestratorError> {
    let manifest = match load_runtime_manifest_result()? {
        Some(manifest) => manifest,
        None => {
            tracing::warn!("runtime manifest missing; skipping persisted custom node reconcile");
            return Ok(());
        }
    };

    let nodes = persisted_custom_nodes(state)?;
    if nodes.is_empty() {
        tracing::info!("no persisted custom node instances found for reconcile");
        return Ok(());
    }

    let mut started = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for node in nodes {
        let unit = unit_from_node_name(&node.node_name);
        let unit_active = systemd_unit_is_active(&unit).unwrap_or(false);
        let visible_in_router = local_inventory_has_node(state, &node.node_name);
        if unit_active || visible_in_router {
            skipped = skipped.saturating_add(1);
            tracing::info!(
                node_name = node.node_name,
                runtime = node.runtime,
                runtime_version = node.runtime_version,
                unit = unit,
                unit_active = unit_active,
                visible_in_router = visible_in_router,
                "skipping persisted custom node reconcile; instance already active/visible"
            );
            continue;
        }

        let runtime_key = match resolve_runtime_key(&manifest, &node.runtime) {
            Ok(value) => value,
            Err(err) => {
                failed = failed.saturating_add(1);
                tracing::warn!(
                    node_name = node.node_name,
                    runtime = node.runtime,
                    runtime_version = node.runtime_version,
                    error = %err,
                    "unable to resolve runtime for persisted custom node reconcile"
                );
                continue;
            }
        };
        let version = match resolve_runtime_version(&manifest, &runtime_key, &node.runtime_version)
        {
            Ok(value) => value,
            Err(err) => {
                failed = failed.saturating_add(1);
                tracing::warn!(
                    node_name = node.node_name,
                    runtime = runtime_key,
                    runtime_version = node.runtime_version,
                    error = %err,
                    "unable to resolve runtime version for persisted custom node reconcile"
                );
                continue;
            }
        };
        let entrypoint = match resolve_runtime_spawn_entrypoint(&manifest, &runtime_key, &version) {
            Ok(value) => value,
            Err(err) => {
                failed = failed.saturating_add(1);
                tracing::warn!(
                    node_name = node.node_name,
                    runtime = runtime_key,
                    runtime_version = version,
                    error_code = err.error_code(),
                    error = err.message(),
                    "unable to resolve entrypoint for persisted custom node reconcile"
                );
                continue;
            }
        };

        let preflight_error = if let Some(runtime_base) = entrypoint.runtime_base.as_deref() {
            runtime_base_start_script_preflight(
                &runtime_key,
                &version,
                runtime_base,
                &entrypoint.script_version,
                &entrypoint.script_path,
                &state.hive_id,
                &node.node_name,
            )
            .err()
        } else {
            runtime_start_script_preflight(
                &entrypoint.script_runtime,
                &entrypoint.script_version,
                &entrypoint.script_path,
                &state.hive_id,
                &node.node_name,
            )
            .err()
        };
        if let Some(payload) = preflight_error {
            failed = failed.saturating_add(1);
            tracing::warn!(
                node_name = node.node_name,
                runtime = runtime_key,
                runtime_version = version,
                payload = %payload,
                "preflight failed for persisted custom node reconcile"
            );
            continue;
        }

        let cmd = build_managed_node_run_command(&unit, &node.node_name, &entrypoint.script_path);
        match execute_on_hive(
            state,
            &state.hive_id,
            &cmd,
            "reconcile_persisted_custom_nodes",
        ) {
            Ok(()) => {
                started = started.saturating_add(1);
                tracing::info!(
                    node_name = node.node_name,
                    kind = node.kind,
                    runtime = runtime_key,
                    runtime_version = version,
                    relaunch_on_boot = node.relaunch_on_boot,
                    config_path = %node.config_path.display(),
                    unit = unit,
                    "persisted custom node relaunched during bootstrap reconcile"
                );
            }
            Err(err) => {
                failed = failed.saturating_add(1);
                tracing::warn!(
                    node_name = node.node_name,
                    runtime = runtime_key,
                    runtime_version = version,
                    unit = unit,
                    error = %err,
                    "failed to relaunch persisted custom node during bootstrap reconcile"
                );
            }
        }
    }

    tracing::info!(
        started = started,
        skipped = skipped,
        failed = failed,
        "persisted custom node reconcile completed"
    );
    Ok(())
}

async fn wait_for_router_ready(
    state: &OrchestratorState,
    socket_dir: &Path,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    const ROUTER_SHM_MAX_STALE_MS: u64 = 30_000;

    let start = Instant::now();
    loop {
        let rt_gateway_active = systemd_is_active("rt-gateway");

        let mut socket_ready = false;
        let mut shm_ready = false;
        let mut socket_path = String::from("unknown");
        let mut heartbeat_age_ms = u64::MAX;

        if let Ok(snapshot) = load_router_snapshot(state) {
            let expected_socket =
                socket_dir.join(format!("{}.sock", snapshot.header.router_uuid.simple()));
            socket_path = expected_socket.display().to_string();
            // If rt-gateway is already running when orchestrator restarts, socket mtime may be old.
            // Readiness should depend on socket presence + fresh SHM heartbeat, not file age.
            socket_ready = expected_socket.exists();

            heartbeat_age_ms = now_epoch_ms().saturating_sub(snapshot.header.heartbeat);
            shm_ready = heartbeat_age_ms <= ROUTER_SHM_MAX_STALE_MS;
        }

        if rt_gateway_active && shm_ready {
            if !socket_ready {
                tracing::warn!(
                    socket = %socket_path,
                    "router heartbeat is fresh but socket path is not visible; continuing with SHM readiness"
                );
            }
            tracing::info!(
                socket = %socket_path,
                heartbeat_age_ms,
                "router socket + shm ready"
            );
            return Ok(());
        }

        if start.elapsed() >= timeout {
            return Err(format!(
                "router bootstrap timeout rt_gateway_active={} socket_ready={} shm_ready={} socket={} heartbeat_age_ms={}",
                rt_gateway_active, socket_ready, shm_ready, socket_path, heartbeat_age_ms
            )
            .into());
        }

        time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_sy_nodes(
    state: &OrchestratorState,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    // Only router-connected SY nodes are visible in router SHM.
    let mut required = vec!["SY.config.routes", "SY.opa.rules"];
    if state.is_motherbee {
        required.push("SY.admin");
    }
    if identity_available() {
        required.push("SY.identity");
    }
    let start = Instant::now();
    let mut last_missing: Vec<String> = required.iter().map(|name| (*name).to_string()).collect();
    loop {
        if let Ok(snapshot) = load_router_snapshot(state) {
            let mut missing = Vec::new();
            for name in required.iter().copied() {
                let expected = ensure_l2_name(name, &state.hive_id);
                let found = snapshot.nodes.iter().any(|node| {
                    if node.name_len == 0 {
                        return false;
                    }
                    let node_name = node_name(node);
                    node_name == name || node_name == expected
                });
                if !found {
                    missing.push(name.to_string());
                }
            }
            if missing.is_empty() {
                tracing::info!("sy nodes connected");
                return Ok(());
            }
            last_missing = missing;
        }
        if start.elapsed() >= timeout {
            tracing::error!(missing = ?last_missing, "sy nodes bootstrap timeout");
            return Err(format!(
                "sy nodes bootstrap timeout (missing: {})",
                last_missing.join(", ")
            )
            .into());
        }
        time::sleep(Duration::from_millis(250)).await;
    }
}

async fn watchdog_tick(state: &OrchestratorState) {
    let services: &[&str] = if state.is_motherbee {
        &MOTHERBEE_CRITICAL_SERVICES
    } else {
        &WORKER_CRITICAL_SERVICES
    };
    for service in services {
        if !systemd_is_active(service) {
            tracing::warn!(service = service, "service not active; attempting restart");
            if let Err(err) = systemd_start(service) {
                tracing::warn!(service = service, error = %err, "service restart failed");
            }
        }
    }
    if identity_available() && !systemd_is_active("sy-identity") {
        tracing::warn!(
            service = "sy-identity",
            "service not active; attempting restart"
        );
        if let Err(err) = systemd_start("sy-identity") {
            tracing::warn!(
                service = "sy-identity",
                error = %err,
                "service restart failed"
            );
        }
    }

    if let Ok(snapshot) = load_router_snapshot(state) {
        let mut current = HashSet::new();
        for node in &snapshot.nodes {
            if node.name_len == 0 {
                continue;
            }
            let name = node_name(node);
            if name.starts_with("AI.") || name.starts_with("WF.") || name.starts_with("IO.") {
                current.insert(name);
            }
        }
        let mut tracked = state.tracked_nodes.lock().await;
        for missing in tracked.difference(&current) {
            tracing::warn!(node = missing.as_str(), "node disconnected");
        }
        *tracked = current;
    }

    if should_verify_runtimes(state).await {
        if let Err(err) = runtime_verify_and_retain(state).await {
            tracing::warn!(error = %err, "runtime verify/sync failed");
        }
    }

    let desired_blob = current_blob_runtime_config(state);
    if should_run_blob_gc(
        state,
        desired_blob.gc_enabled,
        desired_blob.gc_interval_secs,
    )
    .await
    {
        if let Err(err) = run_blob_gc_housekeeping(state, &desired_blob) {
            tracing::warn!(error = %err, "blob gc housekeeping failed");
        }
    }

    if let Err(err) = watchdog_blob_sync(state).await {
        tracing::warn!(error = %err, "blob sync watchdog failed");
    }
}

async fn shutdown_sequence(state: &OrchestratorState) {
    let tracked = state.tracked_nodes.lock().await;
    for node in tracked.iter() {
        tracing::warn!(
            node = node.as_str(),
            "shutdown pending; node still connected"
        );
    }
    drop(tracked);

    time::sleep(Duration::from_secs(10)).await;

    for service in [
        "ai-frontdesk-gov",
        "sy-storage",
        "sy-architect",
        "sy-admin",
        "sy-config-routes",
        "sy-opa-rules",
    ] {
        if let Err(err) = systemd_stop(service) {
            tracing::warn!(service = service, error = %err, "failed to stop service");
        }
    }
    if identity_available() {
        if let Err(err) = systemd_stop("sy-identity") {
            tracing::warn!(
                service = "sy-identity",
                error = %err,
                "failed to stop service"
            );
        }
    }
    if let Err(err) = systemd_stop("rt-gateway") {
        tracing::warn!(service = "rt-gateway", error = %err, "failed to stop service");
    }
    if systemd_is_active(SYNCTHING_SERVICE_NAME) {
        if let Err(err) = systemd_stop(SYNCTHING_SERVICE_NAME) {
            tracing::warn!(
                service = SYNCTHING_SERVICE_NAME,
                error = %err,
                "failed to stop service"
            );
        }
    }
}

async fn send_admin_forbidden(
    sender: &NodeSender,
    msg: &Message,
    action: &str,
    reason: &str,
) -> Result<(), OrchestratorError> {
    let payload = serde_json::json!({
        "status": "error",
        "error_code": "FORBIDDEN",
        "message": reason,
    });
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(msg.routing.src.clone()),
            ttl: 16,
            trace_id: msg.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: None,
            src_ilk: None,
            scope: None,
            target: None,
            action: Some(action.to_string()),
            priority: None,
            context: None,
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

async fn handle_admin(
    sender: &NodeSender,
    msg: &Message,
    state: &OrchestratorState,
) -> Result<(), OrchestratorError> {
    let action = msg.meta.action.as_deref().unwrap_or("");
    tracing::info!(action = action, trace_id = %msg.routing.trace_id, "admin action received");
    let payload = match action {
        "hive_status" => {
            let uptime_ms = state.started_at.elapsed().as_millis() as u64;
            serde_json::json!({
                "status": "ok",
                "hive_id": state.hive_id,
                "pid": std::process::id(),
                "uptime_ms": uptime_ms,
            })
        }
        "get_storage" => {
            let path = state.storage_path.lock().await.clone();
            serde_json::json!({
                "status": "ok",
                "path": path,
            })
        }
        "set_storage" => {
            let path = msg
                .payload
                .get("path")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let payload = if let Some(path) = path {
                match persist_storage_path_in_hive(&state.config_dir, &path) {
                    Ok(()) => serde_json::json!({
                        "status": "ok",
                        "path": path,
                    }),
                    Err(err) => serde_json::json!({
                        "status": "error",
                        "error_code": "PERSIST_FAILED",
                        "message": err.to_string(),
                    }),
                }
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing path",
                })
            };
            let status = payload
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("error");
            if status == "ok" {
                if let Some(path) = payload.get("path").and_then(|value| value.as_str()) {
                    let mut guard = state.storage_path.lock().await;
                    *guard = path.to_string();
                }
            }
            let error_code = payload
                .get("error_code")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let error_detail = payload
                .get("message")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let version = msg
                .payload
                .get("version")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let _ = send_config_response(
                sender,
                msg,
                "storage",
                version,
                status,
                error_code,
                error_detail,
                &state.hive_id,
            )
            .await;
            payload
        }
        "list_nodes" => list_nodes_flow(state, &msg.payload).await,
        "list_versions" => list_versions_flow(state).await,
        "get_versions" => get_versions_flow(state, &msg.payload).await,
        "list_runtimes" => list_runtimes_flow(state, &msg.payload).await,
        "get_runtime" => get_runtime_flow(state, &msg.payload).await,
        "remove_runtime_version" => remove_runtime_version_flow(state, &msg.payload).await,
        "remove_node_instance" => remove_node_instance_flow(state, &msg.payload).await,
        "list_deployments" => list_deployments_flow(state, &msg.payload),
        "get_deployments" => get_deployments_flow(state, &msg.payload),
        "list_drift_alerts" => list_drift_alerts_flow(state, &msg.payload),
        "get_drift_alerts" => get_drift_alerts_flow(state, &msg.payload),
        "get_node_config" => get_node_config_flow(state, &msg.payload).await,
        "get_node_state" => get_node_state_flow(state, &msg.payload).await,
        "get_node_status" => get_node_status_flow(state, &msg.payload).await,
        "set_node_config" => set_node_config_flow(sender, state, &msg.payload).await,
        "run_node" => run_node_flow(state, &msg.payload).await,
        "kill_node" => kill_node_flow(state, &msg.payload).await,
        "list_hives" => {
            serde_json::json!({
                "status": "ok",
                "hives": list_hives(state)?,
            })
        }
        "get_hive" => {
            let hive = msg
                .payload
                .get("hive_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(hive_id) = hive {
                match get_hive(&state.state_dir, &hive_id) {
                    Ok(payload) => serde_json::json!({
                        "status": "ok",
                        "hive": payload,
                    }),
                    Err(err) => serde_json::json!({
                        "status": "error",
                        "error_code": "NOT_FOUND",
                        "message": err.to_string(),
                    }),
                }
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing hive_id",
                })
            }
        }
        "remove_hive" => {
            if !state.is_motherbee {
                return send_admin_forbidden(
                    sender,
                    msg,
                    action,
                    "add_hive/remove_hive are motherbee-only",
                )
                .await;
            }
            let hive = msg
                .payload
                .get("hive_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(hive_id) = hive {
                remove_hive_flow(state, &hive_id).await
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing hive_id",
                })
            }
        }
        "add_hive" => {
            if !state.is_motherbee {
                return send_admin_forbidden(
                    sender,
                    msg,
                    action,
                    "add_hive/remove_hive are motherbee-only",
                )
                .await;
            }
            let hive_id = msg
                .payload
                .get("hive_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let address = msg
                .payload
                .get("address")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(hive_id) = hive_id {
                let address = address.unwrap_or_default();
                let harden_ssh = resolve_add_hive_harden_ssh(&msg.payload);
                let restrict_ssh = resolve_add_hive_restrict_ssh(&msg.payload, harden_ssh);
                let require_dist_sync = resolve_add_hive_require_dist_sync(&msg.payload);
                let dist_sync_probe_timeout_secs =
                    resolve_add_hive_dist_sync_probe_timeout_secs(&msg.payload);
                add_hive_flow(
                    state,
                    &hive_id,
                    &address,
                    harden_ssh,
                    restrict_ssh,
                    require_dist_sync,
                    dist_sync_probe_timeout_secs,
                )
                .await
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing hive_id",
                })
            }
        }
        _ => serde_json::json!({
            "status": "error",
            "error_code": "NOT_IMPLEMENTED",
            "message": format!("action '{action}' not implemented"),
        }),
    };

    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(msg.routing.src.clone()),
            ttl: 16,
            trace_id: msg.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: None,
            src_ilk: None,
            scope: None,
            target: None,
            action: Some(action.to_string()),
            priority: None,
            context: None,
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

async fn handle_system_message(
    sender: &NodeSender,
    msg: &Message,
    state: &OrchestratorState,
) -> Result<(), OrchestratorError> {
    let action = msg.meta.msg.as_deref().unwrap_or_default();
    if matches!(
        action,
        "SYSTEM_UPDATE"
            | "SYSTEM_SYNC_HINT"
            | "SPAWN_NODE"
            | "KILL_NODE"
            | "REMOVE_NODE_INSTANCE"
            | "NODE_CONFIG_SET"
            | "NODE_CONFIG_GET"
            | "NODE_STATE_GET"
            | "NODE_STATUS_GET"
            | "GET_VERSIONS"
            | "INVENTORY_REQUEST"
            | "ADD_HIVE_FINALIZE"
            | "REMOVE_HIVE_CLEANUP"
    ) {
        let source_name = resolve_system_source_name_with_retry(state, &msg.routing.src).await;
        let is_allowed = source_name.as_deref().is_some_and(|name| {
            state.system_allowed_origins.contains(name)
                || name.starts_with("SY.orchestrator@")
                || name.starts_with("SY.orchestrator.")
                || name.starts_with("SY.admin@")
                || name.starts_with("WF.orch.diag@")
        });
        if !is_allowed {
            tracing::warn!(
                action = action,
                source_uuid = %msg.routing.src,
                source_name = ?source_name,
                allowed = ?state.system_allowed_origins,
                "blocked system message from unauthorized origin"
            );
            let payload = serde_json::json!({
                "status": "error",
                "error_code": "FORBIDDEN",
                "message": "system action origin not allowed",
                "source_uuid": msg.routing.src,
                "source_name": source_name,
            });
            match action {
                "SYSTEM_UPDATE" => {
                    let _ =
                        send_system_action_response(sender, msg, "SYSTEM_UPDATE_RESPONSE", payload)
                            .await;
                }
                "SYSTEM_SYNC_HINT" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "SYSTEM_SYNC_HINT_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "SPAWN_NODE" => {
                    let _ =
                        send_system_action_response(sender, msg, "SPAWN_NODE_RESPONSE", payload)
                            .await;
                }
                "KILL_NODE" => {
                    let _ = send_system_action_response(sender, msg, "KILL_NODE_RESPONSE", payload)
                        .await;
                }
                "REMOVE_NODE_INSTANCE" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "REMOVE_NODE_INSTANCE_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "NODE_CONFIG_SET" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "NODE_CONFIG_SET_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "NODE_CONFIG_GET" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "NODE_CONFIG_GET_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "NODE_STATE_GET" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "NODE_STATE_GET_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "NODE_STATUS_GET" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "NODE_STATUS_GET_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "GET_VERSIONS" => {
                    let _ =
                        send_system_action_response(sender, msg, "GET_VERSIONS_RESPONSE", payload)
                            .await;
                }
                "INVENTORY_REQUEST" => {
                    let _ = send_system_action_response(sender, msg, "INVENTORY_RESPONSE", payload)
                        .await;
                }
                "ADD_HIVE_FINALIZE" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "ADD_HIVE_FINALIZE_RESPONSE",
                        payload,
                    )
                    .await;
                }
                "REMOVE_HIVE_CLEANUP" => {
                    let _ = send_system_action_response(
                        sender,
                        msg,
                        "REMOVE_HIVE_CLEANUP_RESPONSE",
                        payload,
                    )
                    .await;
                }
                _ => {}
            }
            return Ok(());
        }
    }

    match msg.meta.msg.as_deref() {
        Some("RUNTIME_UPDATE") => {
            let payload = serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": "action 'RUNTIME_UPDATE' is not supported; use 'SYSTEM_UPDATE'",
            });
            let _ =
                send_system_action_response(sender, msg, "RUNTIME_UPDATE_RESPONSE", payload).await;
        }
        Some("SYSTEM_UPDATE") => {
            let result = handle_system_update_message(state, msg).await;
            let _ =
                send_system_action_response(sender, msg, "SYSTEM_UPDATE_RESPONSE", result).await;
        }
        Some("SYSTEM_SYNC_HINT") => {
            let result = handle_system_sync_hint_message(state, msg).await;
            let _ =
                send_system_action_response(sender, msg, "SYSTEM_SYNC_HINT_RESPONSE", result).await;
        }
        Some("SPAWN_NODE") => {
            let result = run_node_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "SPAWN_NODE processed");
            let _ = send_system_action_response(sender, msg, "SPAWN_NODE_RESPONSE", result).await;
        }
        Some("KILL_NODE") => {
            let result = kill_node_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "KILL_NODE processed");
            let _ = send_system_action_response(sender, msg, "KILL_NODE_RESPONSE", result).await;
        }
        Some("REMOVE_NODE_INSTANCE") => {
            let result = remove_node_instance_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "REMOVE_NODE_INSTANCE processed");
            let _ =
                send_system_action_response(sender, msg, "REMOVE_NODE_INSTANCE_RESPONSE", result)
                    .await;
        }
        Some("NODE_CONFIG_SET") => {
            let result = set_node_config_flow(sender, state, &msg.payload).await;
            tracing::info!(result = %result, "NODE_CONFIG_SET processed");
            let _ =
                send_system_action_response(sender, msg, "NODE_CONFIG_SET_RESPONSE", result).await;
        }
        Some("NODE_CONFIG_GET") => {
            let result = get_node_config_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "NODE_CONFIG_GET processed");
            let _ =
                send_system_action_response(sender, msg, "NODE_CONFIG_GET_RESPONSE", result).await;
        }
        Some("NODE_STATE_GET") => {
            let result = get_node_state_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "NODE_STATE_GET processed");
            let _ =
                send_system_action_response(sender, msg, "NODE_STATE_GET_RESPONSE", result).await;
        }
        Some("NODE_STATUS_GET") => {
            let result = get_node_status_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "NODE_STATUS_GET processed");
            let _ =
                send_system_action_response(sender, msg, "NODE_STATUS_GET_RESPONSE", result).await;
        }
        Some("GET_VERSIONS") => {
            let result = get_versions_flow(state, &msg.payload).await;
            let _ = send_system_action_response(sender, msg, "GET_VERSIONS_RESPONSE", result).await;
        }
        Some("GET_RUNTIMES") => {
            let result = list_runtimes_flow(state, &msg.payload).await;
            let _ = send_system_action_response(sender, msg, "GET_RUNTIMES_RESPONSE", result).await;
        }
        Some("LIST_NODES") => {
            let result = list_nodes_flow(state, &msg.payload).await;
            let _ = send_system_action_response(sender, msg, "LIST_NODES_RESPONSE", result).await;
        }
        Some("GET_RUNTIME") => {
            let result = get_runtime_flow(state, &msg.payload).await;
            let _ = send_system_action_response(sender, msg, "GET_RUNTIME_RESPONSE", result).await;
        }
        Some("REMOVE_RUNTIME_VERSION") => {
            let result = remove_runtime_version_flow(state, &msg.payload).await;
            let _ =
                send_system_action_response(sender, msg, "REMOVE_RUNTIME_VERSION_RESPONSE", result)
                    .await;
        }
        Some("INVENTORY_REQUEST") => {
            let result = inventory_flow(state, &msg.payload);
            let _ = send_system_action_response(sender, msg, "INVENTORY_RESPONSE", result).await;
        }
        Some("ADD_HIVE_FINALIZE") => {
            let result = add_hive_finalize_local_flow(state, &msg.payload).await;
            tracing::info!(result = %result, "ADD_HIVE_FINALIZE processed");
            let _ = send_system_action_response(sender, msg, "ADD_HIVE_FINALIZE_RESPONSE", result)
                .await;
        }
        Some("REMOVE_HIVE_CLEANUP") => {
            let result = remove_hive_cleanup_local_flow();
            tracing::info!(result = %result, "REMOVE_HIVE_CLEANUP processed");
            let _ =
                send_system_action_response(sender, msg, "REMOVE_HIVE_CLEANUP_RESPONSE", result)
                    .await;
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct LocalSystemManifestState {
    version: Option<u64>,
    hash: Option<String>,
}

fn local_system_manifest_state(
    category: &str,
) -> Result<LocalSystemManifestState, OrchestratorError> {
    match category {
        "runtime" => Ok(LocalSystemManifestState {
            version: load_runtime_manifest_result()?.map(|manifest| manifest.version),
            hash: local_runtime_manifest_hash()?,
        }),
        "core" => Ok(LocalSystemManifestState {
            version: None,
            hash: local_core_manifest_hash()?,
        }),
        "vendor" => Ok(LocalSystemManifestState {
            version: load_vendor_manifest()?.map(|manifest| manifest.version),
            hash: local_syncthing_vendor_hash()?,
        }),
        _ => Err(format!("unknown system update category '{}'", category).into()),
    }
}

fn parse_system_update_payload(
    payload: &serde_json::Value,
) -> Result<(String, u64, String), OrchestratorError> {
    if payload.get("version").is_some() {
        return Err("legacy field 'version' is not supported; use 'manifest_version'".into());
    }
    if payload.get("hash").is_some() {
        return Err("legacy field 'hash' is not supported; use 'manifest_hash'".into());
    }

    let category = payload
        .get("category")
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "runtime".to_string());
    if !matches!(category.as_str(), "runtime" | "core" | "vendor") {
        return Err("category must be one of runtime/core/vendor".into());
    }

    let manifest_version = payload
        .get("manifest_version")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    let manifest_hash = payload
        .get("manifest_hash")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("missing manifest_hash")?
        .to_string();

    Ok((category, manifest_version, manifest_hash))
}

#[derive(Debug, Clone)]
struct SystemSyncHintRequest {
    channel: String,
    folder_id: String,
    wait_for_idle: bool,
    timeout_ms: u64,
}

fn default_folder_id_for_sync_channel(channel: &str) -> Option<&'static str> {
    match channel {
        "blob" => Some(SYNCTHING_FOLDER_BLOB_ID),
        "dist" => Some(SYNCTHING_FOLDER_DIST_ID),
        _ => None,
    }
}

fn parse_system_sync_hint_payload(
    payload: &serde_json::Value,
) -> Result<SystemSyncHintRequest, OrchestratorError> {
    let channel = payload
        .get("channel")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("blob")
        .to_ascii_lowercase();

    let default_folder_id = default_folder_id_for_sync_channel(&channel)
        .ok_or("channel must be one of blob/dist")?
        .to_string();

    let folder_id = payload
        .get("folder_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_folder_id.as_str())
        .to_string();

    if folder_id != default_folder_id {
        return Err(format!(
            "folder_id '{}' is invalid for channel '{}'; expected '{}'",
            folder_id, channel, default_folder_id
        )
        .into());
    }

    let wait_for_idle = match payload.get("wait_for_idle") {
        None => true,
        Some(value) => value.as_bool().ok_or("wait_for_idle must be boolean")?,
    };

    let timeout_ms = match payload.get("timeout_ms") {
        None => SYSTEM_SYNC_HINT_DEFAULT_TIMEOUT_MS,
        Some(value) => value
            .as_u64()
            .filter(|v| *v > 0)
            .ok_or("timeout_ms must be a positive unsigned integer")?,
    }
    .min(SYSTEM_SYNC_HINT_MAX_TIMEOUT_MS);

    Ok(SystemSyncHintRequest {
        channel,
        folder_id,
        wait_for_idle,
        timeout_ms,
    })
}

fn system_sync_hint_response_json(
    state: &OrchestratorState,
    request: &SystemSyncHintRequest,
    status: &str,
    api_healthy: bool,
    folder_status: Option<&SyncthingFolderStatus>,
    errors: Vec<String>,
    message: Option<String>,
) -> serde_json::Value {
    let folder_healthy = folder_status.is_some_and(SyncthingFolderStatus::is_healthy);
    let state_text = folder_status
        .map(|value| value.state.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let need_total_items = folder_status
        .map(|value| value.need_total_items)
        .unwrap_or(0);
    let mut payload = serde_json::json!({
        "status": status,
        "hive": state.hive_id.as_str(),
        "channel": request.channel,
        "folder_id": request.folder_id,
        "wait_for_idle": request.wait_for_idle,
        "timeout_ms": request.timeout_ms,
        "api_healthy": api_healthy,
        "folder_healthy": folder_healthy,
        "state": state_text,
        "need_total_items": need_total_items,
        "errors": errors,
    });
    if let Some(detail) = message {
        payload["message"] = serde_json::Value::String(detail);
    }
    payload
}

async fn handle_system_sync_hint_message(
    state: &OrchestratorState,
    msg: &Message,
) -> serde_json::Value {
    let request = match parse_system_sync_hint_payload(&msg.payload) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    let desired_blob = current_blob_runtime_config(state);
    let desired_dist = current_dist_runtime_config(state);
    let desired_sync = effective_syncthing_runtime_config(&desired_blob, &desired_dist);
    let channel_sync_enabled = match request.channel.as_str() {
        "blob" => desired_blob.sync_enabled && blob_sync_tool_is_syncthing(&desired_blob),
        "dist" => desired_dist.sync_enabled && dist_sync_tool_is_syncthing(&desired_dist),
        _ => false,
    };
    if !desired_sync.sync_enabled || !channel_sync_enabled {
        return serde_json::json!({
            "status": "error",
            "error_code": "SYNC_DISABLED",
            "message": format!("sync channel '{}' is not enabled in hive configuration", request.channel),
            "channel": request.channel,
            "folder_id": request.folder_id,
        });
    }
    if !(blob_sync_tool_is_syncthing(&desired_sync) || dist_sync_tool_is_syncthing(&desired_dist)) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SYNC_UNSUPPORTED",
            "message": format!(
                "unsupported sync.tool for blob/dist (blob='{}', dist='{}'; expected syncthing)",
                desired_blob.sync_tool, desired_dist.sync_tool
            ),
            "channel": request.channel,
            "folder_id": request.folder_id,
        });
    }

    let mut service_active = systemd_is_active(SYNCTHING_SERVICE_NAME);
    let mut api_healthy = if service_active {
        syncthing_api_healthy(desired_sync.sync_api_port).await
    } else {
        false
    };

    if !service_active || !api_healthy {
        if let Err(err) = ensure_blob_sync_runtime(&desired_blob, &desired_dist).await {
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_UNHEALTHY",
                "message": format!("failed to reconcile syncthing runtime: {err}"),
                "channel": request.channel,
                "folder_id": request.folder_id,
            });
        }
        service_active = systemd_is_active(SYNCTHING_SERVICE_NAME);
        api_healthy = if service_active {
            syncthing_api_healthy(desired_sync.sync_api_port).await
        } else {
            false
        };
    }

    if !service_active || !api_healthy {
        return serde_json::json!({
            "status": "error",
            "error_code": "SYNC_UNHEALTHY",
            "message": "syncthing service is not healthy",
            "channel": request.channel,
            "folder_id": request.folder_id,
            "api_healthy": api_healthy,
            "service_active": service_active,
        });
    }

    let mut warnings = Vec::new();
    if let Err(err) = syncthing_trigger_folder_scan(&desired_sync, &request.folder_id) {
        tracing::warn!(
            channel = %request.channel,
            folder = %request.folder_id,
            error = %err,
            "sync hint scan trigger failed; continuing with status probe"
        );
        warnings.push(format!("scan trigger failed: {err}"));
    }

    let mut folder_status = match syncthing_folder_status(&desired_sync, &request.folder_id) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_STATUS_FAILED",
                "message": err.to_string(),
                "channel": request.channel,
                "folder_id": request.folder_id,
                "api_healthy": api_healthy,
                "errors": warnings,
            });
        }
    };

    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
    loop {
        if !folder_status.is_healthy() {
            let mut errors = warnings.clone();
            if !folder_status.error.is_empty() {
                errors.push(format!("folder error: {}", folder_status.error));
            }
            if !folder_status.invalid.is_empty() {
                errors.push(format!("folder invalid: {}", folder_status.invalid));
            }
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_UNHEALTHY",
                "message": "syncthing folder is unhealthy",
                "hive": state.hive_id.as_str(),
                "channel": request.channel,
                "folder_id": request.folder_id,
                "api_healthy": api_healthy,
                "folder_healthy": false,
                "state": folder_status.state,
                "need_total_items": folder_status.need_total_items,
                "errors": errors,
            });
        }

        if folder_status.is_converged() {
            return system_sync_hint_response_json(
                state,
                &request,
                "ok",
                api_healthy,
                Some(&folder_status),
                warnings,
                None,
            );
        }

        if !request.wait_for_idle {
            return system_sync_hint_response_json(
                state,
                &request,
                "sync_pending",
                api_healthy,
                Some(&folder_status),
                warnings,
                Some("folder has pending items".to_string()),
            );
        }

        if Instant::now() >= deadline {
            return system_sync_hint_response_json(
                state,
                &request,
                "sync_pending",
                api_healthy,
                Some(&folder_status),
                warnings,
                Some(format!(
                    "sync hint timeout after {}ms waiting for idle convergence",
                    request.timeout_ms
                )),
            );
        }

        time::sleep(Duration::from_millis(SYSTEM_SYNC_HINT_POLL_INTERVAL_MS)).await;
        folder_status = match syncthing_folder_status(&desired_sync, &request.folder_id) {
            Ok(value) => value,
            Err(err) => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "SYNC_STATUS_FAILED",
                    "message": err.to_string(),
                    "hive": state.hive_id.as_str(),
                    "channel": request.channel,
                    "folder_id": request.folder_id,
                    "api_healthy": api_healthy,
                    "errors": warnings,
                });
            }
        };
    }
}

async fn restart_local_core_services_with_health_gate() -> Result<Vec<String>, OrchestratorError> {
    let mut restarted = Vec::new();
    for service in CORE_SYNC_RESTART_ORDER {
        if *service == "sy-orchestrator" {
            // Avoid self-restart while processing the request; operator can restart orchestrator separately.
            continue;
        }
        if !systemd_unit_exists(service) {
            continue;
        }
        systemd_start(service)?;
        wait_for_service_active(
            service,
            Duration::from_secs(CORE_SERVICE_HEALTH_TIMEOUT_SECS),
        )
        .await?;
        restarted.push((*service).to_string());
    }
    Ok(restarted)
}

#[derive(Debug, Clone)]
struct SystemUpdateApplyResult {
    status: String,
    updated: Vec<String>,
    unchanged: Vec<String>,
    restarted: Vec<String>,
    errors: Vec<String>,
}

fn set_exec_0755(path: &Path) -> Result<(), OrchestratorError> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn compute_local_core_update_sets(
    manifest: &CoreManifest,
    is_motherbee: bool,
) -> Result<(Vec<String>, Vec<String>), OrchestratorError> {
    let component_names = core_component_names_for_role(manifest, is_motherbee)?;
    let local_paths = component_names
        .iter()
        .map(|name| local_core_bin_source_path(name).display().to_string())
        .collect::<Vec<_>>();
    validate_core_manifest_for_bins(&local_paths)?;

    let mut updated = Vec::new();
    let mut unchanged = Vec::new();
    for name in component_names {
        let source_path = local_core_bin_source_path(&name);
        let source_hash = sha256_file(&source_path)?;
        let target_path = Path::new("/usr/bin").join(&name);
        let target_hash = if target_path.exists() {
            Some(sha256_file(&target_path)?)
        } else {
            None
        };
        if target_hash
            .as_deref()
            .is_some_and(|hash| hash.eq_ignore_ascii_case(source_hash.trim()))
        {
            unchanged.push(name);
        } else {
            updated.push(name);
        }
    }
    Ok((updated, unchanged))
}

fn rollback_local_core_binaries(
    updated: &[String],
    backup_dir: &Path,
    created_without_backup: &HashSet<String>,
) -> Result<(), OrchestratorError> {
    let mut rollback_errors = Vec::new();
    for name in updated {
        let target_path = Path::new("/usr/bin").join(name);
        let backup_path = backup_dir.join(name);
        if backup_path.exists() {
            if let Err(err) = fs::copy(&backup_path, &target_path) {
                rollback_errors.push(format!("restore {} failed: {}", target_path.display(), err));
                continue;
            }
            if let Err(err) = set_exec_0755(&target_path) {
                rollback_errors.push(format!(
                    "restore chmod {} failed: {}",
                    target_path.display(),
                    err
                ));
            }
            continue;
        }
        if created_without_backup.contains(name)
            && target_path.exists()
            && fs::remove_file(&target_path).is_err()
        {
            rollback_errors.push(format!(
                "remove {} failed during rollback",
                target_path.display()
            ));
        }
    }

    if rollback_errors.is_empty() {
        Ok(())
    } else {
        Err(format!("rollback errors: {}", rollback_errors.join("; ")).into())
    }
}

async fn apply_system_update_local(
    state: &OrchestratorState,
    category: &str,
) -> Result<SystemUpdateApplyResult, OrchestratorError> {
    let _lifecycle_guard = state.runtime_lifecycle_lock.lock().await;
    match category {
        "runtime" => {
            let manifest =
                load_runtime_manifest_result()?.ok_or("runtime manifest missing locally")?;
            apply_runtime_retention(&manifest)?;
            let artifact_errors = verify_runtime_current_artifacts(&manifest)?;
            {
                let mut guard = state.runtime_manifest.lock().await;
                *guard = Some(manifest);
            }
            if !artifact_errors.is_empty() {
                return Ok(SystemUpdateApplyResult {
                    status: "sync_pending".to_string(),
                    updated: Vec::new(),
                    unchanged: vec!["runtime-manifest".to_string()],
                    restarted: Vec::new(),
                    errors: artifact_errors,
                });
            }
            Ok(SystemUpdateApplyResult {
                status: "ok".to_string(),
                updated: Vec::new(),
                unchanged: vec!["runtime-manifest".to_string()],
                restarted: Vec::new(),
                errors: Vec::new(),
            })
        }
        "core" => {
            let manifest = load_core_manifest()?;
            let (updated, unchanged) =
                compute_local_core_update_sets(&manifest, state.is_motherbee)?;
            let backup_dir = orchestrator_runtime_dir()
                .join("core-bin.prev.local")
                .join(format!("update-{}", now_epoch_ms()));
            fs::create_dir_all(&backup_dir)?;
            let mut created_without_backup = HashSet::new();
            let mut installed = Vec::new();
            for name in &updated {
                let source_path = local_core_bin_source_path(name);
                let target_path = Path::new("/usr/bin").join(name);
                if target_path.exists() {
                    fs::copy(&target_path, backup_dir.join(name))?;
                } else {
                    created_without_backup.insert(name.clone());
                }

                let stage_path = Path::new("/usr/bin").join(format!(".{name}.fluxbee.tmp"));
                if let Err(err) = (|| -> Result<(), OrchestratorError> {
                    fs::copy(&source_path, &stage_path)?;
                    set_exec_0755(&stage_path)?;
                    fs::rename(&stage_path, &target_path)?;
                    Ok(())
                })() {
                    let rollback_note = match rollback_local_core_binaries(
                        &installed,
                        &backup_dir,
                        &created_without_backup,
                    ) {
                        Ok(()) => "rollback applied".to_string(),
                        Err(rb_err) => format!("rollback failed: {rb_err}"),
                    };
                    return Err(format!(
                        "core local install failed for component '{}': {}; {}",
                        name, err, rollback_note
                    )
                    .into());
                }
                installed.push(name.clone());
            }

            match restart_local_core_services_with_health_gate().await {
                Ok(restarted) => Ok(SystemUpdateApplyResult {
                    status: "ok".to_string(),
                    updated,
                    unchanged,
                    restarted,
                    errors: Vec::new(),
                }),
                Err(err) => {
                    let rollback_note = match rollback_local_core_binaries(
                        &updated,
                        &backup_dir,
                        &created_without_backup,
                    ) {
                        Ok(()) => match restart_local_core_services_with_health_gate().await {
                            Ok(_) => "rollback applied and services recovered".to_string(),
                            Err(rb_restart_err) => {
                                format!("rollback applied but service recovery failed: {rb_restart_err}")
                            }
                        },
                        Err(rb_err) => format!("rollback failed: {rb_err}"),
                    };
                    Ok(SystemUpdateApplyResult {
                        status: "rollback".to_string(),
                        updated: Vec::new(),
                        unchanged,
                        restarted: Vec::new(),
                        errors: vec![format!("core health gate failed: {err}; {rollback_note}")],
                    })
                }
            }
        }
        "vendor" => {
            let desired_blob = current_blob_runtime_config(state);
            let desired_dist = current_dist_runtime_config(state);
            let desired_sync = effective_syncthing_runtime_config(&desired_blob, &desired_dist);
            if desired_sync.sync_enabled
                && (blob_sync_tool_is_syncthing(&desired_sync)
                    || dist_sync_tool_is_syncthing(&desired_dist))
            {
                ensure_blob_sync_runtime(&desired_blob, &desired_dist).await?;
                Ok(SystemUpdateApplyResult {
                    status: "ok".to_string(),
                    updated: Vec::new(),
                    unchanged: vec!["syncthing".to_string()],
                    restarted: vec![SYNCTHING_SERVICE_NAME.to_string()],
                    errors: Vec::new(),
                })
            } else {
                Ok(SystemUpdateApplyResult {
                    status: "ok".to_string(),
                    updated: Vec::new(),
                    unchanged: vec!["vendor-sync-disabled".to_string()],
                    restarted: Vec::new(),
                    errors: Vec::new(),
                })
            }
        }
        _ => Err(format!("unknown system update category '{}'", category).into()),
    }
}

async fn handle_system_update_message(
    state: &OrchestratorState,
    msg: &Message,
) -> serde_json::Value {
    let (category, expected_version, expected_hash) =
        match parse_system_update_payload(&msg.payload) {
            Ok(parsed) => parsed,
            Err(err) => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "MANIFEST_INVALID",
                    "message": err.to_string(),
                    "category": msg.payload.get("category").and_then(|v| v.as_str()).unwrap_or(""),
                });
            }
        };

    let local = match local_system_manifest_state(&category) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "MANIFEST_INVALID",
                "message": err.to_string(),
                "category": category,
            });
        }
    };

    let local_version = local.version.unwrap_or(0);
    let local_hash = local.hash.unwrap_or_default();
    let expected_hash_norm = normalize_sha256(&expected_hash);
    let local_hash_norm = normalize_sha256(&local_hash);
    if expected_version != 0 {
        if let Some(local_known_version) = local.version {
            if local_known_version > expected_version {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "VERSION_MISMATCH",
                    "message": format!(
                        "local manifest version is ahead of requested version (local={} requested={})",
                        local_known_version, expected_version
                    ),
                    "category": category,
                    "hive": state.hive_id.as_str(),
                    "manifest_version": expected_version,
                    "local_manifest_version": local.version,
                    "local_manifest_hash": if local_hash.is_empty() { serde_json::Value::Null } else { serde_json::json!(local_hash) },
                    "errors": [],
                });
            }
            if local_known_version < expected_version {
                return serde_json::json!({
                    "status": "sync_pending",
                    "category": category,
                    "hive": state.hive_id.as_str(),
                    "manifest_version": expected_version,
                    "local_manifest_version": local.version,
                    "local_manifest_hash": if local_hash.is_empty() { serde_json::Value::Null } else { serde_json::json!(local_hash) },
                    "errors": [],
                    "message": "Local manifest version is behind expected. Sync channel may still be propagating.",
                });
            }
        }
    }
    let version_match = if expected_version == 0 || local.version.is_none() {
        true
    } else {
        local_version == expected_version
    };
    let hash_match = !local_hash_norm.is_empty() && local_hash_norm == expected_hash_norm;

    if !(version_match && hash_match) {
        return serde_json::json!({
            "status": "sync_pending",
            "category": category,
            "hive": state.hive_id.as_str(),
            "manifest_version": expected_version,
            "local_manifest_version": local.version,
            "local_manifest_hash": if local_hash.is_empty() { serde_json::Value::Null } else { serde_json::json!(local_hash) },
            "errors": [],
            "message": "Local manifest does not match expected. Sync channel may still be propagating.",
        });
    }

    match apply_system_update_local(state, &category).await {
        Ok(result) => serde_json::json!({
            "status": result.status,
            "category": category,
            "hive": state.hive_id.as_str(),
            "manifest_version": expected_version,
            "local_manifest_version": local.version,
            "local_manifest_hash": local_hash,
            "updated": result.updated,
            "unchanged": result.unchanged,
            "restarted": result.restarted,
            "errors": result.errors,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "UPDATE_FAILED",
            "message": err.to_string(),
            "category": category,
            "hive": state.hive_id.as_str(),
            "manifest_version": expected_version,
            "local_manifest_version": local.version,
            "local_manifest_hash": if local_hash.is_empty() { serde_json::Value::Null } else { serde_json::json!(local_hash) },
            "updated": [],
            "unchanged": [],
            "restarted": [],
            "errors": [err.to_string()],
        }),
    }
}

fn load_system_allowed_origins(hive_id: &str) -> HashSet<String> {
    let raw = std::env::var("ORCH_SYSTEM_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "SY.admin,WF.orch.diag".to_string());
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| {
            if item.contains('@') {
                item.to_string()
            } else {
                format!("{item}@{hive_id}")
            }
        })
        .collect()
}

async fn resolve_system_source_name_with_retry(
    state: &OrchestratorState,
    source_uuid: &str,
) -> Option<String> {
    let uuid = Uuid::parse_str(source_uuid).ok()?;
    let start = Instant::now();
    loop {
        if let Ok(snapshot) = load_router_snapshot(state) {
            if let Some(name) = source_name_from_snapshot(&snapshot, uuid) {
                return Some(name);
            }
        }
        if let Ok(snapshot) = load_lsa_snapshot(state) {
            if let Some(name) = source_name_from_lsa_snapshot(&snapshot, uuid) {
                return Some(name);
            }
        }
        if start.elapsed() >= Duration::from_secs(2) {
            return None;
        }
        time::sleep(Duration::from_millis(25)).await;
    }
}

fn source_name_from_snapshot(snapshot: &ShmSnapshot, source_uuid: Uuid) -> Option<String> {
    for entry in &snapshot.nodes {
        if entry.name_len == 0 {
            continue;
        }
        let Ok(entry_uuid) = Uuid::from_slice(&entry.uuid) else {
            continue;
        };
        if entry_uuid == source_uuid {
            return Some(node_name(entry));
        }
    }
    None
}

fn source_name_from_lsa_snapshot(snapshot: &LsaSnapshot, source_uuid: Uuid) -> Option<String> {
    for entry in &snapshot.nodes {
        if entry.name_len == 0 {
            continue;
        }
        let Ok(entry_uuid) = Uuid::from_slice(&entry.uuid) else {
            continue;
        };
        if entry_uuid == source_uuid {
            let len = entry.name_len as usize;
            let name = String::from_utf8_lossy(&entry.name[..len]).into_owned();
            return Some(name);
        }
    }
    None
}

async fn send_system_action_response(
    sender: &NodeSender,
    request: &Message,
    msg_name: &str,
    payload: serde_json::Value,
) -> Result<(), OrchestratorError> {
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: 16,
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg_name.to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

async fn wait_for_nats_ready(endpoint: &str, timeout: Duration) -> Result<(), OrchestratorError> {
    let start = Instant::now();
    loop {
        match json_router::nats::check_endpoint(endpoint, Duration::from_secs(2)).await {
            Ok(()) => {
                tracing::info!(endpoint = %endpoint, "nats endpoint ready");
                return Ok(());
            }
            Err(err) => {
                if start.elapsed() >= timeout {
                    return Err(format!(
                        "nats bootstrap timeout endpoint={} error={}",
                        endpoint, err
                    )
                    .into());
                }
            }
        }
        time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_service_active(
    service: &str,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    let start = Instant::now();
    loop {
        if systemd_is_active(service) {
            tracing::info!(service = service, "service active");
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(format!("{} bootstrap timeout (systemd inactive)", service).into());
        }
        time::sleep(Duration::from_millis(250)).await;
    }
}

fn nats_endpoint_from_hive(hive: &HiveFile) -> String {
    let Some(nats) = hive.nats.as_ref() else {
        return "nats://127.0.0.1:4222".to_string();
    };

    if let Some(url) = nats.url.as_ref() {
        let url = url.trim();
        if !url.is_empty() {
            return url.to_string();
        }
    }

    let mode = nats
        .mode
        .as_deref()
        .unwrap_or("embedded")
        .trim()
        .to_ascii_lowercase();
    let port = nats.port.unwrap_or(4222);
    if mode == "embedded" || mode == "client" {
        format!("nats://127.0.0.1:{port}")
    } else {
        "nats://127.0.0.1:4222".to_string()
    }
}

fn blob_runtime_from_hive(hive: &HiveFile) -> BlobRuntimeConfig {
    let mut enabled = DEFAULT_BLOB_ENABLED;
    let mut path = PathBuf::from(DEFAULT_BLOB_PATH);
    let mut sync_enabled = DEFAULT_BLOB_SYNC_ENABLED;
    let mut sync_tool = DEFAULT_BLOB_SYNC_TOOL.to_string();
    let mut sync_api_port = DEFAULT_BLOB_SYNC_API_PORT;
    let mut sync_data_dir = PathBuf::from(DEFAULT_BLOB_SYNC_DATA_DIR);
    let mut sync_service_user = DEFAULT_BLOB_SYNC_SERVICE_USER.to_string();
    let mut sync_allow_root_fallback = DEFAULT_BLOB_SYNC_ALLOW_ROOT_FALLBACK;
    let mut gc_enabled = DEFAULT_BLOB_GC_ENABLED;
    let mut gc_interval_secs = DEFAULT_BLOB_GC_INTERVAL_SECS;
    let mut gc_apply = DEFAULT_BLOB_GC_APPLY;
    let mut gc_staging_ttl_hours = DEFAULT_BLOB_GC_STAGING_TTL_HOURS;
    let mut gc_active_retain_days = DEFAULT_BLOB_GC_ACTIVE_RETAIN_DAYS;

    if let Some(blob) = hive.blob.as_ref() {
        if let Some(value) = blob.enabled {
            enabled = value;
        }
        if let Some(value) = blob.path.as_ref() {
            let value = value.trim();
            if !value.is_empty() {
                path = PathBuf::from(value);
            }
        }
        if let Some(sync) = blob.sync.as_ref() {
            if let Some(value) = sync.enabled {
                sync_enabled = value;
            }
            if let Some(value) = sync.tool.as_ref() {
                let value = value.trim().to_ascii_lowercase();
                if !value.is_empty() {
                    sync_tool = value;
                }
            }
            if let Some(value) = sync.api_port {
                sync_api_port = value;
            }
            if let Some(value) = sync.data_dir.as_ref() {
                let value = value.trim();
                if !value.is_empty() {
                    sync_data_dir = PathBuf::from(value);
                }
            }
            if let Some(value) = sync.service_user.as_ref() {
                let value = value.trim();
                if !value.is_empty() {
                    sync_service_user = value.to_string();
                }
            }
            if let Some(value) = sync.allow_root_fallback {
                sync_allow_root_fallback = value;
            }
        }
        if let Some(gc) = blob.gc.as_ref() {
            if let Some(value) = gc.enabled {
                gc_enabled = value;
            }
            if let Some(value) = gc.interval_secs {
                gc_interval_secs = value.max(60);
            }
            if let Some(value) = gc.apply {
                gc_apply = value;
            }
            if let Some(value) = gc.staging_ttl_hours {
                gc_staging_ttl_hours = value.max(1);
            }
            if let Some(value) = gc.active_retain_days {
                gc_active_retain_days = value.max(1);
            }
        }
    }

    BlobRuntimeConfig {
        enabled,
        path,
        sync_enabled,
        sync_tool,
        sync_api_port,
        sync_data_dir,
        sync_service_user,
        sync_allow_root_fallback,
        gc_enabled,
        gc_interval_secs,
        gc_apply,
        gc_staging_ttl_hours,
        gc_active_retain_days,
    }
}

fn dist_runtime_from_hive(hive: &HiveFile) -> DistRuntimeConfig {
    let mut path = PathBuf::from(DEFAULT_DIST_PATH);
    let mut sync_enabled = DEFAULT_DIST_SYNC_ENABLED;
    let mut sync_tool = DEFAULT_DIST_SYNC_TOOL.to_string();

    if let Some(dist) = hive.dist.as_ref() {
        if let Some(value) = dist.path.as_ref() {
            let value = value.trim();
            if !value.is_empty() {
                path = PathBuf::from(value);
            }
        }
        if let Some(sync) = dist.sync.as_ref() {
            if let Some(value) = sync.enabled {
                sync_enabled = value;
            }
            if let Some(value) = sync.tool.as_ref() {
                let value = value.trim().to_ascii_lowercase();
                if !value.is_empty() {
                    sync_tool = value;
                }
            }
        }
    }

    DistRuntimeConfig {
        path,
        sync_enabled,
        sync_tool,
    }
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!("connect failed: {err}");
                time::sleep(delay).await;
            }
        }
    }
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, OrchestratorError> {
    let data = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn ensure_dirs(
    config_dir: &Path,
    state_dir: &Path,
    run_dir: &Path,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> Result<(), OrchestratorError> {
    let storage_root = json_router::paths::storage_root_dir();
    let opa_root = storage_root.join("opa");
    fs::create_dir_all(config_dir)?;
    fs::create_dir_all(state_dir.join("nodes"))?;
    fs::create_dir_all(hives_root())?;
    fs::create_dir_all(&storage_root)?;
    fs::create_dir_all(opa_root.join("current"))?;
    fs::create_dir_all(opa_root.join("staged"))?;
    fs::create_dir_all(opa_root.join("backup"))?;
    fs::create_dir_all(storage_root.join("nats"))?;
    if blob.enabled {
        ensure_dir_permissions_0750(&blob.path)?;
        ensure_dir_permissions_0750(&blob.path.join("active"))?;
        ensure_dir_permissions_0750(&blob.path.join("staging"))?;
    }
    if blob.sync_enabled {
        ensure_dir_permissions_0750(&blob.sync_data_dir)?;
    }
    if blob.sync_enabled && blob_sync_tool_is_syncthing(blob) {
        let service_user = resolve_syncthing_service_user(blob)?;
        if service_user == "root" {
            ensure_owned_dir(&blob.path, "root")?;
            ensure_owned_dir(&blob.path.join("active"), "root")?;
            ensure_owned_dir(&blob.path.join("staging"), "root")?;
            ensure_owned_dir(&blob.sync_data_dir, "root")?;
        } else {
            ensure_owned_dir(&blob.path, &service_user)?;
            ensure_owned_dir(&blob.path.join("active"), &service_user)?;
            ensure_owned_dir(&blob.path.join("staging"), &service_user)?;
            ensure_owned_dir(&blob.sync_data_dir, &service_user)?;
        }
    }
    fs::create_dir_all(&dist.path)?;
    fs::create_dir_all(dist.path.join("runtimes"))?;
    fs::create_dir_all(dist.path.join("core").join("bin"))?;
    fs::create_dir_all(dist.path.join("vendor"))?;
    fs::create_dir_all(runtimes_root())?;
    fs::create_dir_all(Path::new(DIST_CORE_BIN_SOURCE_DIR))?;
    fs::create_dir_all(Path::new(DIST_VENDOR_ROOT_DIR))?;
    fs::create_dir_all(syncthing_install_dir())?;
    fs::create_dir_all(orchestrator_runtime_dir())?;
    fs::create_dir_all(run_dir)?;
    Ok(())
}

fn blob_sync_folder_path(blob: &BlobRuntimeConfig) -> PathBuf {
    blob.path.join("active")
}

fn blob_sync_tool_is_syncthing(blob: &BlobRuntimeConfig) -> bool {
    blob.sync_tool.trim().eq_ignore_ascii_case("syncthing")
}

fn dist_sync_tool_is_syncthing(dist: &DistRuntimeConfig) -> bool {
    dist.sync_tool.trim().eq_ignore_ascii_case("syncthing")
}

fn effective_syncthing_runtime_config(
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> BlobRuntimeConfig {
    let mut effective = blob.clone();
    if !effective.sync_enabled && dist.sync_enabled {
        effective.sync_enabled = true;
        effective.sync_tool = dist.sync_tool.clone();
    }
    effective
}

fn current_blob_runtime_config(state: &OrchestratorState) -> BlobRuntimeConfig {
    match load_hive(&state.config_dir) {
        Ok(hive) => blob_runtime_from_hive(&hive),
        Err(err) => {
            tracing::warn!(
                error = %err,
                config_dir = %state.config_dir.display(),
                "failed to reload hive.yaml for blob sync reconciliation; using startup config"
            );
            state.blob.clone()
        }
    }
}

fn current_dist_runtime_config(state: &OrchestratorState) -> DistRuntimeConfig {
    match load_hive(&state.config_dir) {
        Ok(hive) => dist_runtime_from_hive(&hive),
        Err(err) => {
            tracing::warn!(
                error = %err,
                config_dir = %state.config_dir.display(),
                "failed to reload hive.yaml for dist reconciliation; using startup config"
            );
            state.dist.clone()
        }
    }
}

fn linux_user_exists(user: &str) -> bool {
    Command::new("id")
        .arg("-u")
        .arg(user)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn linux_user_primary_group(user: &str) -> Option<String> {
    let mut cmd = Command::new("id");
    cmd.arg("-gn").arg(user);
    let out = run_cmd_output(cmd, "id -gn").ok()?;
    let value = out.trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn resolve_syncthing_service_user(blob: &BlobRuntimeConfig) -> Result<String, OrchestratorError> {
    let requested = blob.sync_service_user.trim();
    let requested = if requested.is_empty() {
        DEFAULT_BLOB_SYNC_SERVICE_USER
    } else {
        requested
    };
    if linux_user_exists(requested) {
        return Ok(requested.to_string());
    }
    if blob.sync_allow_root_fallback {
        tracing::warn!(
            user = requested,
            allow_root_fallback = blob.sync_allow_root_fallback,
            "syncthing service user missing; using explicit root fallback"
        );
        return Ok("root".to_string());
    }
    Err(format!(
        "linux user '{}' not found and blob.sync.allow_root_fallback=false",
        requested
    )
    .into())
}

fn ensure_dir_permissions_0750(path: &Path) -> Result<(), OrchestratorError> {
    fs::create_dir_all(path)?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o750);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn ensure_owned_dir(path: &Path, user: &str) -> Result<(), OrchestratorError> {
    ensure_dir_permissions_0750(path)?;
    let group = linux_user_primary_group(user).unwrap_or_else(|| user.to_string());
    let mut chown = Command::new("chown");
    chown.arg(format!("{user}:{group}")).arg(path);
    run_cmd(chown, "chown syncthing dir ownership")
}

fn syncthing_binary_available() -> bool {
    Path::new(SYNCTHING_INSTALL_PATH).exists()
}

fn syncthing_install_dir() -> PathBuf {
    Path::new(SYNCTHING_INSTALL_PATH)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/var/lib/fluxbee/vendor/bin"))
}

fn normalize_sha256(raw: &str) -> String {
    raw.trim()
        .strip_prefix("sha256:")
        .unwrap_or(raw.trim())
        .to_string()
}

fn load_vendor_manifest() -> Result<Option<VendorManifest>, OrchestratorError> {
    let Some(manifest_path) = local_vendor_manifest_path() else {
        return Ok(None);
    };
    let data = fs::read_to_string(manifest_path)?;
    let manifest: VendorManifest = serde_json::from_str(&data)?;
    if manifest.schema_version == 0 {
        return Err("vendor manifest invalid: schema_version must be >= 1".into());
    }
    if manifest.components.is_empty() {
        return Err("vendor manifest has no components".into());
    }
    Ok(Some(manifest))
}

fn vendor_syncthing_component() -> Result<Option<VendorManifestComponent>, OrchestratorError> {
    let Some(manifest) = load_vendor_manifest()? else {
        return Ok(None);
    };
    let component = manifest
        .components
        .get("syncthing")
        .cloned()
        .ok_or_else(|| "vendor manifest missing 'syncthing' component".to_string())?;
    if component.path.trim().is_empty() {
        return Err("vendor manifest syncthing component missing path".into());
    }
    if normalize_sha256(&component.hash).is_empty() {
        return Err("vendor manifest syncthing component missing hash".into());
    }
    if component.upstream_version.trim().is_empty() {
        return Err("vendor manifest syncthing component missing upstream_version".into());
    }
    Ok(Some(component))
}

fn resolve_syncthing_vendor_source_path() -> Result<PathBuf, OrchestratorError> {
    if let Some(component) = vendor_syncthing_component()? {
        if let Some(path) = local_vendor_component_path(&component.path) {
            return Ok(path);
        }
        let primary = Path::new(DIST_VENDOR_ROOT_DIR).join(&component.path);
        return Err(format!(
            "vendor manifest syncthing path missing at '{}'",
            primary.display()
        )
        .into());
    }
    let fallback = PathBuf::from(DIST_SYNCTHING_VENDOR_SOURCE_PATH);
    if fallback.exists() {
        return Ok(fallback);
    }
    Err(format!(
        "syncthing vendor binary missing at '{}' and vendor manifest is absent",
        DIST_SYNCTHING_VENDOR_SOURCE_PATH
    )
    .into())
}

fn local_syncthing_vendor_hash() -> Result<Option<String>, OrchestratorError> {
    let source = resolve_syncthing_vendor_source_path()?;
    let mut cmd = Command::new("sha256sum");
    cmd.arg(&source);
    let out = run_cmd_output(cmd, "sha256sum local syncthing vendor")?;
    let hash = out
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if hash.is_empty() {
        return Ok(None);
    }
    if let Some(component) = vendor_syncthing_component()? {
        let expected = normalize_sha256(&component.hash);
        if !expected.is_empty() && expected != hash {
            return Err(format!(
                "vendor manifest hash mismatch for syncthing: expected={} actual={}",
                expected, hash
            )
            .into());
        }
        if let Some(size) = component.size {
            let actual_size = fs::metadata(&source)?.len();
            if actual_size != size {
                return Err(format!(
                    "vendor manifest size mismatch for syncthing: expected={} actual={}",
                    size, actual_size
                )
                .into());
            }
        }
    }
    Ok(Some(hash))
}

fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn open_firewall_rules_local(rules: &[String], context: &str) {
    if rules.is_empty() {
        return;
    }

    let mut applied = false;
    if command_exists("ufw") {
        applied = true;
        for rule in rules {
            let mut cmd = Command::new("ufw");
            cmd.arg("allow").arg(rule);
            if let Err(err) = run_cmd(cmd, "ufw allow") {
                tracing::warn!(context = context, rule = %rule, error = %err, "ufw allow failed");
            }
        }
    }

    if command_exists("firewall-cmd") {
        let mut state_cmd = Command::new("firewall-cmd");
        state_cmd.arg("--state");
        if run_cmd(state_cmd, "firewall-cmd --state").is_ok() {
            applied = true;
            for rule in rules {
                let mut cmd_now = Command::new("firewall-cmd");
                cmd_now.arg("--add-port").arg(rule);
                if let Err(err) = run_cmd(cmd_now, "firewall-cmd --add-port") {
                    tracing::warn!(
                        context = context,
                        rule = %rule,
                        error = %err,
                        "firewalld runtime port rule failed"
                    );
                }

                let mut cmd_persistent = Command::new("firewall-cmd");
                cmd_persistent
                    .arg("--permanent")
                    .arg("--add-port")
                    .arg(rule);
                if let Err(err) = run_cmd(cmd_persistent, "firewall-cmd --permanent --add-port") {
                    tracing::warn!(
                        context = context,
                        rule = %rule,
                        error = %err,
                        "firewalld permanent port rule failed"
                    );
                }
            }
        }
    }

    if !applied {
        tracing::warn!(
            context = context,
            rules = ?rules,
            "no ufw/firewalld detected; ports must be opened by host firewall policy"
        );
    }
}

fn close_firewall_rules_local(rules: &[String], context: &str) {
    if rules.is_empty() {
        return;
    }

    let mut applied = false;
    if command_exists("ufw") {
        applied = true;
        for rule in rules {
            let mut cmd = Command::new("ufw");
            cmd.arg("--force").arg("delete").arg("allow").arg(rule);
            if let Err(err) = run_cmd(cmd, "ufw delete allow") {
                tracing::warn!(
                    context = context,
                    rule = %rule,
                    error = %err,
                    "ufw delete allow failed"
                );
            }
        }
    }

    if command_exists("firewall-cmd") {
        let mut state_cmd = Command::new("firewall-cmd");
        state_cmd.arg("--state");
        if run_cmd(state_cmd, "firewall-cmd --state").is_ok() {
            applied = true;
            for rule in rules {
                let mut cmd_now = Command::new("firewall-cmd");
                cmd_now.arg("--remove-port").arg(rule);
                if let Err(err) = run_cmd(cmd_now, "firewall-cmd --remove-port") {
                    tracing::warn!(
                        context = context,
                        rule = %rule,
                        error = %err,
                        "firewalld runtime port remove failed"
                    );
                }

                let mut cmd_persistent = Command::new("firewall-cmd");
                cmd_persistent
                    .arg("--permanent")
                    .arg("--remove-port")
                    .arg(rule);
                if let Err(err) = run_cmd(cmd_persistent, "firewall-cmd --permanent --remove-port")
                {
                    tracing::warn!(
                        context = context,
                        rule = %rule,
                        error = %err,
                        "firewalld permanent port remove failed"
                    );
                }
            }
        }
    }

    if !applied {
        tracing::warn!(
            context = context,
            rules = ?rules,
            "no ufw/firewalld detected; ports must be removed by host firewall policy"
        );
    }
}

fn syncthing_firewall_rules() -> Vec<String> {
    vec![
        format!("{SYNCTHING_SYNC_PORT_TCP}/tcp"),
        format!("{SYNCTHING_SYNC_PORT_UDP}/udp"),
        format!("{SYNCTHING_DISCOVERY_PORT_UDP}/udp"),
    ]
}

fn ensure_syncthing_firewall_local() {
    open_firewall_rules_local(&syncthing_firewall_rules(), "syncthing");
}

fn disable_syncthing_firewall_local() {
    close_firewall_rules_local(&syncthing_firewall_rules(), "syncthing");
}

fn ensure_core_firewall_local(state: &OrchestratorState) {
    let mut rules: Vec<String> = Vec::new();
    if let Some(listen) = state.wan_listen.as_deref() {
        let listen = listen.trim();
        if !listen.is_empty() {
            match parse_host_port(listen) {
                Ok((_, port)) => rules.push(format!("{port}/tcp")),
                Err(err) => tracing::warn!(
                    wan_listen = listen,
                    error = %err,
                    "failed to parse wan.listen for firewall setup; skipping WAN port rule"
                ),
            }
        }
    }

    if state.is_motherbee {
        rules.push(format!("{}/tcp", state.identity_sync_port));
    }

    if rules.is_empty() {
        return;
    }
    rules.sort();
    rules.dedup();
    open_firewall_rules_local(&rules, "core");
}

fn ensure_syncthing_installed() -> Result<(), OrchestratorError> {
    let source = resolve_syncthing_vendor_source_path()?;
    let source_hash = local_syncthing_vendor_hash()?.unwrap_or_default();
    if let Some(parent) = Path::new(SYNCTHING_INSTALL_PATH).parent() {
        fs::create_dir_all(parent)?;
    }
    let mut install_required = !syncthing_binary_available();
    if !install_required {
        let mut cmd = Command::new("sha256sum");
        cmd.arg(SYNCTHING_INSTALL_PATH);
        match run_cmd_output(cmd, "sha256sum installed syncthing") {
            Ok(out) => {
                let installed_hash = out
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if installed_hash.is_empty()
                    || (!source_hash.is_empty() && installed_hash != source_hash)
                {
                    install_required = true;
                    tracing::warn!(
                        installed_hash = installed_hash,
                        source_hash = source_hash,
                        "syncthing binary drift detected locally; reinstalling from vendor source"
                    );
                }
            }
            Err(err) => {
                install_required = true;
                tracing::warn!(error = %err, "failed to hash installed syncthing; reinstalling");
            }
        }
    }
    if !install_required {
        return Ok(());
    }
    tracing::info!(
        source = %source.display(),
        target = SYNCTHING_INSTALL_PATH,
        "installing syncthing from vendor source"
    );
    let mut install = Command::new("install");
    install
        .arg("-m")
        .arg("0755")
        .arg(&source)
        .arg(SYNCTHING_INSTALL_PATH);
    run_cmd(install, "install syncthing from vendor source")?;
    if !syncthing_binary_available() {
        return Err("syncthing install finished but installed binary is still missing".into());
    }
    Ok(())
}

fn ensure_local_syncthing_vendor_layout() -> Result<(), OrchestratorError> {
    if !Path::new(SYNCTHING_INSTALL_PATH).exists() {
        return Err("syncthing installed binary missing while ensuring vendor layout".into());
    }
    let target_path = Path::new(DIST_SYNCTHING_VENDOR_SOURCE_PATH);
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut install = Command::new("install");
    install
        .arg("-m")
        .arg("0755")
        .arg(SYNCTHING_INSTALL_PATH)
        .arg(DIST_SYNCTHING_VENDOR_SOURCE_PATH);
    run_cmd(
        install,
        &format!(
            "install local syncthing vendor source ({})",
            DIST_SYNCTHING_VENDOR_SOURCE_PATH
        ),
    )?;
    Ok(())
}

fn syncthing_unit_contents(blob: &BlobRuntimeConfig, service_user: &str) -> String {
    let service_group =
        linux_user_primary_group(service_user).unwrap_or_else(|| service_user.to_string());
    format!(
        "[Unit]\nDescription=Fluxbee Syncthing (blob sync)\nAfter=network.target\n\n[Service]\nType=simple\nUser={}\nGroup={}\nWorkingDirectory={}\nEnvironment=HOME={}\nUMask=0027\nExecStart={} --no-browser --no-restart --home={} --gui-address=127.0.0.1:{}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
        service_user,
        service_group,
        blob.sync_data_dir.display(),
        blob.sync_data_dir.display(),
        SYNCTHING_INSTALL_PATH,
        blob.sync_data_dir.display(),
        blob.sync_api_port
    )
}

fn ensure_syncthing_unit(blob: &BlobRuntimeConfig) -> Result<(), OrchestratorError> {
    let service_user = resolve_syncthing_service_user(blob)?;
    let unit_contents = syncthing_unit_contents(blob, &service_user);
    let unit_path =
        Path::new("/etc/systemd/system").join(format!("{SYNCTHING_SERVICE_NAME}.service"));
    let current = fs::read_to_string(&unit_path).unwrap_or_default();
    if current == unit_contents {
        return Ok(());
    }
    fs::write(&unit_path, unit_contents)?;
    let mut daemon_reload = Command::new("systemctl");
    daemon_reload.arg("daemon-reload");
    run_cmd(daemon_reload, "systemctl daemon-reload")?;
    Ok(())
}

async fn syncthing_api_healthy(api_port: u16) -> bool {
    let endpoint = format!("127.0.0.1:{api_port}");
    matches!(
        time::timeout(
            Duration::from_secs(SYNCTHING_HEALTH_TIMEOUT_SECS),
            tokio::net::TcpStream::connect(endpoint)
        )
        .await,
        Ok(Ok(_))
    )
}

fn syncthing_api_key(sync: &BlobRuntimeConfig) -> Result<String, OrchestratorError> {
    let config_path = sync.sync_data_dir.join("config.xml");
    let data = fs::read_to_string(&config_path)?;
    let re = Regex::new(r#"(?s)<apikey>([^<]+)</apikey>"#)?;
    let api_key = re
        .captures(&data)
        .and_then(|caps| caps.get(1).map(|m| m.as_str().trim().to_string()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("syncthing API key missing in '{}'", config_path.display()))?;
    Ok(api_key)
}

#[derive(Debug, Clone)]
struct SyncthingFolderStatus {
    state: String,
    need_total_items: u64,
    error: String,
    invalid: String,
}

impl SyncthingFolderStatus {
    fn is_healthy(&self) -> bool {
        self.error.trim().is_empty() && self.invalid.trim().is_empty()
    }

    fn is_converged(&self) -> bool {
        self.is_healthy() && self.state.eq_ignore_ascii_case("idle") && self.need_total_items == 0
    }
}

fn json_u64(payload: &serde_json::Value, camel_key: &str, snake_key: &str) -> u64 {
    payload
        .get(camel_key)
        .or_else(|| payload.get(snake_key))
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

fn syncthing_folder_status(
    sync: &BlobRuntimeConfig,
    folder_id: &str,
) -> Result<SyncthingFolderStatus, OrchestratorError> {
    let api_key = syncthing_api_key(sync)?;
    let endpoint = format!(
        "http://127.0.0.1:{}/rest/db/status?folder={}",
        sync.sync_api_port, folder_id
    );
    let mut cmd = Command::new("curl");
    cmd.arg("-fsS")
        .arg("-H")
        .arg(format!("X-API-Key: {}", api_key))
        .arg(&endpoint);
    let out = run_cmd_output(cmd, &format!("syncthing db status folder={folder_id}"))?;
    let payload: serde_json::Value = serde_json::from_str(&out).map_err(|err| {
        format!("invalid syncthing db/status payload for folder '{folder_id}': {err}")
    })?;
    let state = payload
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .trim()
        .to_string();
    let need_total_items = json_u64(&payload, "needTotalItems", "need_total_items");
    let error = payload
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let invalid = payload
        .get("invalid")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(SyncthingFolderStatus {
        state,
        need_total_items,
        error,
        invalid,
    })
}

fn syncthing_folder_healthy(
    sync: &BlobRuntimeConfig,
    folder_id: &str,
) -> Result<bool, OrchestratorError> {
    let status = syncthing_folder_status(sync, folder_id)?;
    Ok(status.is_healthy())
}

fn syncthing_trigger_folder_scan(
    sync: &BlobRuntimeConfig,
    folder_id: &str,
) -> Result<(), OrchestratorError> {
    let api_key = syncthing_api_key(sync)?;
    let endpoint = format!(
        "http://127.0.0.1:{}/rest/db/scan?folder={}",
        sync.sync_api_port, folder_id
    );
    let mut cmd = Command::new("curl");
    cmd.arg("-fsS")
        .arg("-X")
        .arg("POST")
        .arg("-H")
        .arg(format!("X-API-Key: {}", api_key))
        .arg(&endpoint);
    let _ = run_cmd_output(cmd, &format!("syncthing scan folder={folder_id}"))?;
    Ok(())
}

async fn wait_for_syncthing_health(blob: &BlobRuntimeConfig) -> Result<(), OrchestratorError> {
    let start = Instant::now();
    let timeout = Duration::from_secs(SYNCTHING_BOOTSTRAP_TIMEOUT_SECS);
    loop {
        if syncthing_api_healthy(blob.sync_api_port).await {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(
                format!("syncthing health timeout (api port {})", blob.sync_api_port).into(),
            );
        }
        time::sleep(Duration::from_millis(250)).await;
    }
}

fn valid_syncthing_device_id(device_id: &str) -> bool {
    let id = device_id.trim();
    if id.len() < 16 {
        return false;
    }
    id.chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '-')
}

fn extract_first_syncthing_device_id(config_xml: &str) -> Option<String> {
    let device_re = Regex::new(r#"<device\b[^>]*\bid="([^"]+)""#).ok()?;
    for caps in device_re.captures_iter(config_xml) {
        let candidate = caps.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if valid_syncthing_device_id(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn local_syncthing_device_id(sync: &BlobRuntimeConfig) -> Result<String, OrchestratorError> {
    let home = sync.sync_data_dir.display().to_string();
    let mut cmd = Command::new(SYNCTHING_INSTALL_PATH);
    cmd.arg("--home")
        .arg(&sync.sync_data_dir)
        .arg("--device-id")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home);
    match run_cmd_output(cmd, "syncthing --device-id") {
        Ok(out) => {
            let device_id = out.trim();
            if valid_syncthing_device_id(device_id) {
                return Ok(device_id.to_string());
            }
            tracing::warn!(
                output = %truncate_for_error(&out, 200),
                "invalid syncthing --device-id output; falling back to config.xml parsing"
            );
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to resolve syncthing device id from binary; falling back to config.xml parsing"
            );
        }
    }

    let config_path = sync.sync_data_dir.join("config.xml");
    let data = fs::read_to_string(&config_path)?;
    extract_first_syncthing_device_id(&data).ok_or_else(|| {
        format!(
            "failed to resolve syncthing device id from '{}'",
            config_path.display()
        )
        .into()
    })
}

fn xml_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn set_xml_attr(start_tag: &str, attr: &str, value: &str) -> Result<String, OrchestratorError> {
    let attr_re = Regex::new(&format!(r#"\b{}="[^"]*""#, regex::escape(attr)))?;
    let replacement = format!(r#"{attr}="{value}""#);
    if attr_re.is_match(start_tag) {
        return Ok(attr_re
            .replace(start_tag, replacement.as_str())
            .into_owned());
    }
    let Some(insert_at) = start_tag.rfind('>') else {
        return Err(format!("invalid xml tag (missing '>'): {start_tag}").into());
    };
    let mut updated = String::with_capacity(start_tag.len() + replacement.len() + 2);
    updated.push_str(&start_tag[..insert_at]);
    updated.push(' ');
    updated.push_str(&replacement);
    updated.push_str(&start_tag[insert_at..]);
    Ok(updated)
}

fn rewrite_syncthing_folder_block(
    block: &str,
    folder_id: &str,
    folder_path: &str,
    folder_label: &str,
) -> Result<String, OrchestratorError> {
    let Some(tag_end) = block.find('>') else {
        return Err("invalid syncthing folder block".into());
    };
    let start_tag = &block[..=tag_end];
    let body = &block[tag_end + 1..];
    let mut updated_tag = start_tag.to_string();
    updated_tag = set_xml_attr(&updated_tag, "id", &xml_escape_attr(folder_id))?;
    updated_tag = set_xml_attr(&updated_tag, "path", &xml_escape_attr(folder_path))?;
    updated_tag = set_xml_attr(&updated_tag, "label", &xml_escape_attr(folder_label))?;
    if !updated_tag.contains(" type=") {
        updated_tag = set_xml_attr(&updated_tag, "type", "sendreceive")?;
    }
    Ok(format!("{updated_tag}{body}"))
}

fn minimal_syncthing_folder_block(
    config_xml: &str,
    folder_id: &str,
    folder_path: &str,
    folder_label: &str,
) -> Result<String, OrchestratorError> {
    let device_re = Regex::new(r#"<device\b[^>]*\bid="([^"]+)""#)?;
    let Some(caps) = device_re.captures(config_xml) else {
        return Err("syncthing config has no device id to seed folder".into());
    };
    let Some(device_id) = caps.get(1).map(|m| m.as_str()) else {
        return Err("syncthing config has malformed device id".into());
    };
    Ok(format!(
        "<folder id=\"{}\" label=\"{}\" path=\"{}\" type=\"sendreceive\" rescanIntervalS=\"3600\" fsWatcherEnabled=\"true\" fsWatcherDelayS=\"10\" ignorePerms=\"false\" autoNormalize=\"true\">\n    <filesystemType>basic</filesystemType>\n    <device id=\"{}\" introducedBy=\"\"/>\n    <minDiskFree unit=\"%\">1</minDiskFree>\n  </folder>",
        xml_escape_attr(folder_id),
        xml_escape_attr(folder_label),
        xml_escape_attr(folder_path),
        xml_escape_attr(device_id)
    ))
}

fn ensure_syncthing_folder_in_config_xml(
    config_xml: &str,
    folder_id: &str,
    folder_path: &str,
    folder_label: &str,
) -> Result<(String, bool), OrchestratorError> {
    let folder_re = Regex::new(r#"(?s)<folder\b[^>]*\bid="([^"]+)"[^>]*>.*?</folder>"#)?;

    for caps in folder_re.captures_iter(config_xml) {
        let Some(found_id) = caps.get(1).map(|m| m.as_str()) else {
            continue;
        };
        if found_id != folder_id {
            continue;
        }
        let Some(full) = caps.get(0) else {
            continue;
        };
        let rewritten =
            rewrite_syncthing_folder_block(full.as_str(), folder_id, folder_path, folder_label)?;
        if rewritten == full.as_str() {
            return Ok((config_xml.to_string(), false));
        }
        let mut out =
            String::with_capacity(config_xml.len() + rewritten.len().saturating_sub(full.len()));
        out.push_str(&config_xml[..full.start()]);
        out.push_str(&rewritten);
        out.push_str(&config_xml[full.end()..]);
        return Ok((out, true));
    }

    let mut template_block: Option<String> = None;
    for caps in folder_re.captures_iter(config_xml) {
        let Some(full) = caps.get(0) else {
            continue;
        };
        if template_block.is_none() {
            template_block = Some(full.as_str().to_string());
        }
        if caps.get(1).map(|m| m.as_str()) == Some("default") {
            template_block = Some(full.as_str().to_string());
            break;
        }
    }

    let new_block = if let Some(template) = template_block {
        rewrite_syncthing_folder_block(&template, folder_id, folder_path, folder_label)?
    } else {
        minimal_syncthing_folder_block(config_xml, folder_id, folder_path, folder_label)?
    };

    let insert_at = config_xml
        .rfind("</configuration>")
        .unwrap_or(config_xml.len());
    let mut out = String::with_capacity(config_xml.len() + new_block.len() + 8);
    out.push_str(&config_xml[..insert_at]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("  ");
    out.push_str(&new_block);
    out.push('\n');
    out.push_str(&config_xml[insert_at..]);
    Ok((out, true))
}

fn ensure_syncthing_top_level_device_in_config_xml(
    config_xml: &str,
    device_id: &str,
    device_name: &str,
) -> Result<(String, bool), OrchestratorError> {
    let normalized_id = device_id.trim();
    if !valid_syncthing_device_id(normalized_id) {
        return Err(format!("invalid syncthing device id '{}'", device_id).into());
    }

    let header_end = config_xml
        .find("<folder")
        .or_else(|| config_xml.find("</configuration>"))
        .unwrap_or(config_xml.len());
    let header = &config_xml[..header_end];
    let device_re = Regex::new(&format!(
        r#"<device\b[^>]*\bid="{}"[^>]*>"#,
        regex::escape(normalized_id)
    ))?;
    if device_re.is_match(header) {
        return Ok((config_xml.to_string(), false));
    }

    let name = if device_name.trim().is_empty() {
        normalized_id
    } else {
        device_name.trim()
    };
    let new_device = format!(
        "  <device id=\"{}\" name=\"{}\" compression=\"metadata\" introducer=\"false\" skipIntroductionRemovals=\"false\" introducedBy=\"\"></device>\n",
        xml_escape_attr(normalized_id),
        xml_escape_attr(name),
    );
    let mut out = String::with_capacity(config_xml.len() + new_device.len() + 1);
    out.push_str(&config_xml[..header_end]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&new_device);
    out.push_str(&config_xml[header_end..]);
    Ok((out, true))
}

fn ensure_syncthing_folder_has_device(
    config_xml: &str,
    folder_id: &str,
    device_id: &str,
) -> Result<(String, bool), OrchestratorError> {
    let normalized_id = device_id.trim();
    if !valid_syncthing_device_id(normalized_id) {
        return Err(format!("invalid syncthing device id '{}'", device_id).into());
    }

    let folder_re = Regex::new(r#"(?s)<folder\b[^>]*\bid="([^"]+)"[^>]*>.*?</folder>"#)?;
    let device_re = Regex::new(&format!(
        r#"<device\b[^>]*\bid="{}"[^>]*>"#,
        regex::escape(normalized_id)
    ))?;
    for caps in folder_re.captures_iter(config_xml) {
        let Some(found_id) = caps.get(1).map(|m| m.as_str()) else {
            continue;
        };
        if found_id != folder_id {
            continue;
        }
        let Some(full) = caps.get(0) else {
            continue;
        };
        if device_re.is_match(full.as_str()) {
            return Ok((config_xml.to_string(), false));
        }
        let insert_at = full.end().saturating_sub("</folder>".len());
        let mut out = String::with_capacity(config_xml.len() + 72 + normalized_id.len());
        out.push_str(&config_xml[..insert_at]);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("    <device id=\"");
        out.push_str(&xml_escape_attr(normalized_id));
        out.push_str("\" introducedBy=\"\"/>\n");
        out.push_str(&config_xml[insert_at..]);
        return Ok((out, true));
    }
    Ok((config_xml.to_string(), false))
}

fn reconcile_syncthing_peer_xml(
    config_xml: &str,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
    peer_device_id: &str,
    peer_name: &str,
) -> Result<(String, bool), OrchestratorError> {
    let mut updated = config_xml.to_string();
    let mut changed = false;

    let (next, top_changed) =
        ensure_syncthing_top_level_device_in_config_xml(&updated, peer_device_id, peer_name)?;
    updated = next;
    changed |= top_changed;

    if blob.sync_enabled && blob_sync_tool_is_syncthing(blob) {
        let blob_sync_path = blob_sync_folder_path(blob);
        let (next, folder_changed) = ensure_syncthing_folder_in_config_xml(
            &updated,
            SYNCTHING_FOLDER_BLOB_ID,
            &blob_sync_path.display().to_string(),
            "Fluxbee Blob",
        )?;
        updated = next;
        changed |= folder_changed;
        let (next, device_changed) =
            ensure_syncthing_folder_has_device(&updated, SYNCTHING_FOLDER_BLOB_ID, peer_device_id)?;
        updated = next;
        changed |= device_changed;
    }

    if dist.sync_enabled && dist_sync_tool_is_syncthing(dist) {
        let (next, folder_changed) = ensure_syncthing_folder_in_config_xml(
            &updated,
            SYNCTHING_FOLDER_DIST_ID,
            &dist.path.display().to_string(),
            "Fluxbee Dist",
        )?;
        updated = next;
        changed |= folder_changed;
        let (next, device_changed) =
            ensure_syncthing_folder_has_device(&updated, SYNCTHING_FOLDER_DIST_ID, peer_device_id)?;
        updated = next;
        changed |= device_changed;
    }

    Ok((updated, changed))
}

fn ensure_local_syncthing_peer_link(
    sync: &BlobRuntimeConfig,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
    peer_device_id: &str,
    peer_name: &str,
) -> Result<bool, OrchestratorError> {
    let config_path = sync.sync_data_dir.join("config.xml");
    let current = fs::read_to_string(&config_path)?;
    let (updated, changed) =
        reconcile_syncthing_peer_xml(&current, blob, dist, peer_device_id, peer_name)?;
    if changed {
        fs::write(&config_path, updated)?;
    }
    Ok(changed)
}

async fn ensure_syncthing_peer_link_runtime(
    sync: &BlobRuntimeConfig,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
    peer_device_id: &str,
    peer_name: &str,
) -> Result<(), OrchestratorError> {
    if !sync.sync_enabled {
        return Ok(());
    }
    if !(blob_sync_tool_is_syncthing(sync) || dist_sync_tool_is_syncthing(dist)) {
        return Ok(());
    }
    let changed = ensure_local_syncthing_peer_link(sync, blob, dist, peer_device_id, peer_name)?;
    if !changed {
        return Ok(());
    }
    tracing::info!(
        service = SYNCTHING_SERVICE_NAME,
        peer_name = peer_name,
        "syncthing peer config reconciled locally; restarting service"
    );
    let mut restart = Command::new("systemctl");
    restart.arg("restart").arg(SYNCTHING_SERVICE_NAME);
    run_cmd(restart, "systemctl restart")?;
    wait_for_service_active(
        SYNCTHING_SERVICE_NAME,
        Duration::from_secs(SYNCTHING_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;
    wait_for_syncthing_health(sync).await?;
    Ok(())
}

fn remove_syncthing_folder_device(
    config_xml: &str,
    folder_id: &str,
    device_id: &str,
) -> Result<(String, bool), OrchestratorError> {
    let normalized_id = device_id.trim();
    if !valid_syncthing_device_id(normalized_id) {
        return Err(format!("invalid syncthing device id '{}'", device_id).into());
    }
    let folder_re = Regex::new(r#"(?s)<folder\b[^>]*\bid="([^"]+)"[^>]*>.*?</folder>"#)?;
    let device_re = Regex::new(&format!(
        r#"(?s)\n?[ \t]*<device\b[^>]*\bid="{}"[^>]*(?:/>|>.*?</device>)[ \t]*\n?"#,
        regex::escape(normalized_id)
    ))?;
    for caps in folder_re.captures_iter(config_xml) {
        let Some(found_id) = caps.get(1).map(|m| m.as_str()) else {
            continue;
        };
        if found_id != folder_id {
            continue;
        }
        let Some(full) = caps.get(0) else {
            continue;
        };
        let rewritten = device_re.replace_all(full.as_str(), "").into_owned();
        if rewritten == full.as_str() {
            return Ok((config_xml.to_string(), false));
        }
        let mut out =
            String::with_capacity(config_xml.len() + rewritten.len().saturating_sub(full.len()));
        out.push_str(&config_xml[..full.start()]);
        out.push_str(&rewritten);
        out.push_str(&config_xml[full.end()..]);
        return Ok((out, true));
    }
    Ok((config_xml.to_string(), false))
}

fn remove_syncthing_top_level_device_from_config_xml(
    config_xml: &str,
    device_id: &str,
) -> Result<(String, bool), OrchestratorError> {
    let normalized_id = device_id.trim();
    if !valid_syncthing_device_id(normalized_id) {
        return Err(format!("invalid syncthing device id '{}'", device_id).into());
    }
    let header_end = config_xml
        .find("<folder")
        .or_else(|| config_xml.find("</configuration>"))
        .unwrap_or(config_xml.len());
    let header = &config_xml[..header_end];
    let device_re = Regex::new(&format!(
        r#"(?s)\n?[ \t]*<device\b[^>]*\bid="{}"[^>]*(?:/>|>.*?</device>)[ \t]*\n?"#,
        regex::escape(normalized_id)
    ))?;
    let rewritten = device_re.replace_all(header, "").into_owned();
    if rewritten == header {
        return Ok((config_xml.to_string(), false));
    }
    let mut out = String::with_capacity(config_xml.len());
    out.push_str(&rewritten);
    out.push_str(&config_xml[header_end..]);
    Ok((out, true))
}

fn reconcile_syncthing_peer_removal_xml(
    config_xml: &str,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
    peer_device_id: &str,
) -> Result<(String, bool), OrchestratorError> {
    let mut updated = config_xml.to_string();
    let mut changed = false;

    if blob.sync_enabled && blob_sync_tool_is_syncthing(blob) {
        let (next, folder_changed) =
            remove_syncthing_folder_device(&updated, SYNCTHING_FOLDER_BLOB_ID, peer_device_id)?;
        updated = next;
        changed |= folder_changed;
    }

    if dist.sync_enabled && dist_sync_tool_is_syncthing(dist) {
        let (next, folder_changed) =
            remove_syncthing_folder_device(&updated, SYNCTHING_FOLDER_DIST_ID, peer_device_id)?;
        updated = next;
        changed |= folder_changed;
    }

    let (next, top_changed) =
        remove_syncthing_top_level_device_from_config_xml(&updated, peer_device_id)?;
    updated = next;
    changed |= top_changed;

    Ok((updated, changed))
}

fn ensure_local_syncthing_peer_unlink(
    sync: &BlobRuntimeConfig,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
    peer_device_id: &str,
) -> Result<bool, OrchestratorError> {
    let config_path = sync.sync_data_dir.join("config.xml");
    let current = fs::read_to_string(&config_path)?;
    let (updated, changed) =
        reconcile_syncthing_peer_removal_xml(&current, blob, dist, peer_device_id)?;
    if changed {
        fs::write(&config_path, updated)?;
    }
    Ok(changed)
}

async fn ensure_syncthing_peer_unlink_runtime(
    sync: &BlobRuntimeConfig,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
    peer_device_id: &str,
    peer_name: &str,
) -> Result<bool, OrchestratorError> {
    if !sync.sync_enabled {
        return Ok(false);
    }
    if !(blob_sync_tool_is_syncthing(sync) || dist_sync_tool_is_syncthing(dist)) {
        return Ok(false);
    }
    let changed = ensure_local_syncthing_peer_unlink(sync, blob, dist, peer_device_id)?;
    if !changed {
        return Ok(false);
    }
    tracing::info!(
        service = SYNCTHING_SERVICE_NAME,
        peer_name = peer_name,
        "syncthing peer config reconciled locally (unlink); restarting service"
    );
    let mut restart = Command::new("systemctl");
    restart.arg("restart").arg(SYNCTHING_SERVICE_NAME);
    run_cmd(restart, "systemctl restart")?;
    wait_for_service_active(
        SYNCTHING_SERVICE_NAME,
        Duration::from_secs(SYNCTHING_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;
    wait_for_syncthing_health(sync).await?;
    Ok(true)
}

fn reconcile_syncthing_folders_xml(
    config_xml: &str,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> Result<(String, Vec<String>), OrchestratorError> {
    let mut updated = config_xml.to_string();
    let mut changed_folders = Vec::new();

    if blob.sync_enabled && blob_sync_tool_is_syncthing(blob) {
        let blob_sync_path = blob_sync_folder_path(blob);
        let (next, changed) = ensure_syncthing_folder_in_config_xml(
            &updated,
            SYNCTHING_FOLDER_BLOB_ID,
            &blob_sync_path.display().to_string(),
            "Fluxbee Blob",
        )?;
        if changed {
            changed_folders.push(SYNCTHING_FOLDER_BLOB_ID.to_string());
        }
        updated = next;
    }

    if dist.sync_enabled && dist_sync_tool_is_syncthing(dist) {
        let (next, changed) = ensure_syncthing_folder_in_config_xml(
            &updated,
            SYNCTHING_FOLDER_DIST_ID,
            &dist.path.display().to_string(),
            "Fluxbee Dist",
        )?;
        if changed {
            changed_folders.push(SYNCTHING_FOLDER_DIST_ID.to_string());
        }
        updated = next;
    }

    Ok((updated, changed_folders))
}

fn reconcile_local_syncthing_folders(
    sync: &BlobRuntimeConfig,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> Result<Vec<String>, OrchestratorError> {
    let config_path = sync.sync_data_dir.join("config.xml");
    let current = fs::read_to_string(&config_path)?;
    let (updated, changed_folders) = reconcile_syncthing_folders_xml(&current, blob, dist)?;
    if !changed_folders.is_empty() {
        fs::write(&config_path, updated)?;
    }
    Ok(changed_folders)
}

fn ensure_syncthing_folder_marker(path: &Path) -> Result<bool, OrchestratorError> {
    fs::create_dir_all(path)?;
    let marker = path.join(".stfolder");
    if marker.exists() {
        return Ok(false);
    }
    fs::write(&marker, b"fluxbee syncthing marker\n")?;
    Ok(true)
}

fn ensure_syncthing_folder_markers(
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> Result<Vec<String>, OrchestratorError> {
    let mut repaired = Vec::new();

    if blob.sync_enabled && blob_sync_tool_is_syncthing(blob) {
        let blob_sync_path = blob_sync_folder_path(blob);
        if ensure_syncthing_folder_marker(&blob_sync_path)? {
            repaired.push(SYNCTHING_FOLDER_BLOB_ID.to_string());
        }
    }

    if dist.sync_enabled && dist_sync_tool_is_syncthing(dist) {
        if ensure_syncthing_folder_marker(&dist.path)? {
            repaired.push(SYNCTHING_FOLDER_DIST_ID.to_string());
        }
    }

    Ok(repaired)
}

async fn ensure_blob_sync_runtime(
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> Result<(), OrchestratorError> {
    let sync = effective_syncthing_runtime_config(blob, dist);
    if !sync.sync_enabled {
        return Ok(());
    }
    if !(blob_sync_tool_is_syncthing(&sync) || dist_sync_tool_is_syncthing(dist)) {
        return Err(format!(
            "unsupported sync.tool for blob/dist (blob='{}', dist='{}'; expected syncthing)",
            blob.sync_tool, dist.sync_tool
        )
        .into());
    }

    let repaired_markers = ensure_syncthing_folder_markers(blob, dist)?;

    ensure_syncthing_installed()?;
    ensure_local_syncthing_vendor_layout()?;
    ensure_syncthing_unit(&sync)?;
    ensure_syncthing_firewall_local();
    tracing::info!(
        service = SYNCTHING_SERVICE_NAME,
        "starting blob sync service"
    );
    systemd_start(SYNCTHING_SERVICE_NAME)?;
    wait_for_service_active(
        SYNCTHING_SERVICE_NAME,
        Duration::from_secs(SYNCTHING_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;
    wait_for_syncthing_health(&sync).await?;
    let changed_folders = reconcile_local_syncthing_folders(&sync, blob, dist)?;
    if !changed_folders.is_empty() || !repaired_markers.is_empty() {
        tracing::info!(
            service = SYNCTHING_SERVICE_NAME,
            folders = ?changed_folders,
            repaired_markers = ?repaired_markers,
            "syncthing runtime reconciled locally; restarting service"
        );
        let mut restart = Command::new("systemctl");
        restart.arg("restart").arg(SYNCTHING_SERVICE_NAME);
        run_cmd(restart, "systemctl restart")?;
        wait_for_service_active(
            SYNCTHING_SERVICE_NAME,
            Duration::from_secs(SYNCTHING_BOOTSTRAP_TIMEOUT_SECS),
        )
        .await?;
        wait_for_syncthing_health(&sync).await?;
    }
    tracing::info!(
        service = SYNCTHING_SERVICE_NAME,
        api_port = sync.sync_api_port,
        "blob sync service healthy"
    );
    Ok(())
}

fn disable_blob_sync_runtime_local() -> Result<(), OrchestratorError> {
    if systemd_is_active(SYNCTHING_SERVICE_NAME) {
        tracing::info!(
            service = SYNCTHING_SERVICE_NAME,
            "stopping blob sync service"
        );
        systemd_stop(SYNCTHING_SERVICE_NAME)?;
    }
    if systemd_unit_exists(SYNCTHING_SERVICE_NAME) {
        if let Err(err) = systemd_disable(SYNCTHING_SERVICE_NAME) {
            tracing::warn!(
                service = SYNCTHING_SERVICE_NAME,
                error = %err,
                "failed to disable blob sync service"
            );
        }
    }

    let unit_path =
        Path::new("/etc/systemd/system").join(format!("{SYNCTHING_SERVICE_NAME}.service"));
    if unit_path.exists() {
        fs::remove_file(&unit_path)?;
        let mut daemon_reload = Command::new("systemctl");
        daemon_reload.arg("daemon-reload");
        run_cmd(daemon_reload, "systemctl daemon-reload")?;
    }
    disable_syncthing_firewall_local();
    Ok(())
}

async fn watchdog_blob_sync(state: &OrchestratorState) -> Result<(), OrchestratorError> {
    let desired_blob = current_blob_runtime_config(state);
    let desired_dist = current_dist_runtime_config(state);
    let desired_sync = effective_syncthing_runtime_config(&desired_blob, &desired_dist);
    let changed = {
        let mut last = state.blob_sync_last_desired.lock().await;
        if *last != desired_sync {
            *last = desired_sync.clone();
            true
        } else {
            false
        }
    };

    if !desired_sync.sync_enabled {
        if changed {
            tracing::info!("blob/dist sync disabled in hive.yaml; reverting syncthing runtime");
            disable_blob_sync_runtime_local()?;
        }
        return Ok(());
    }
    if !(blob_sync_tool_is_syncthing(&desired_sync) || dist_sync_tool_is_syncthing(&desired_dist)) {
        return Err(format!(
            "unsupported sync.tool for blob/dist (blob='{}', dist='{}'; expected syncthing)",
            desired_blob.sync_tool, desired_dist.sync_tool
        )
        .into());
    }

    if changed {
        tracing::info!("blob/dist sync config changed in hive.yaml; reconciling syncthing runtime");
        ensure_blob_sync_runtime(&desired_blob, &desired_dist).await?;
        return Ok(());
    }

    let service_active = systemd_is_active(SYNCTHING_SERVICE_NAME);
    let api_healthy = syncthing_api_healthy(desired_sync.sync_api_port).await;
    let mut folders_healthy = true;
    if service_active && api_healthy {
        if desired_blob.sync_enabled && blob_sync_tool_is_syncthing(&desired_blob) {
            match syncthing_folder_healthy(&desired_sync, SYNCTHING_FOLDER_BLOB_ID) {
                Ok(true) => {}
                Ok(false) => {
                    folders_healthy = false;
                    tracing::warn!(
                        folder = SYNCTHING_FOLDER_BLOB_ID,
                        "syncthing folder unhealthy; scheduling runtime reconcile"
                    );
                }
                Err(err) => {
                    folders_healthy = false;
                    tracing::warn!(
                        folder = SYNCTHING_FOLDER_BLOB_ID,
                        error = %err,
                        "failed to verify syncthing folder health; scheduling runtime reconcile"
                    );
                }
            }
        }
        if desired_dist.sync_enabled && dist_sync_tool_is_syncthing(&desired_dist) {
            match syncthing_folder_healthy(&desired_sync, SYNCTHING_FOLDER_DIST_ID) {
                Ok(true) => {}
                Ok(false) => {
                    folders_healthy = false;
                    tracing::warn!(
                        folder = SYNCTHING_FOLDER_DIST_ID,
                        "syncthing folder unhealthy; scheduling runtime reconcile"
                    );
                }
                Err(err) => {
                    folders_healthy = false;
                    tracing::warn!(
                        folder = SYNCTHING_FOLDER_DIST_ID,
                        error = %err,
                        "failed to verify syncthing folder health; scheduling runtime reconcile"
                    );
                }
            }
        }
    }
    if service_active && api_healthy && folders_healthy {
        return Ok(());
    }

    tracing::warn!(
        service = SYNCTHING_SERVICE_NAME,
        service_active = service_active,
        api_healthy = api_healthy,
        folders_healthy = folders_healthy,
        "syncthing unhealthy; restarting"
    );
    ensure_blob_sync_runtime(&desired_blob, &desired_dist).await?;
    Ok(())
}

fn write_pid(run_dir: &Path) -> Result<(), OrchestratorError> {
    let pid_path = run_dir.join("orchestrator.pid");
    let pid = std::process::id();
    fs::write(pid_path, pid.to_string())?;
    Ok(())
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

fn load_router_snapshot(state: &OrchestratorState) -> Result<ShmSnapshot, OrchestratorError> {
    let router_l2_name = ensure_l2_name(&state.gateway_name, &state.hive_id);
    let identity_path = state.state_dir.join(&router_l2_name).join("identity.yaml");
    let data = fs::read_to_string(&identity_path)?;
    let identity: IdentityFile = serde_yaml::from_str(&data)?;
    let shm_name = identity.shm.name;
    let reader = RouterRegionReader::open_read_only(&shm_name)?;
    reader
        .read_snapshot()
        .ok_or_else(|| "shm snapshot unavailable".into())
}

fn load_lsa_snapshot(state: &OrchestratorState) -> Result<LsaSnapshot, OrchestratorError> {
    let shm_name = format!("/jsr-lsa-{}", state.hive_id);
    let reader = LsaRegionReader::open_read_only(&shm_name)?;
    reader
        .read_snapshot()
        .ok_or_else(|| "lsa snapshot unavailable".into())
}

fn managed_node_inventory_with_root(
    state: &OrchestratorState,
    root: &Path,
) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    for kind_entry in fs::read_dir(root)? {
        let kind_entry = kind_entry?;
        if !kind_entry.file_type()?.is_dir() {
            continue;
        }
        for node_entry in fs::read_dir(kind_entry.path())? {
            let node_entry = node_entry?;
            if !node_entry.file_type()?.is_dir() {
                continue;
            }

            let node_name = node_entry.file_name().to_string_lossy().to_string();
            let hive = node_name
                .rsplit_once('@')
                .map(|(_, hive)| hive.to_string())
                .unwrap_or_else(|| state.hive_id.clone());
            let kind = node_kind_from_name(&node_name);
            let config_path = node_entry.path().join("config.json");
            let state_path = node_entry.path().join("state.json");
            let config_exists = config_path.is_file();
            let state_exists = state_path.is_file();
            let unit = unit_from_node_name(&node_name);
            let unit_active = systemd_unit_is_active(&unit).unwrap_or(false);
            let unit_failed = systemd_unit_is_failed(&unit).unwrap_or(false);
            let inventory_visible = local_inventory_has_node(state, &node_name);

            let mut config_valid = false;
            let mut config_version: Option<u64> = None;
            let mut runtime_name: Option<String> = None;
            let mut runtime_version: Option<String> = None;
            let mut identity_ilk_id: Option<String> = None;
            let mut identity_tenant_id: Option<String> = None;
            if config_exists {
                if let Ok(config_payload) = load_node_effective_config(&config_path) {
                    config_valid = true;
                    config_version = Some(config_version_from_value(&config_payload));
                    if let Some(system) = config_payload.get("_system").and_then(|v| v.as_object())
                    {
                        runtime_name = system
                            .get("runtime")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        runtime_version = system
                            .get("runtime_version")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        identity_ilk_id = system
                            .get("ilk_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        identity_tenant_id = system
                            .get("tenant_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                    if identity_tenant_id.is_none() {
                        identity_tenant_id = config_payload
                            .get("tenant_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                    }
                }
            }

            let lifecycle_state = if unit_active {
                if inventory_visible {
                    "RUNNING"
                } else {
                    "STARTING"
                }
            } else if unit_failed {
                "FAILED"
            } else if config_exists || state_exists {
                "STOPPED"
            } else {
                "UNKNOWN"
            };
            let health_state = if lifecycle_state == "RUNNING" {
                if !config_exists || !config_valid {
                    "ERROR"
                } else {
                    "HEALTHY"
                }
            } else {
                "UNKNOWN"
            };

            nodes.push(serde_json::json!({
                "name": node_name,
                "node_name": node_name,
                "hive": hive,
                "kind": kind,
                "managed": true,
                "inventory_source": "managed_instance",
                "status": lifecycle_state.to_ascii_lowercase(),
                "lifecycle_state": lifecycle_state,
                "health_state": health_state,
                "visible_in_router": inventory_visible,
                "runtime": {
                    "name": runtime_name,
                    "version": runtime_version,
                },
                "config": {
                    "path": config_path.display().to_string(),
                    "exists": config_exists,
                    "valid": config_valid,
                    "config_version": config_version,
                },
                "state": {
                    "path": state_path.display().to_string(),
                    "exists": state_exists,
                },
                "identity": {
                    "ilk_id": identity_ilk_id,
                    "tenant_id": identity_tenant_id,
                }
            }));
        }
    }

    nodes.sort_by(|a, b| {
        let a_name = a
            .get("node_name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let b_name = b
            .get("node_name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        a_name.cmp(b_name)
    });
    Ok(nodes)
}

fn managed_node_inventory(
    state: &OrchestratorState,
) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    managed_node_inventory_with_root(state, &node_files_root())
}

fn persisted_custom_nodes_with_root(
    root: &Path,
) -> Result<Vec<PersistedManagedNode>, OrchestratorError> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut nodes = Vec::new();
    for kind_entry in fs::read_dir(root)? {
        let kind_entry = kind_entry?;
        if !kind_entry.file_type()?.is_dir() {
            continue;
        }
        for node_entry in fs::read_dir(kind_entry.path())? {
            let node_entry = node_entry?;
            if !node_entry.file_type()?.is_dir() {
                continue;
            }

            let node_name = node_entry.file_name().to_string_lossy().to_string();
            let kind = node_kind_from_name(&node_name);
            if matches!(kind.as_str(), "UNKNOWN")
                || !managed_reconcile_candidate_allowed(&node_name)
            {
                continue;
            }

            let config_path = node_entry.path().join("config.json");
            if !config_path.is_file() {
                continue;
            }
            let config = match load_node_effective_config(&config_path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let Some(system) = config.get("_system").and_then(|value| value.as_object()) else {
                continue;
            };
            let Some(runtime) = system
                .get("runtime")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Some(runtime_version) = system
                .get("runtime_version")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let relaunch_on_boot = system
                .get("relaunch_on_boot")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if !relaunch_on_boot {
                continue;
            }

            nodes.push(PersistedManagedNode {
                node_name,
                kind,
                runtime: runtime.to_string(),
                runtime_version: runtime_version.to_string(),
                config_path,
                relaunch_on_boot,
            });
        }
    }

    nodes.sort_by(|a, b| a.node_name.cmp(&b.node_name));
    Ok(nodes)
}

fn persisted_custom_nodes(
    _state: &OrchestratorState,
) -> Result<Vec<PersistedManagedNode>, OrchestratorError> {
    persisted_custom_nodes_with_root(&node_files_root())
}

async fn list_nodes_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        let inventory_payload = serde_json::json!({
            "scope": "hive",
            "filter_hive": state.hive_id,
        });
        let inventory = inventory_flow(state, &inventory_payload);
        if inventory.get("status").and_then(|value| value.as_str()) == Some("ok") {
            return serde_json::json!({
                "status": "ok",
                "target": state.hive_id,
                "hive": state.hive_id,
                "inventory_source": "lsa_inventory",
                "hive_status": inventory.get("hive_status").cloned().unwrap_or(serde_json::Value::Null),
                "node_count": inventory.get("node_count").cloned().unwrap_or(serde_json::json!(0)),
                "nodes": inventory.get("nodes").cloned().unwrap_or_else(|| serde_json::json!([])),
            });
        }
        return match managed_node_inventory(state) {
            Ok(nodes) => serde_json::json!({
                "status": "ok",
                "target": state.hive_id,
                "hive": state.hive_id,
                "inventory_source": "managed_instances_fallback",
                "nodes": nodes,
            }),
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "NODE_INVENTORY_READ_FAILED",
                "message": err.to_string(),
            }),
        };
    }

    match forward_system_action_to_hive(
        state,
        &target_hive,
        "LIST_NODES",
        "LIST_NODES_RESPONSE",
        serde_json::json!({
            "target": target_hive,
        }),
    )
    .await
    {
        Ok(response) => response,
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "NODE_LIST_FAILED",
            "message": err.to_string(),
            "target": target_hive,
        }),
    }
}

fn remote_router_name(entry: &RemoteHiveEntry) -> String {
    if entry.router_name_len == 0 {
        return String::new();
    }
    let len = entry.router_name_len as usize;
    String::from_utf8_lossy(&entry.router_name[..len]).into_owned()
}

fn inventory_scope_from_payload(payload: &serde_json::Value) -> String {
    payload
        .get("scope")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("global")
        .to_ascii_lowercase()
}

fn inventory_filter_type(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("filter_type")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("type").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_uppercase())
}

fn inventory_filter_hive(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("filter_hive")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("hive_id").and_then(|v| v.as_str()))
        .or_else(|| payload.get("target").and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
}

fn hive_is_registered_for_inventory(state: &OrchestratorState, hive_id: &str) -> bool {
    let hive_id = hive_id.trim();
    !hive_id.is_empty()
        && (hive_id == state.hive_id || hives_root().join(hive_id).join("info.yaml").exists())
}

fn inventory_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let snapshot = match load_lsa_snapshot(state) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "SHM_NOT_FOUND",
                "message": err.to_string(),
            });
        }
    };

    let now = now_epoch_ms();
    let scope = inventory_scope_from_payload(payload);
    let filter_type = inventory_filter_type(payload);
    let filter_hive = if scope == "hive" {
        inventory_filter_hive(payload)
            .or_else(|| Some(target_hive_from_payload(payload, &state.hive_id)))
    } else {
        inventory_filter_hive(payload)
    };

    let mut hive_meta_by_index: HashMap<u16, (String, bool, String, String, u64)> = HashMap::new();
    let mut hive_node_counts: HashMap<u16, u32> = HashMap::new();

    for (idx, hive) in snapshot.hives.iter().enumerate() {
        let Some(hive_id) = remote_hive_name(hive) else {
            continue;
        };
        if !hive_is_registered_for_inventory(state, &hive_id) {
            continue;
        }
        if let Some(expected_hive) = filter_hive.as_deref() {
            if hive_id != expected_hive {
                continue;
            }
        }
        let status = remote_hive_status(hive, now).to_string();
        let is_local = (hive.flags & HIVE_FLAG_SELF) != 0 || hive_id == state.hive_id;
        let router_name = remote_router_name(hive);
        hive_meta_by_index.insert(
            idx as u16,
            (hive_id, is_local, status, router_name, hive.last_updated),
        );
    }

    let mut nodes: Vec<serde_json::Value> = Vec::new();
    let mut nodes_by_type: BTreeMap<String, u32> = BTreeMap::new();
    let mut nodes_by_status: BTreeMap<String, u32> = BTreeMap::new();

    for node in &snapshot.nodes {
        if node.name_len == 0 {
            continue;
        }
        if node.flags & FLAG_DELETED != 0 {
            continue;
        }

        let Some((hive_id, _is_local, hive_status, _router_name, last_seen)) =
            hive_meta_by_index.get(&node.hive_index)
        else {
            continue;
        };

        let len = node.name_len as usize;
        let name = String::from_utf8_lossy(&node.name[..len]).into_owned();
        let kind = node_kind_from_name(&name);
        if let Some(expected_kind) = filter_type.as_deref() {
            if kind != expected_kind {
                continue;
            }
        }
        let status = if hive_status == "alive" {
            remote_flags_status(node.flags).to_string()
        } else {
            hive_status.clone()
        };

        *hive_node_counts.entry(node.hive_index).or_insert(0) += 1;
        *nodes_by_type.entry(kind.clone()).or_insert(0) += 1;
        *nodes_by_status.entry(status.clone()).or_insert(0) += 1;

        let (node_name_l2, node_hive) = node_l2_and_hive(&name, hive_id);
        let uuid = match Uuid::from_slice(&node.uuid) {
            Ok(uuid) => uuid,
            Err(_) => continue,
        };
        nodes.push(serde_json::json!({
            "uuid": uuid.to_string(),
            "name": name,
            "node_name": node_name_l2,
            "hive": node_hive,
            "kind": kind,
            "vpn_id": node.vpn_id,
            "connected_at": *last_seen,
            "status": status,
        }));
    }

    let mut hives: Vec<serde_json::Value> = Vec::new();
    let mut hives_alive: u32 = 0;
    let mut hives_stale: u32 = 0;
    let mut hives_deleted: u32 = 0;

    let mut hive_indexes: Vec<u16> = hive_meta_by_index.keys().copied().collect();
    hive_indexes.sort_unstable();
    for idx in hive_indexes {
        let Some((hive_id, is_local, status, router_name, last_seen)) =
            hive_meta_by_index.get(&idx)
        else {
            continue;
        };
        match status.as_str() {
            "alive" => hives_alive += 1,
            "stale" => hives_stale += 1,
            "deleted" => hives_deleted += 1,
            _ => {}
        }
        hives.push(serde_json::json!({
            "hive_id": hive_id,
            "is_local": is_local,
            "status": status,
            "router_name": router_name,
            "last_seen": *last_seen,
            "node_count": hive_node_counts.get(&idx).copied().unwrap_or(0),
        }));
    }

    let total_hives = hives.len() as u32;
    let total_nodes = nodes.len() as u32;

    match scope.as_str() {
        "summary" => serde_json::json!({
            "status": "ok",
            "scope": "summary",
            "updated_at": now,
            "total_hives": total_hives,
            "hives_alive": hives_alive,
            "hives_stale": hives_stale,
            "hives_deleted": hives_deleted,
            "total_nodes": total_nodes,
            "nodes_by_type": nodes_by_type,
            "nodes_by_status": nodes_by_status,
        }),
        "hive" => {
            let Some(filter_hive) = filter_hive else {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing hive filter for scope='hive'",
                });
            };
            let Some(hive) = hives.first() else {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "NOT_FOUND",
                    "message": format!("hive '{}' not found in inventory", filter_hive),
                    "target": filter_hive,
                });
            };
            serde_json::json!({
                "status": "ok",
                "scope": "hive",
                "hive_id": hive.get("hive_id").cloned().unwrap_or(serde_json::Value::Null),
                "is_local": hive.get("is_local").cloned().unwrap_or(serde_json::Value::Null),
                "hive_status": hive.get("status").cloned().unwrap_or(serde_json::Value::Null),
                "router_name": hive.get("router_name").cloned().unwrap_or(serde_json::Value::Null),
                "last_seen": hive.get("last_seen").cloned().unwrap_or(serde_json::Value::Null),
                "node_count": total_nodes,
                "nodes": nodes,
                "updated_at": now,
            })
        }
        _ => serde_json::json!({
            "status": "ok",
            "scope": "global",
            "updated_at": now,
            "total_hives": total_hives,
            "total_nodes": total_nodes,
            "hives": hives,
            "nodes": nodes,
        }),
    }
}

fn local_versions_snapshot(state: &OrchestratorState) -> serde_json::Value {
    let core = match load_core_manifest() {
        Ok(manifest) => {
            let manifest_hash = local_core_manifest_hash().ok().flatten();
            serde_json::json!({
                "status": "ok",
                "schema_version": manifest.schema_version,
                "manifest_hash": manifest_hash,
                "components": manifest.components,
            })
        }
        Err(err) => serde_json::json!({
            "status": "error",
            "message": err.to_string(),
        }),
    };

    let runtimes = match load_runtime_manifest_result() {
        Ok(Some(manifest)) => {
            let manifest_hash = local_runtime_manifest_hash().ok().flatten();
            let mut runtimes = manifest.runtimes.clone();
            let runtime_entries_snapshot = manifest.runtimes.as_object().cloned();
            if let Some(runtime_map) = runtimes.as_object_mut() {
                for (runtime, entry) in runtime_map.iter_mut() {
                    let readiness = match runtime_manifest_entry_from_value(runtime, entry) {
                        Ok(entry_model) => runtime_readiness_for_entry(
                            runtime,
                            &entry_model,
                            runtime_entries_snapshot.as_ref(),
                        ),
                        Err(err) => {
                            tracing::warn!(
                                runtime = %runtime,
                                error = %err,
                                "runtime readiness skipped due invalid manifest entry"
                            );
                            serde_json::json!({})
                        }
                    };
                    if let Some(entry_obj) = entry.as_object_mut() {
                        entry_obj.insert("readiness".to_string(), readiness);
                    }
                }
            }
            serde_json::json!({
                "status": "ok",
                "manifest_version": manifest.version,
                "manifest_hash": manifest_hash,
                "runtimes": runtimes,
            })
        }
        Ok(None) => serde_json::json!({
            "status": "missing",
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "message": err.to_string(),
        }),
    };

    let vendor = match load_vendor_manifest() {
        Ok(Some(manifest)) => {
            let syncthing_path = manifest
                .components
                .get("syncthing")
                .and_then(|component| local_vendor_component_path(&component.path));
            let syncthing_present = syncthing_path.as_ref().is_some_and(|path| path.exists());
            let manifest_hash = local_syncthing_vendor_hash().ok().flatten();
            serde_json::json!({
                "status": "ok",
                "schema_version": manifest.schema_version,
                "manifest_version": manifest.version,
                "manifest_updated_at": manifest.updated_at,
                "manifest_hash": manifest_hash,
                "syncthing_present": syncthing_present,
                "components": manifest.components,
            })
        }
        Ok(None) => {
            let syncthing_present = Path::new(DIST_SYNCTHING_VENDOR_SOURCE_PATH).exists();
            serde_json::json!({
                "status": "missing",
                "syncthing_present": syncthing_present,
            })
        }
        Err(err) => serde_json::json!({
            "status": "error",
            "message": err.to_string(),
        }),
    };

    serde_json::json!({
        "hive_id": state.hive_id,
        "core": core,
        "runtimes": runtimes,
        "vendor": vendor,
    })
}

fn runtime_api_entry(
    runtime: &str,
    entry: &RuntimeManifestEntry,
    runtime_entries: Option<&serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Value {
    serde_json::json!({
        "runtime": runtime,
        "current": entry.current,
        "available": entry.available,
        "type": runtime_package_type(entry),
        "runtime_base": entry.runtime_base,
        "readiness": runtime_readiness_for_entry(runtime, entry, runtime_entries),
    })
}

fn local_runtimes_snapshot(state: &OrchestratorState) -> serde_json::Value {
    match load_runtime_manifest_result() {
        Ok(Some(manifest)) => {
            let manifest_hash = local_runtime_manifest_hash().ok().flatten();
            let runtime_entries_snapshot = manifest.runtimes.as_object().cloned();
            let runtime_map = match runtime_manifest_entry_map(&manifest) {
                Ok(map) => map,
                Err(err) => {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "MANIFEST_INVALID",
                        "message": err.to_string(),
                    });
                }
            };
            let runtimes = runtime_map
                .iter()
                .map(|(runtime, entry)| {
                    runtime_api_entry(runtime, entry, runtime_entries_snapshot.as_ref())
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "status": "ok",
                "hive_id": state.hive_id,
                "manifest_version": manifest.version,
                "manifest_hash": manifest_hash,
                "runtimes": runtimes,
            })
        }
        Ok(None) => serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_MANIFEST_MISSING",
            "message": "runtime manifest not found",
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "MANIFEST_INVALID",
            "message": err.to_string(),
        }),
    }
}

async fn local_runtime_snapshot(
    state: &OrchestratorState,
    runtime: &str,
    version_filter: Option<&str>,
) -> serde_json::Value {
    match load_runtime_manifest_result() {
        Ok(Some(manifest)) => {
            let manifest_hash = local_runtime_manifest_hash().ok().flatten();
            let runtime_entries_snapshot = manifest.runtimes.as_object().cloned();
            let runtime_map = match runtime_manifest_entry_map(&manifest) {
                Ok(map) => map,
                Err(err) => {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "MANIFEST_INVALID",
                        "message": err.to_string(),
                    });
                }
            };
            let Some(entry) = runtime_map.get(runtime) else {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "RUNTIME_NOT_FOUND",
                    "message": format!("runtime '{}' not found", runtime),
                });
            };
            let usage =
                local_runtime_usage_summary(state, runtime, version_filter).unwrap_or_else(|err| {
                    serde_json::json!({
                        "scope": "hive_local_visible",
                        "status": "error",
                        "message": err.to_string(),
                    })
                });
            let usage_global =
                global_visible_runtime_usage_summary(state, runtime, version_filter).await;
            serde_json::json!({
                "status": "ok",
                "hive_id": state.hive_id,
                "manifest_version": manifest.version,
                "manifest_hash": manifest_hash,
                "runtime": runtime_api_entry(runtime, entry, runtime_entries_snapshot.as_ref()),
                "runtime_version_filter": version_filter,
                "usage": usage,
                "usage_global_visible": usage_global,
            })
        }
        Ok(None) => serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_MANIFEST_MISSING",
            "message": "runtime manifest not found",
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "MANIFEST_INVALID",
            "message": err.to_string(),
        }),
    }
}

fn local_runtime_usage_summary(
    state: &OrchestratorState,
    runtime: &str,
    version_filter: Option<&str>,
) -> Result<serde_json::Value, OrchestratorError> {
    let root = node_files_root();
    if !root.exists() {
        return Ok(serde_json::json!({
            "scope": "hive_local_visible",
            "in_use": false,
            "running_count": 0,
            "running_nodes": [],
        }));
    }

    let mut running_nodes = Vec::new();
    for kind_entry in fs::read_dir(&root)? {
        let kind_entry = kind_entry?;
        if !kind_entry.file_type()?.is_dir() {
            continue;
        }
        for node_entry in fs::read_dir(kind_entry.path())? {
            let node_entry = node_entry?;
            if !node_entry.file_type()?.is_dir() {
                continue;
            }

            let node_name = node_entry.file_name().to_string_lossy().to_string();
            let config_path = node_entry.path().join("config.json");
            if !config_path.is_file() {
                continue;
            }

            let config = match load_node_effective_config(&config_path) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let Some(system) = config.get("_system").and_then(|value| value.as_object()) else {
                continue;
            };
            let Some(config_runtime) = system
                .get("runtime")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if config_runtime != runtime {
                continue;
            }

            let config_runtime_version = system
                .get("runtime_version")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(expected_version) = version_filter {
                if config_runtime_version != Some(expected_version) {
                    continue;
                }
            }

            let unit = unit_from_node_name(&node_name);
            let unit_active = systemd_unit_is_active(&unit).unwrap_or(false);
            let lifecycle = if unit_active {
                if local_inventory_has_node(state, &node_name) {
                    "RUNNING"
                } else {
                    "STARTING"
                }
            } else if systemd_unit_is_failed(&unit).unwrap_or(false) {
                "FAILED"
            } else {
                "STOPPED"
            };
            if lifecycle != "RUNNING" {
                continue;
            }

            let hive = node_name
                .rsplit_once('@')
                .map(|(_, hive)| hive.to_string())
                .unwrap_or_else(|| state.hive_id.clone());
            running_nodes.push(serde_json::json!({
                "node_name": node_name,
                "hive": hive,
                "runtime": config_runtime,
                "runtime_version": config_runtime_version,
                "lifecycle": lifecycle,
            }));
        }
    }

    running_nodes.sort_by(|a, b| {
        let a_name = a
            .get("node_name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let b_name = b
            .get("node_name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        a_name.cmp(b_name)
    });

    Ok(serde_json::json!({
        "scope": "hive_local_visible",
        "in_use": !running_nodes.is_empty(),
        "running_count": running_nodes.len(),
        "running_nodes": running_nodes,
    }))
}

async fn global_visible_runtime_usage_summary(
    state: &OrchestratorState,
    runtime: &str,
    version_filter: Option<&str>,
) -> serde_json::Value {
    let local_usage = match local_runtime_usage_summary(state, runtime, version_filter) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "scope": "group_visible_control_plane",
                "status": "error",
                "message": err.to_string(),
            });
        }
    };

    if !state.is_motherbee {
        return local_usage;
    }

    let mut running_nodes = local_usage
        .get("running_nodes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut skipped_hives = Vec::new();

    for hive_id in list_managed_hive_ids() {
        if hive_id == state.hive_id {
            continue;
        }
        let mut payload = serde_json::json!({
            "target": hive_id,
            "runtime": runtime,
        });
        if let Some(version) = version_filter {
            payload["runtime_version"] = serde_json::json!(version);
        }

        match forward_system_action_to_hive(
            state,
            &hive_id,
            "GET_RUNTIME",
            "GET_RUNTIME_RESPONSE",
            payload,
        )
        .await
        {
            Ok(response) => {
                if response
                    .get("status")
                    .and_then(|value| value.as_str())
                    .is_some_and(|status| status.eq_ignore_ascii_case("ok"))
                {
                    if let Some(nodes) = response
                        .get("usage")
                        .and_then(|value| value.get("running_nodes"))
                        .and_then(|value| value.as_array())
                    {
                        running_nodes.extend(nodes.iter().cloned());
                    }
                } else {
                    skipped_hives.push(serde_json::json!({
                        "hive_id": hive_id,
                        "reason": response
                            .get("error_code")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!("RUNTIME_QUERY_FAILED")),
                    }));
                }
            }
            Err(err) => {
                skipped_hives.push(serde_json::json!({
                    "hive_id": hive_id,
                    "reason": "UNREACHABLE",
                    "message": err.to_string(),
                }));
            }
        }
    }

    running_nodes.sort_by(|a, b| {
        let a_hive = a.get("hive").and_then(|value| value.as_str()).unwrap_or("");
        let b_hive = b.get("hive").and_then(|value| value.as_str()).unwrap_or("");
        let a_name = a
            .get("node_name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let b_name = b
            .get("node_name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        (a_hive, a_name).cmp(&(b_hive, b_name))
    });

    serde_json::json!({
        "scope": "group_visible_control_plane",
        "in_use": !running_nodes.is_empty(),
        "running_count": running_nodes.len(),
        "running_nodes": running_nodes,
        "skipped_hives": skipped_hives,
    })
}

fn runtime_dependents_summary(
    manifest: &RuntimeManifest,
    runtime: &str,
) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    let runtimes = runtime_manifest_entry_map(manifest)?;
    let mut dependents = Vec::new();
    for (dependent_runtime, entry) in runtimes {
        if !matches!(runtime_package_type(&entry), "config_only" | "workflow") {
            continue;
        }
        let Some(runtime_base) = entry
            .runtime_base
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if runtime_base != runtime {
            continue;
        }
        dependents.push(serde_json::json!({
            "runtime": dependent_runtime,
            "type": runtime_package_type(&entry),
            "runtime_base": runtime_base,
            "current": entry.current,
            "available": runtime_versions_from_manifest_entry(&entry),
        }));
    }
    dependents.sort_by(|a, b| {
        let a_runtime = a
            .get("runtime")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let b_runtime = b
            .get("runtime")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        a_runtime.cmp(b_runtime)
    });
    Ok(dependents)
}

fn next_runtime_manifest_version_ms(now_ms: u64, current_manifest_version: u64) -> u64 {
    if now_ms > current_manifest_version {
        now_ms
    } else {
        current_manifest_version + 1
    }
}

fn remove_runtime_version_from_manifest(
    manifest: &RuntimeManifest,
    runtime: &str,
    runtime_version: &str,
) -> Result<RuntimeManifest, OrchestratorError> {
    let mut updated = manifest.clone();
    let runtime_entries = updated
        .runtimes
        .as_object_mut()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let entry_value = runtime_entries
        .get(runtime)
        .cloned()
        .ok_or_else(|| format!("runtime '{}' not found", runtime))?;
    let mut entry = runtime_manifest_entry_from_value(runtime, &entry_value)?;

    let current = entry.current.as_deref().map(str::trim).unwrap_or("");
    if runtime_version == "current" || (!current.is_empty() && current == runtime_version) {
        return Err("RUNTIME_CURRENT_CONFLICT".into());
    }

    let version_exists = runtime_versions_from_manifest_entry(&entry)
        .iter()
        .any(|value| value == runtime_version);
    if !version_exists {
        return Err(format!(
            "version '{}' not available for runtime '{}'",
            runtime_version, runtime
        )
        .into());
    }

    entry
        .available
        .retain(|value| value.trim() != runtime_version);
    if entry.available.is_empty() && current.is_empty() {
        runtime_entries.remove(runtime);
    } else {
        runtime_entries.insert(
            runtime.to_string(),
            serde_json::to_value(entry).map_err(|err| {
                format!(
                    "runtime manifest remove failed: serialize runtime '{}' failed: {}",
                    runtime, err
                )
            })?,
        );
    }

    let now_ms_i64 = Utc::now().timestamp_millis();
    let now_ms = u64::try_from(now_ms_i64).map_err(|_| {
        format!(
            "runtime manifest remove failed: invalid clock value {}",
            now_ms_i64
        )
    })?;
    updated.version = next_runtime_manifest_version_ms(now_ms, updated.version);
    updated.updated_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    updated.hash = None;
    Ok(updated)
}

fn quarantine_runtime_version_dir(
    runtime: &str,
    runtime_version: &str,
) -> Result<Option<(PathBuf, PathBuf)>, OrchestratorError> {
    let runtime_dir = Path::new(DIST_RUNTIME_ROOT_DIR).join(runtime);
    let version_dir = runtime_dir.join(runtime_version);
    if !version_dir.exists() {
        return Ok(None);
    }
    let quarantine = runtime_dir.join(format!(
        ".removing-{}-{}",
        sanitize_unit_suffix(runtime_version),
        Uuid::new_v4().simple()
    ));
    fs::rename(&version_dir, &quarantine).map_err(|err| {
        format!(
            "failed to quarantine runtime version dir '{}' -> '{}': {}",
            version_dir.display(),
            quarantine.display(),
            err
        )
    })?;
    Ok(Some((version_dir, quarantine)))
}

fn restore_quarantined_runtime_version_dir(
    original: &Path,
    quarantine: &Path,
) -> Result<(), OrchestratorError> {
    fs::rename(quarantine, original).map_err(|err| {
        format!(
            "failed to restore quarantined runtime version dir '{}' -> '{}': {}",
            quarantine.display(),
            original.display(),
            err
        )
        .into()
    })
}

fn finalize_quarantined_runtime_version_dir(
    runtime: &str,
    quarantine: &Path,
) -> Result<bool, OrchestratorError> {
    fs::remove_dir_all(quarantine).map_err(|err| {
        format!(
            "failed to remove quarantined runtime version dir '{}': {}",
            quarantine.display(),
            err
        )
    })?;
    let runtime_dir = Path::new(DIST_RUNTIME_ROOT_DIR).join(runtime);
    let runtime_dir_empty = runtime_dir
        .read_dir()
        .map(|mut read| read.next().is_none())
        .unwrap_or(false);
    if runtime_dir_empty {
        fs::remove_dir(&runtime_dir).map_err(|err| {
            format!(
                "failed to remove empty runtime dir '{}': {}",
                runtime_dir.display(),
                err
            )
        })?;
        return Ok(true);
    }
    Ok(false)
}

async fn remove_runtime_version_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    if !state.is_motherbee {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "runtime lifecycle owner is motherbee; execute delete on motherbee",
            "owner_hive": PRIMARY_HIVE_ID,
        });
    }

    let runtime = payload
        .get("runtime")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if runtime.is_empty() || !valid_token(runtime) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing or invalid runtime",
        });
    }
    let runtime_version = payload
        .get("runtime_version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if runtime_version.is_empty() || !valid_token(runtime_version) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing or invalid runtime_version",
        });
    }
    let test_hold_ms = payload
        .get("test_hold_ms")
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0);

    let _lifecycle_guard = match state.runtime_lifecycle_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "BUSY",
                "message": "runtime lifecycle operation already in progress",
            });
        }
    };
    if let Some(hold_ms) = test_hold_ms {
        tracing::info!(
            runtime = runtime,
            runtime_version = runtime_version,
            test_hold_ms = hold_ms,
            "runtime delete diagnostic hold active"
        );
        time::sleep(Duration::from_millis(hold_ms)).await;
    }

    let manifest = match load_runtime_manifest_result() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_MANIFEST_MISSING",
                "message": "runtime manifest not found",
            });
        }
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "MANIFEST_INVALID",
                "message": err.to_string(),
            });
        }
    };

    let runtime_map = match runtime_manifest_entry_map(&manifest) {
        Ok(map) => map,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "MANIFEST_INVALID",
                "message": err.to_string(),
            });
        }
    };
    let Some(entry) = runtime_map.get(runtime) else {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_NOT_FOUND",
            "message": format!("runtime '{}' not found", runtime),
        });
    };

    let current = entry.current.as_deref().map(str::trim).unwrap_or("");
    if runtime_version == "current" || (!current.is_empty() && current == runtime_version) {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_CURRENT_CONFLICT",
            "message": format!(
                "cannot delete current version '{}' for runtime '{}'",
                current, runtime
            ),
            "runtime": runtime,
            "runtime_version": runtime_version,
            "current": if current.is_empty() { serde_json::Value::Null } else { serde_json::json!(current) },
        });
    }

    let version_exists = runtime_versions_from_manifest_entry(entry)
        .iter()
        .any(|value| value == runtime_version);
    if !version_exists {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_VERSION_NOT_FOUND",
            "message": format!(
                "version '{}' not available for runtime '{}'",
                runtime_version, runtime
            ),
            "runtime": runtime,
            "runtime_version": runtime_version,
        });
    }

    let usage_global =
        global_visible_runtime_usage_summary(state, runtime, Some(runtime_version)).await;
    if usage_global
        .get("status")
        .and_then(|value| value.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("error"))
    {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_REMOVE_FAILED",
            "message": "failed to resolve visible runtime usage before delete",
            "runtime": runtime,
            "runtime_version": runtime_version,
            "usage_global_visible": usage_global,
        });
    }
    let running_nodes = usage_global
        .get("running_nodes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if !running_nodes.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_IN_USE",
            "message": format!(
                "runtime '{}' version '{}' is still running in visible inventory",
                runtime, runtime_version
            ),
            "runtime": runtime,
            "runtime_version": runtime_version,
            "usage_global_visible": usage_global,
        });
    }

    let dependents = match runtime_dependents_summary(&manifest, runtime) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "MANIFEST_INVALID",
                "message": err.to_string(),
            });
        }
    };
    if !dependents.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_HAS_DEPENDENTS",
            "message": format!(
                "runtime '{}' has published dependents; remove dependent runtimes first",
                runtime
            ),
            "runtime": runtime,
            "runtime_version": runtime_version,
            "dependents": dependents,
        });
    }

    let updated_manifest =
        match remove_runtime_version_from_manifest(&manifest, runtime, runtime_version) {
            Ok(value) => value,
            Err(err) if err.to_string() == "RUNTIME_CURRENT_CONFLICT" => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "RUNTIME_CURRENT_CONFLICT",
                    "message": format!(
                        "cannot delete current version '{}' for runtime '{}'",
                        runtime_version, runtime
                    ),
                    "runtime": runtime,
                    "runtime_version": runtime_version,
                });
            }
            Err(err) => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "RUNTIME_REMOVE_FAILED",
                    "message": err.to_string(),
                    "runtime": runtime,
                    "runtime_version": runtime_version,
                });
            }
        };

    let quarantined = match quarantine_runtime_version_dir(runtime, runtime_version) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_REMOVE_FAILED",
                "message": err.to_string(),
                "runtime": runtime,
                "runtime_version": runtime_version,
            });
        }
    };

    let manifest_path = Path::new(DIST_RUNTIME_MANIFEST_PATH);
    let write_v2_gate_enabled = runtime_manifest_write_v2_gate_enabled_from_env();
    if let Err(err) =
        write_runtime_manifest_file_atomic(manifest_path, &updated_manifest, write_v2_gate_enabled)
    {
        if let Some((original, quarantine)) = quarantined.as_ref() {
            let _ = restore_quarantined_runtime_version_dir(original, quarantine);
        }
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_REMOVE_FAILED",
            "message": format!("failed to update runtime manifest: {}", err),
            "runtime": runtime,
            "runtime_version": runtime_version,
        });
    }

    let removed_runtime_dir = if let Some((original, quarantine)) = quarantined.as_ref() {
        match finalize_quarantined_runtime_version_dir(runtime, quarantine) {
            Ok(removed_runtime_dir) => removed_runtime_dir,
            Err(err) => {
                let rollback_manifest = write_runtime_manifest_file_atomic(
                    manifest_path,
                    &manifest,
                    write_v2_gate_enabled,
                );
                let rollback_fs = restore_quarantined_runtime_version_dir(original, quarantine);
                return serde_json::json!({
                    "status": "error",
                    "error_code": "RUNTIME_REMOVE_FAILED",
                    "message": format!("failed to finalize runtime dir removal: {}", err),
                    "runtime": runtime,
                    "runtime_version": runtime_version,
                    "rollback_manifest": rollback_manifest.err(),
                    "rollback_fs": rollback_fs.err().map(|value| value.to_string()),
                });
            }
        }
    } else {
        false
    };

    {
        let mut guard = state.runtime_manifest.lock().await;
        *guard = Some(updated_manifest.clone());
    }
    let manifest_hash = match local_runtime_manifest_hash() {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(error = %err, "runtime manifest hash refresh failed after delete");
            None
        }
    };

    serde_json::json!({
        "status": "ok",
        "owner_hive": state.hive_id,
        "runtime": runtime,
        "removed_version": runtime_version,
        "current": updated_manifest
            .runtimes
            .get(runtime)
            .and_then(|value| value.get("current"))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "manifest_version": updated_manifest.version,
        "manifest_hash": manifest_hash,
        "removed_path": Path::new(DIST_RUNTIME_ROOT_DIR)
            .join(runtime)
            .join(runtime_version)
            .display()
            .to_string(),
        "removed_runtime_dir": removed_runtime_dir,
        "convergence_status": "owner_updated",
        "usage_global_visible": usage_global,
    })
}

async fn get_versions_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        return serde_json::json!({
            "status": "ok",
            "hive": local_versions_snapshot(state),
        });
    }

    let forwarded = forward_system_action_to_hive(
        state,
        &target_hive,
        "GET_VERSIONS",
        "GET_VERSIONS_RESPONSE",
        serde_json::json!({
            "target": target_hive,
        }),
    )
    .await;
    match forwarded {
        Ok(payload) => payload,
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "VERSIONS_FAILED",
            "message": err.to_string(),
            "target": target_hive,
        }),
    }
}

async fn list_versions_flow(state: &OrchestratorState) -> serde_json::Value {
    let mut hives = Vec::new();
    hives.push(local_versions_snapshot(state));
    for hive_id in list_managed_hive_ids() {
        if hive_id == state.hive_id {
            continue;
        }
        let response = get_versions_flow(
            state,
            &serde_json::json!({
                "target": hive_id,
            }),
        )
        .await;
        if response
            .get("status")
            .and_then(|v| v.as_str())
            .is_some_and(|status| status.eq_ignore_ascii_case("ok"))
        {
            if let Some(snapshot) = response.get("hive") {
                hives.push(snapshot.clone());
            } else {
                hives.push(serde_json::json!({
                    "hive_id": hive_id,
                    "status": "error",
                    "message": "remote GET_VERSIONS returned ok without hive payload",
                }));
            }
        } else {
            let message = response
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("versions request failed");
            hives.push(serde_json::json!({
                "hive_id": hive_id,
                "status": "error",
                "message": message,
                "error_code": response.get("error_code").cloned().unwrap_or(serde_json::Value::Null),
            }));
        }
    }
    serde_json::json!({
        "status": "ok",
        "hives": hives,
    })
}

async fn list_runtimes_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        return local_runtimes_snapshot(state);
    }

    match forward_system_action_to_hive(
        state,
        &target_hive,
        "GET_RUNTIMES",
        "GET_RUNTIMES_RESPONSE",
        serde_json::json!({
            "target": target_hive,
        }),
    )
    .await
    {
        Ok(payload) => payload,
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "RUNTIMES_FAILED",
            "message": err.to_string(),
            "target": target_hive,
        }),
    }
}

async fn get_runtime_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    let runtime = payload
        .get("runtime")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if runtime.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing runtime",
        });
    }
    if !valid_token(runtime) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "invalid runtime",
        });
    }
    let runtime_version = payload
        .get("runtime_version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(version) = runtime_version {
        if !valid_token(version) {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": "invalid runtime_version",
            });
        }
    }

    if target_hive == state.hive_id {
        return local_runtime_snapshot(state, runtime, runtime_version).await;
    }

    match forward_system_action_to_hive(
        state,
        &target_hive,
        "GET_RUNTIME",
        "GET_RUNTIME_RESPONSE",
        serde_json::json!({
            "target": target_hive,
            "runtime": runtime,
            "runtime_version": runtime_version,
        }),
    )
    .await
    {
        Ok(payload) => payload,
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_GET_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "runtime": runtime,
        }),
    }
}

fn deployment_history_path() -> PathBuf {
    json_router::paths::storage_root_dir()
        .join("orchestrator")
        .join("deployments-history.jsonl")
}

fn deployment_limit_from_payload(payload: &serde_json::Value) -> usize {
    payload
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(50)
        .clamp(1, DEPLOYMENT_HISTORY_MAX_LIMIT)
}

fn read_deployment_history(
    limit: usize,
    hive_filter: Option<&str>,
    category_filter: Option<&str>,
) -> Result<Vec<DeploymentHistoryEntry>, OrchestratorError> {
    let path = deployment_history_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<DeploymentHistoryEntry>(&line);
        let entry = match parsed {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(error = %err, "invalid deployment history line; skipping");
                continue;
            }
        };
        if let Some(category) = category_filter {
            if entry.category != category {
                continue;
            }
        }
        if let Some(hive) = hive_filter {
            let target_match = entry.target_hives.iter().any(|id| id == hive);
            let worker_match = entry.workers.iter().any(|item| item.hive_id == hive);
            if !target_match && !worker_match {
                continue;
            }
        }
        entries.push(entry);
    }

    entries.reverse();
    if entries.len() > limit {
        entries.truncate(limit);
    }
    Ok(entries)
}

fn hive_present_for_deployments(state: &OrchestratorState, hive_id: &str) -> bool {
    let hive_id = hive_id.trim();
    !hive_id.is_empty()
        && (hive_id == state.hive_id || hives_root().join(hive_id).join("info.yaml").exists())
}

fn file_mtime_epoch_ms(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

fn local_deployment_history_snapshot_entries(
    state: &OrchestratorState,
) -> Vec<DeploymentHistoryEntry> {
    let mut entries = Vec::new();
    let local_hive = state.hive_id.clone();

    if let Some(core_path) = local_core_manifest_path() {
        if let Ok(hash) = local_core_manifest_hash() {
            let started_at = file_mtime_epoch_ms(&core_path).unwrap_or_else(now_epoch_ms);
            entries.push(DeploymentHistoryEntry {
                deployment_id: format!("local-core-{}", local_hive),
                category: "core".to_string(),
                trigger: "local_current_state".to_string(),
                actor: default_deployment_actor(state),
                started_at,
                finished_at: started_at,
                manifest_version: None,
                manifest_hash: hash,
                target_hives: vec![local_hive.clone()],
                result: "ok".to_string(),
                workers: vec![DeploymentWorkerOutcome {
                    hive_id: local_hive.clone(),
                    status: "ok".to_string(),
                    reason: None,
                    duration_ms: 0,
                    local_hash: local_core_manifest_hash().ok().flatten(),
                    remote_hash_before: None,
                    remote_hash_after: None,
                }],
            });
        }
    }

    if let Some(runtime_path) = local_runtime_manifest_paths()
        .into_iter()
        .find(|path| path.exists())
    {
        if let Ok(hash) = local_runtime_manifest_hash() {
            let started_at = file_mtime_epoch_ms(&runtime_path).unwrap_or_else(now_epoch_ms);
            entries.push(DeploymentHistoryEntry {
                deployment_id: format!("local-runtime-{}", local_hive),
                category: "runtime".to_string(),
                trigger: "local_current_state".to_string(),
                actor: default_deployment_actor(state),
                started_at,
                finished_at: started_at,
                manifest_version: None,
                manifest_hash: hash,
                target_hives: vec![local_hive.clone()],
                result: "ok".to_string(),
                workers: vec![DeploymentWorkerOutcome {
                    hive_id: local_hive.clone(),
                    status: "ok".to_string(),
                    reason: None,
                    duration_ms: 0,
                    local_hash: local_runtime_manifest_hash().ok().flatten(),
                    remote_hash_before: None,
                    remote_hash_after: None,
                }],
            });
        }
    }

    if let Some(vendor_path) =
        local_vendor_manifest_path().or_else(|| resolve_syncthing_vendor_source_path().ok())
    {
        if let Ok(hash) = local_syncthing_vendor_hash() {
            let started_at = file_mtime_epoch_ms(&vendor_path).unwrap_or_else(now_epoch_ms);
            entries.push(DeploymentHistoryEntry {
                deployment_id: format!("local-vendor-{}", local_hive),
                category: "vendor".to_string(),
                trigger: "local_current_state".to_string(),
                actor: default_deployment_actor(state),
                started_at,
                finished_at: started_at,
                manifest_version: None,
                manifest_hash: hash,
                target_hives: vec![local_hive.clone()],
                result: "ok".to_string(),
                workers: vec![DeploymentWorkerOutcome {
                    hive_id: local_hive.clone(),
                    status: "ok".to_string(),
                    reason: None,
                    duration_ms: 0,
                    local_hash: local_syncthing_vendor_hash().ok().flatten(),
                    remote_hash_before: None,
                    remote_hash_after: None,
                }],
            });
        }
    }

    entries
}

fn enrich_deployment_history_entries(
    state: &OrchestratorState,
    mut entries: Vec<DeploymentHistoryEntry>,
    hive_filter: Option<&str>,
    limit: usize,
) -> Vec<serde_json::Value> {
    let local_hive = state.hive_id.as_str();
    let should_include_local_snapshot = match hive_filter {
        Some(hive) => hive == local_hive,
        None => true,
    };

    if should_include_local_snapshot {
        let local_already_present = entries
            .iter()
            .any(|entry| entry.target_hives.iter().any(|hive| hive == local_hive));
        if !local_already_present {
            entries.extend(local_deployment_history_snapshot_entries(state));
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.finished_at));
        }
    }

    if entries.len() > limit {
        entries.truncate(limit);
    }

    entries
        .into_iter()
        .map(|entry| {
            let target_hives_detail = entry
                .target_hives
                .iter()
                .map(|hive_id| {
                    serde_json::json!({
                        "hive_id": hive_id,
                        "present": hive_present_for_deployments(state, hive_id),
                    })
                })
                .collect::<Vec<_>>();
            let workers = entry
                .workers
                .iter()
                .map(|worker| {
                    let mut value =
                        serde_json::to_value(worker).unwrap_or_else(|_| serde_json::json!({}));
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert(
                            "hive_present".to_string(),
                            serde_json::json!(hive_present_for_deployments(state, &worker.hive_id)),
                        );
                    }
                    value
                })
                .collect::<Vec<_>>();
            let mut value = serde_json::to_value(entry).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = value.as_object_mut() {
                obj.insert("history_view".to_string(), serde_json::json!(true));
                obj.insert(
                    "target_hives_detail".to_string(),
                    serde_json::Value::Array(target_hives_detail),
                );
                obj.insert("workers".to_string(), serde_json::Value::Array(workers));
            }
            value
        })
        .collect()
}

fn append_deployment_history(entry: &DeploymentHistoryEntry) -> Result<(), OrchestratorError> {
    let path = deployment_history_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(entry)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn default_deployment_actor(state: &OrchestratorState) -> String {
    format!("SY.orchestrator@{}", state.hive_id)
}

fn append_single_deployment_history(
    state: &OrchestratorState,
    category: &str,
    trigger: &str,
    hive_id: &str,
    status: &str,
    reason: Option<String>,
    manifest_hash: Option<String>,
) {
    let started_at = now_epoch_ms();
    let worker = DeploymentWorkerOutcome {
        hive_id: hive_id.to_string(),
        status: status.to_string(),
        reason,
        duration_ms: 0,
        local_hash: manifest_hash.clone(),
        remote_hash_before: None,
        remote_hash_after: None,
    };
    let entry = DeploymentHistoryEntry {
        deployment_id: Uuid::new_v4().to_string(),
        category: category.to_string(),
        trigger: trigger.to_string(),
        actor: default_deployment_actor(state),
        started_at,
        finished_at: started_at,
        manifest_version: None,
        manifest_hash,
        target_hives: vec![hive_id.to_string()],
        result: if status == "ok" {
            "ok".to_string()
        } else {
            "error".to_string()
        },
        workers: vec![worker],
    };
    if let Err(err) = append_deployment_history(&entry) {
        tracing::warn!(error = %err, "failed to persist single deployment history");
    }
}

fn list_deployments_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let limit = deployment_limit_from_payload(payload);
    let category = payload
        .get("category")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match read_deployment_history(limit, None, category) {
        Ok(entries) => serde_json::json!({
            "status": "ok",
            "view": "history",
            "entries": enrich_deployment_history_entries(state, entries, None, limit),
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "DEPLOYMENTS_READ_FAILED",
            "message": err.to_string(),
        }),
    }
}

fn get_deployments_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    let limit = deployment_limit_from_payload(payload);
    let category = payload
        .get("category")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match read_deployment_history(limit, Some(&target_hive), category) {
        Ok(entries) => serde_json::json!({
            "status": "ok",
            "view": "history",
            "hive_id": target_hive,
            "entries": enrich_deployment_history_entries(state, entries, Some(&target_hive), limit),
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "DEPLOYMENTS_READ_FAILED",
            "message": err.to_string(),
            "target": target_hive,
        }),
    }
}

fn drift_alerts_path() -> PathBuf {
    json_router::paths::storage_root_dir()
        .join("orchestrator")
        .join("drift-alerts.jsonl")
}

fn drift_alert_limit_from_payload(payload: &serde_json::Value) -> usize {
    payload
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(50)
        .clamp(1, DRIFT_ALERT_MAX_LIMIT)
}

fn read_drift_alerts(
    limit: usize,
    hive_filter: Option<&str>,
    category_filter: Option<&str>,
    severity_filter: Option<&str>,
    kind_filter: Option<&str>,
) -> Result<Vec<DriftAlertEntry>, OrchestratorError> {
    let path = drift_alerts_path();
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<DriftAlertEntry>(&line);
        let entry = match parsed {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(error = %err, "invalid drift alert line; skipping");
                continue;
            }
        };
        if let Some(hive) = hive_filter {
            if entry.hive_id != hive {
                continue;
            }
        }
        if let Some(category) = category_filter {
            if entry.category != category {
                continue;
            }
        }
        if let Some(severity) = severity_filter {
            if entry.severity != severity {
                continue;
            }
        }
        if let Some(kind) = kind_filter {
            if entry.kind != kind {
                continue;
            }
        }
        entries.push(entry);
    }

    entries.reverse();
    if entries.len() > limit {
        entries.truncate(limit);
    }
    Ok(entries)
}

fn local_drift_alert_history_snapshot_entries(state: &OrchestratorState) -> Vec<DriftAlertEntry> {
    let mut entries = Vec::new();
    let local_hive = state.hive_id.clone();

    if let Some(core_path) = local_core_manifest_path() {
        let detected_at = file_mtime_epoch_ms(&core_path).unwrap_or_else(now_epoch_ms);
        entries.push(DriftAlertEntry {
            alert_id: format!("local-drift-core-{}", local_hive),
            detected_at,
            category: "core".to_string(),
            trigger: "local_current_state".to_string(),
            hive_id: local_hive.clone(),
            severity: "info".to_string(),
            kind: "local_current_state".to_string(),
            message: format!(
                "Local hive current-state snapshot for {}. No historical drift alert entry was recorded for this category.",
                local_hive
            ),
            local_hash: local_core_manifest_hash().ok().flatten(),
            remote_hash_before: None,
            remote_hash_after: None,
        });
    }

    if let Some(runtime_path) = local_runtime_manifest_paths()
        .into_iter()
        .find(|path| path.exists())
    {
        let detected_at = file_mtime_epoch_ms(&runtime_path).unwrap_or_else(now_epoch_ms);
        entries.push(DriftAlertEntry {
            alert_id: format!("local-drift-runtime-{}", local_hive),
            detected_at,
            category: "runtime".to_string(),
            trigger: "local_current_state".to_string(),
            hive_id: local_hive.clone(),
            severity: "info".to_string(),
            kind: "local_current_state".to_string(),
            message: format!(
                "Local hive current-state snapshot for {}. No historical drift alert entry was recorded for this category.",
                local_hive
            ),
            local_hash: local_runtime_manifest_hash().ok().flatten(),
            remote_hash_before: None,
            remote_hash_after: None,
        });
    }

    if let Some(vendor_path) =
        local_vendor_manifest_path().or_else(|| resolve_syncthing_vendor_source_path().ok())
    {
        let detected_at = file_mtime_epoch_ms(&vendor_path).unwrap_or_else(now_epoch_ms);
        entries.push(DriftAlertEntry {
            alert_id: format!("local-drift-vendor-{}", local_hive),
            detected_at,
            category: "vendor".to_string(),
            trigger: "local_current_state".to_string(),
            hive_id: local_hive.clone(),
            severity: "info".to_string(),
            kind: "local_current_state".to_string(),
            message: format!(
                "Local hive current-state snapshot for {}. No historical drift alert entry was recorded for this category.",
                local_hive
            ),
            local_hash: local_syncthing_vendor_hash().ok().flatten(),
            remote_hash_before: None,
            remote_hash_after: None,
        });
    }

    entries
}

fn enrich_drift_alert_history_entries(
    state: &OrchestratorState,
    mut entries: Vec<DriftAlertEntry>,
    hive_filter: Option<&str>,
    category_filter: Option<&str>,
    severity_filter: Option<&str>,
    kind_filter: Option<&str>,
    limit: usize,
) -> Vec<serde_json::Value> {
    let local_hive = state.hive_id.as_str();
    let should_include_local_snapshot = match hive_filter {
        Some(hive) => hive == local_hive,
        None => true,
    };

    if should_include_local_snapshot {
        let local_already_present = entries.iter().any(|entry| entry.hive_id == local_hive);
        if !local_already_present {
            let synthetic = local_drift_alert_history_snapshot_entries(state)
                .into_iter()
                .filter(|entry| {
                    if let Some(category) = category_filter {
                        if entry.category != category {
                            return false;
                        }
                    }
                    if let Some(severity) = severity_filter {
                        if entry.severity != severity {
                            return false;
                        }
                    }
                    if let Some(kind) = kind_filter {
                        if entry.kind != kind {
                            return false;
                        }
                    }
                    true
                });
            entries.extend(synthetic);
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.detected_at));
        }
    }

    if entries.len() > limit {
        entries.truncate(limit);
    }

    entries
        .into_iter()
        .map(|entry| {
            let mut value = serde_json::to_value(entry).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = value.as_object_mut() {
                obj.insert("history_view".to_string(), serde_json::json!(true));
                obj.insert(
                    "hive_present".to_string(),
                    serde_json::json!(hive_present_for_deployments(
                        state,
                        obj.get("hive_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                    )),
                );
                obj.insert(
                    "synthetic".to_string(),
                    serde_json::json!(obj
                        .get("trigger")
                        .and_then(|v| v.as_str())
                        .map(|v| v == "local_current_state")
                        .unwrap_or(false)),
                );
            }
            value
        })
        .collect()
}

fn drift_filter_str<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn list_drift_alerts_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let limit = drift_alert_limit_from_payload(payload);
    let category = drift_filter_str(payload, "category");
    let severity = drift_filter_str(payload, "severity");
    let kind = drift_filter_str(payload, "kind");
    match read_drift_alerts(limit, None, category, severity, kind) {
        Ok(entries) => serde_json::json!({
            "status": "ok",
            "view": "history",
            "entries": enrich_drift_alert_history_entries(
                state,
                entries,
                None,
                category,
                severity,
                kind,
                limit,
            ),
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "DRIFT_ALERTS_READ_FAILED",
            "message": err.to_string(),
        }),
    }
}

fn get_drift_alerts_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    let limit = drift_alert_limit_from_payload(payload);
    let category = drift_filter_str(payload, "category");
    let severity = drift_filter_str(payload, "severity");
    let kind = drift_filter_str(payload, "kind");
    match read_drift_alerts(limit, Some(&target_hive), category, severity, kind) {
        Ok(entries) => serde_json::json!({
            "status": "ok",
            "view": "history",
            "hive_id": target_hive,
            "entries": enrich_drift_alert_history_entries(
                state,
                entries,
                Some(&target_hive),
                category,
                severity,
                kind,
                limit,
            ),
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "DRIFT_ALERTS_READ_FAILED",
            "message": err.to_string(),
            "target": target_hive,
        }),
    }
}

fn node_kind_from_name(name: &str) -> String {
    let local = name.split('@').next().unwrap_or(name);
    let prefix = local
        .split('.')
        .next()
        .unwrap_or(local)
        .trim()
        .to_ascii_uppercase();
    match prefix.as_str() {
        "AI" | "IO" | "WF" | "SY" | "RT" => prefix,
        _ => "UNKNOWN".to_string(),
    }
}

fn managed_spawn_disallowed_reason(node_name: &str) -> Option<&'static str> {
    let local = node_name.split('@').next().unwrap_or(node_name).trim();
    let kind = node_kind_from_name(node_name);
    match kind.as_str() {
        "SY" => Some("managed spawn does not support SY.* nodes"),
        "RT" if local.eq_ignore_ascii_case("RT.gateway") => {
            Some("managed spawn does not support RT.gateway; it is core hive infrastructure")
        }
        _ => None,
    }
}

fn managed_reconcile_candidate_allowed(node_name: &str) -> bool {
    managed_spawn_disallowed_reason(node_name).is_none()
}

fn node_l2_and_hive(name: &str, default_hive: &str) -> (String, String) {
    if let Some((local, hive)) = name.rsplit_once('@') {
        let local = local.trim();
        let hive = hive.trim();
        if !local.is_empty() && !hive.is_empty() {
            return (format!("{local}@{hive}"), hive.to_string());
        }
    }
    let l2 = format!("{}@{}", name.trim(), default_hive);
    (l2, default_hive.to_string())
}

fn node_entry_to_json(entry: &NodeEntry, local_hive: &str) -> Option<serde_json::Value> {
    if entry.name_len == 0 {
        return None;
    }
    let name = node_name(entry);
    let (node_name_l2, hive) = node_l2_and_hive(&name, local_hive);
    let uuid = Uuid::from_slice(&entry.uuid).ok()?;
    Some(serde_json::json!({
        "uuid": uuid.to_string(),
        "name": name,
        "node_name": node_name_l2,
        "hive": hive,
        "kind": node_kind_from_name(&name),
        "vpn_id": entry.vpn_id,
        "connected_at": entry.connected_at,
        "status": "active",
    }))
}

fn node_name(entry: &NodeEntry) -> String {
    let len = entry.name_len as usize;
    let name_bytes = &entry.name[..len];
    String::from_utf8_lossy(name_bytes).into_owned()
}

fn remote_nodes_for_hive(snapshot: &LsaSnapshot, target_hive: &str) -> Vec<serde_json::Value> {
    let now = now_epoch_ms();
    let mut out = Vec::new();
    for node in &snapshot.nodes {
        let hive_idx = node.hive_index as usize;
        let Some(hive_entry) = snapshot.hives.get(hive_idx) else {
            continue;
        };
        let Some(hive_id) = remote_hive_name(hive_entry) else {
            continue;
        };
        if hive_id != target_hive {
            continue;
        }
        if remote_hive_is_stale(hive_entry, now) {
            continue;
        }
        if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
            continue;
        }
        let Some(node_json) =
            remote_node_to_json(node, hive_entry.last_updated, node.flags, target_hive)
        else {
            continue;
        };
        out.push(node_json);
    }
    out
}

fn remote_node_to_json(
    entry: &RemoteNodeEntry,
    connected_at: u64,
    flags: u16,
    remote_hive: &str,
) -> Option<serde_json::Value> {
    if entry.name_len == 0 {
        return None;
    }
    let len = entry.name_len as usize;
    let name = String::from_utf8_lossy(&entry.name[..len]).into_owned();
    let (node_name_l2, hive) = node_l2_and_hive(&name, remote_hive);
    let uuid = Uuid::from_slice(&entry.uuid).ok()?;
    Some(serde_json::json!({
        "uuid": uuid.to_string(),
        "name": name,
        "node_name": node_name_l2,
        "hive": hive,
        "kind": node_kind_from_name(&name),
        "vpn_id": entry.vpn_id,
        "connected_at": connected_at,
        "status": remote_flags_status(flags),
    }))
}

async fn send_config_response(
    sender: &NodeSender,
    request: &Message,
    subsystem: &str,
    version: u64,
    status: &str,
    error_code: Option<String>,
    error_detail: Option<String>,
    hive: &str,
) -> Result<(), OrchestratorError> {
    let mut payload = serde_json::json!({
        "subsystem": subsystem,
        "version": version,
        "status": status,
        "hive": hive,
    });
    if let Some(code) = error_code {
        payload["error_code"] = serde_json::Value::String(code);
    }
    if let Some(detail) = error_detail {
        payload["error_detail"] = serde_json::Value::String(detail);
    }
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: 16,
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some("CONFIG_RESPONSE".to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

fn list_hives(state: &OrchestratorState) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    let root = hives_root();
    let mut out = Vec::new();
    if root.exists() {
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let hive_id = entry.file_name().to_string_lossy().to_string();
            if let Ok(info) = read_hive_info(&root, &hive_id) {
                out.push(info);
            } else {
                out.push(serde_json::json!({ "hive_id": hive_id }));
            }
        }
    }
    if !out.iter().any(|entry| {
        entry
            .get("hive_id")
            .and_then(|value| value.as_str())
            .map(|value| value == state.hive_id)
            .unwrap_or(false)
    }) {
        let local_role = if state.is_motherbee {
            "motherbee"
        } else {
            "worker"
        };
        out.push(serde_json::json!({
            "hive_id": state.hive_id,
            "role": local_role,
            "status": "alive",
            "is_local": true,
        }));
    }
    out.sort_by(|a, b| {
        let a = a.get("hive_id").and_then(|v| v.as_str()).unwrap_or("");
        let b = b.get("hive_id").and_then(|v| v.as_str()).unwrap_or("");
        a.cmp(b)
    });
    Ok(out)
}

fn get_hive(_state_dir: &Path, hive_id: &str) -> Result<serde_json::Value, OrchestratorError> {
    let root = hives_root();
    read_hive_info(&root, hive_id)
}

fn remove_hive_cleanup_script() -> &'static str {
    "for s in rt-gateway sy-config-routes sy-opa-rules sy-identity sy-orchestrator sy-admin sy-architect sy-storage ai-frontdesk-gov fluxbee-syncthing; do \
systemctl stop --no-block \"$s\" >/dev/null 2>&1 || true; \
systemctl disable \"$s\" >/dev/null 2>&1 || true; \
systemctl kill -s KILL \"$s\" >/dev/null 2>&1 || true; \
systemctl reset-failed \"$s\" >/dev/null 2>&1 || true; \
done; \
rm -rf /var/lib/fluxbee/nodes/* >/dev/null 2>&1 || true; \
rm -rf /var/lib/fluxbee/state/nodes/* >/dev/null 2>&1 || true"
}

fn remove_hive_cleanup_shell_command(script: &str) -> String {
    sudo_wrap(&format!("bash -lc '{}'", shell_single_quote(script)))
}

fn remove_hive_cleanup_local_flow() -> serde_json::Value {
    let deferred_script = format!("sleep 1; {}", remove_hive_cleanup_script());
    let deferred_cmd = remove_hive_cleanup_shell_command(&deferred_script);
    match Command::new("bash").arg("-lc").arg(deferred_cmd).spawn() {
        Ok(_) => serde_json::json!({
            "status": "ok",
            "cleanup": "scheduled",
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "CLEANUP_SCHEDULE_FAILED",
            "message": err.to_string(),
        }),
    }
}

fn remove_hive_cleanup_via_ssh(address: &str) -> Result<(), OrchestratorError> {
    let address = address.trim();
    if address.is_empty() {
        return Err("missing worker address for remote cleanup".into());
    }
    let key_path = PathBuf::from(MOTHERBEE_SSH_KEY_PATH);
    if !key_path.exists() {
        return Err(format!(
            "motherbee ssh key missing (expected '{}')",
            key_path.display()
        )
        .into());
    }
    let cleanup_cmd = remove_hive_cleanup_shell_command(remove_hive_cleanup_script());
    ssh_with_key(address, &key_path, &cleanup_cmd, BOOTSTRAP_SSH_USER)
}

async fn remove_hive_flow(state: &OrchestratorState, hive_id: &str) -> serde_json::Value {
    if !valid_hive_id(hive_id) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_HIVE_ID",
            "message": "invalid hive_id",
        });
    }
    if hive_id == state.hive_id {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "cannot remove local motherbee hive",
        });
    }
    let root = hives_root();
    let dir = root.join(hive_id);
    if !dir.exists() {
        return serde_json::json!({
            "status": "error",
            "error_code": "NOT_FOUND",
            "message": "hive not found",
        });
    }

    let remote_cleanup: String;
    let remote_cleanup_via: String;
    let hive_info = read_hive_info(&root, hive_id).ok();
    let address = hive_info
        .as_ref()
        .and_then(|info| {
            info.get("address")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let syncthing_peer_device_id = hive_info
        .as_ref()
        .and_then(|info| info.get("syncthing_device_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| valid_syncthing_device_id(value))
        .map(str::to_string);
    let forward_result = forward_system_action_to_hive_with_timeout(
        state,
        hive_id,
        "REMOVE_HIVE_CLEANUP",
        "REMOVE_HIVE_CLEANUP_RESPONSE",
        serde_json::json!({
            "hive_id": hive_id,
            "target": hive_id,
        }),
        Duration::from_secs(REMOVE_HIVE_SOCKET_CLEANUP_TIMEOUT_SECS),
    )
    .await;

    let socket_cleanup_ok = match forward_result.as_ref() {
        Ok(payload) => payload.get("status").and_then(|value| value.as_str()) == Some("ok"),
        Err(_) => false,
    };
    let socket_cleanup_timed_out = socket_cleanup_timeout(&forward_result);

    if socket_cleanup_ok {
        let mut ssh_verified = false;
        if !address.trim().is_empty() {
            match remove_hive_cleanup_via_ssh(&address) {
                Ok(()) => {
                    ssh_verified = true;
                }
                Err(err) => {
                    tracing::warn!(
                        hive_id = hive_id,
                        address = %address,
                        error = %err,
                        "ssh verification cleanup failed after socket remove_hive cleanup"
                    );
                }
            }
        }
        if ssh_verified {
            remote_cleanup = "socket_ok_ssh_verified".to_string();
            remote_cleanup_via = "socket+ssh".to_string();
        } else {
            remote_cleanup = "socket_ok".to_string();
            remote_cleanup_via = "socket".to_string();
        }
    } else {
        let mut ssh_cleanup_ok = false;
        if !address.trim().is_empty() {
            match remove_hive_cleanup_via_ssh(&address) {
                Ok(()) => {
                    ssh_cleanup_ok = true;
                }
                Err(err) => {
                    tracing::warn!(
                        hive_id = hive_id,
                        address = %address,
                        error = %err,
                        "ssh cleanup fallback failed during remove_hive"
                    );
                }
            }
        }
        if ssh_cleanup_ok {
            remote_cleanup = if socket_cleanup_timed_out {
                "socket_timeout_ssh_ok"
            } else {
                "socket_failed_ssh_ok"
            }
            .to_string();
            remote_cleanup_via = "ssh".to_string();
        } else {
            remote_cleanup = if socket_cleanup_timed_out {
                "socket_timeout"
            } else {
                "local_only"
            }
            .to_string();
            remote_cleanup_via = "local_only".to_string();
            if let Err(err) = &forward_result {
                tracing::warn!(
                    hive_id = hive_id,
                    error = %err,
                    "socket cleanup failed during remove_hive; using local-only cleanup"
                );
            } else if let Ok(payload) = &forward_result {
                tracing::warn!(
                    hive_id = hive_id,
                    payload = %payload,
                    "socket cleanup returned non-ok status; using local-only cleanup"
                );
            }
        }
    }

    let mut syncthing_peer_cleanup = "skipped_no_peer_device_id".to_string();
    let mut syncthing_peer_unlinked = false;
    if let Some(peer_device_id) = syncthing_peer_device_id.as_deref() {
        let desired_blob = current_blob_runtime_config(state);
        let desired_dist = current_dist_runtime_config(state);
        let desired_sync = effective_syncthing_runtime_config(&desired_blob, &desired_dist);
        match ensure_syncthing_peer_unlink_runtime(
            &desired_sync,
            &desired_blob,
            &desired_dist,
            peer_device_id,
            hive_id,
        )
        .await
        {
            Ok(changed) => {
                syncthing_peer_unlinked = changed;
                syncthing_peer_cleanup = if changed {
                    "local_unlinked".to_string()
                } else {
                    "local_no_changes".to_string()
                };
            }
            Err(err) => {
                syncthing_peer_cleanup = "local_unlink_failed".to_string();
                tracing::warn!(
                    hive_id = hive_id,
                    peer_device_id = peer_device_id,
                    error = %err,
                    "failed to unlink syncthing peer during remove_hive; continuing with local cleanup"
                );
            }
        }
    }

    if let Err(err) = fs::remove_dir_all(dir) {
        return serde_json::json!({
            "status": "error",
            "error_code": "IO_ERROR",
            "message": err.to_string(),
            "hive_id": hive_id,
            "address": address,
        });
    }

    serde_json::json!({
        "status": "ok",
        "hive_id": hive_id,
        "address": address,
        "remote_cleanup": remote_cleanup,
        "remote_cleanup_via": remote_cleanup_via,
        "syncthing_peer_cleanup": syncthing_peer_cleanup,
        "syncthing_peer_unlinked": syncthing_peer_unlinked,
    })
}

fn socket_cleanup_timeout(forward_result: &Result<serde_json::Value, OrchestratorError>) -> bool {
    match forward_result {
        Ok(payload) => {
            if payload
                .get("status")
                .and_then(|value| value.as_str())
                .is_some_and(|status| status.eq_ignore_ascii_case("timeout"))
            {
                return true;
            }
            if payload
                .get("error_code")
                .and_then(|value| value.as_str())
                .is_some_and(|code| code.eq_ignore_ascii_case("TIMEOUT"))
            {
                return true;
            }
            payload
                .get("message")
                .and_then(|value| value.as_str())
                .map(|message| message.to_ascii_lowercase().contains("timeout"))
                .unwrap_or(false)
        }
        Err(err) => err.to_string().to_ascii_lowercase().contains("timeout"),
    }
}

fn read_hive_info(root: &Path, hive_id: &str) -> Result<serde_json::Value, OrchestratorError> {
    let path = root.join(hive_id).join("info.yaml");
    let data = fs::read_to_string(&path)?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let json = serde_json::to_value(yaml)?;
    Ok(json)
}

fn write_hive_info(
    root: &Path,
    hive_id: &str,
    info: &serde_json::Value,
) -> Result<(), OrchestratorError> {
    let path = root.join(hive_id).join("info.yaml");
    let yaml = serde_yaml::to_string(info)?;
    fs::write(path, yaml)?;
    Ok(())
}

fn runtime_manifest_entry_from_value(
    runtime: &str,
    value: &serde_json::Value,
) -> Result<RuntimeManifestEntry, OrchestratorError> {
    if !value.is_object() {
        return Err(format!(
            "runtime manifest invalid entry for '{}': expected object",
            runtime
        )
        .into());
    }
    serde_json::from_value::<RuntimeManifestEntry>(value.clone())
        .map_err(|err| format!("runtime manifest invalid entry for '{}': {}", runtime, err).into())
}

fn runtime_manifest_entry_map(
    manifest: &RuntimeManifest,
) -> Result<BTreeMap<String, RuntimeManifestEntry>, OrchestratorError> {
    let runtimes = manifest
        .runtimes
        .as_object()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let mut out: BTreeMap<String, RuntimeManifestEntry> = BTreeMap::new();
    for (runtime, value) in runtimes {
        if !valid_token(runtime) {
            return Err(format!("runtime manifest invalid runtime name '{}'", runtime).into());
        }
        out.insert(
            runtime.clone(),
            runtime_manifest_entry_from_value(runtime, value)?,
        );
    }
    Ok(out)
}

fn runtime_keep_versions(
    manifest: &RuntimeManifest,
) -> Result<HashMap<String, HashSet<String>>, OrchestratorError> {
    let runtimes = runtime_manifest_entry_map(manifest)?;
    let mut keep_map: HashMap<String, HashSet<String>> = HashMap::new();
    for (runtime, entry) in runtimes {
        let mut keep: HashSet<String> = HashSet::new();
        if let Some(current) = entry.current.as_deref() {
            let current = current.trim();
            if !current.is_empty() {
                if !valid_token(current) {
                    return Err(
                        format!("runtime manifest invalid current version '{}'", current).into(),
                    );
                }
                keep.insert(current.to_string());
            }
        }
        for value in &entry.available {
            let version = value.trim();
            if version.is_empty() {
                return Err(format!(
                    "runtime manifest invalid available version for runtime '{}'",
                    runtime
                )
                .into());
            }
            if !valid_token(version) {
                return Err(
                    format!("runtime manifest invalid available version '{}'", version).into(),
                );
            }
            keep.insert(version.to_string());
        }
        if !keep.is_empty() {
            keep_map.insert(runtime, keep);
        }
    }
    Ok(keep_map)
}

fn persisted_runtime_keep_versions_with_root(
    nodes_root: &Path,
) -> Result<HashMap<String, HashSet<String>>, OrchestratorError> {
    if !nodes_root.exists() {
        return Ok(HashMap::new());
    }

    let mut keep_map: HashMap<String, HashSet<String>> = HashMap::new();
    for kind_entry in fs::read_dir(nodes_root)? {
        let kind_entry = kind_entry?;
        if !kind_entry.file_type()?.is_dir() {
            continue;
        }
        for node_entry in fs::read_dir(kind_entry.path())? {
            let node_entry = node_entry?;
            if !node_entry.file_type()?.is_dir() {
                continue;
            }

            let config_path = node_entry.path().join("config.json");
            if !config_path.is_file() {
                continue;
            }
            let config = match load_node_effective_config(&config_path) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(
                        path = %config_path.display(),
                        error = %err,
                        "runtime retention skipping unreadable persisted node config"
                    );
                    continue;
                }
            };
            let Some(system) = config.get("_system").and_then(|value| value.as_object()) else {
                continue;
            };
            let Some(runtime) = system
                .get("runtime")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let Some(runtime_version) = system
                .get("runtime_version")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !valid_token(runtime) || !valid_token(runtime_version) {
                tracing::warn!(
                    path = %config_path.display(),
                    runtime = runtime,
                    runtime_version = runtime_version,
                    "runtime retention skipping persisted node with invalid runtime tokens"
                );
                continue;
            }
            keep_map
                .entry(runtime.to_string())
                .or_default()
                .insert(runtime_version.to_string());
        }
    }

    Ok(keep_map)
}

fn verify_runtime_current_artifacts(
    manifest: &RuntimeManifest,
) -> Result<Vec<String>, OrchestratorError> {
    verify_runtime_current_artifacts_with_root(manifest, Path::new(DIST_RUNTIME_ROOT_DIR))
}

fn verify_runtime_current_artifacts_with_root(
    manifest: &RuntimeManifest,
    runtimes_root: &Path,
) -> Result<Vec<String>, OrchestratorError> {
    let runtimes = runtime_manifest_entry_map(manifest)?;
    let mut errors = Vec::new();
    for (runtime, entry) in &runtimes {
        let version = entry.current.as_deref().map(str::trim).unwrap_or("");
        if version.is_empty() {
            continue;
        }
        if !valid_token(version) {
            return Err(format!("runtime manifest invalid current version '{}'", version).into());
        }

        let package_dir = runtimes_root.join(runtime).join(version);
        if !package_dir.is_dir() {
            errors.push(format!(
                "runtime package missing directory runtime='{}' version='{}' path='{}'",
                runtime,
                version,
                package_dir.display()
            ));
            continue;
        }

        match runtime_package_type(entry) {
            "full_runtime" => {
                let start_script = package_dir.join("bin/start.sh");
                if !start_script.is_file() {
                    errors.push(format!(
                        "runtime artifact missing start.sh runtime='{}' version='{}' path='{}'",
                        runtime,
                        version,
                        start_script.display()
                    ));
                    continue;
                }
                if !local_runtime_script_is_executable(&start_script) {
                    errors.push(format!(
                        "runtime artifact start.sh is not executable runtime='{}' version='{}' path='{}'",
                        runtime,
                        version,
                        start_script.display()
                    ));
                }
            }
            "config_only" | "workflow" => {
                let base_runtime = entry
                    .runtime_base
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let Some(base_runtime) = base_runtime else {
                    errors.push(format!(
                        "runtime package missing runtime_base runtime='{}' version='{}' type='{}'",
                        runtime,
                        version,
                        runtime_package_type(entry)
                    ));
                    continue;
                };
                if !valid_token(base_runtime) {
                    errors.push(format!(
                        "runtime package invalid runtime_base runtime='{}' version='{}' type='{}' runtime_base='{}'",
                        runtime,
                        version,
                        runtime_package_type(entry),
                        base_runtime
                    ));
                    continue;
                }
                let Some(base_entry) = runtimes.get(base_runtime) else {
                    errors.push(format!(
                        "runtime package runtime_base not available runtime='{}' version='{}' type='{}' runtime_base='{}'",
                        runtime,
                        version,
                        runtime_package_type(entry),
                        base_runtime
                    ));
                    continue;
                };
                let base_version = base_entry.current.as_deref().map(str::trim).unwrap_or("");
                if base_version.is_empty() || !valid_token(base_version) {
                    errors.push(format!(
                        "runtime package runtime_base has invalid current version runtime='{}' version='{}' type='{}' runtime_base='{}' base_version='{}'",
                        runtime,
                        version,
                        runtime_package_type(entry),
                        base_runtime,
                        base_version
                    ));
                    continue;
                }

                let base_start = runtimes_root
                    .join(base_runtime)
                    .join(base_version)
                    .join("bin/start.sh");
                if !base_start.is_file() {
                    errors.push(format!(
                        "runtime artifact missing base start.sh runtime='{}' version='{}' runtime_base='{}' base_version='{}' path='{}'",
                        runtime,
                        version,
                        base_runtime,
                        base_version,
                        base_start.display()
                    ));
                    continue;
                }
                if !local_runtime_script_is_executable(&base_start) {
                    errors.push(format!(
                        "runtime artifact base start.sh is not executable runtime='{}' version='{}' runtime_base='{}' base_version='{}' path='{}'",
                        runtime,
                        version,
                        base_runtime,
                        base_version,
                        base_start.display()
                    ));
                }
            }
            other => {
                errors.push(format!(
                    "runtime package unknown type runtime='{}' version='{}' type='{}'",
                    runtime, version, other
                ));
            }
        }
    }
    Ok(errors)
}

fn apply_runtime_retention_with_roots(
    manifest: &RuntimeManifest,
    runtimes_root: &Path,
    nodes_root: &Path,
) -> Result<RuntimeRetentionStats, OrchestratorError> {
    let mut keep_map = runtime_keep_versions(manifest)?;
    for (runtime, versions) in persisted_runtime_keep_versions_with_root(nodes_root)? {
        keep_map.entry(runtime).or_default().extend(versions);
    }
    let root = runtimes_root.to_path_buf();
    if !root.exists() {
        return Ok(RuntimeRetentionStats::default());
    }

    let mut stats = RuntimeRetentionStats::default();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let runtime = entry.file_name().to_string_lossy().to_string();
        if !valid_token(&runtime) {
            tracing::warn!(
                runtime = %runtime,
                "runtime retention skipping non-token directory name"
            );
            continue;
        }
        let runtime_dir = entry.path();
        let Some(keep_versions) = keep_map.get(&runtime) else {
            fs::remove_dir_all(&runtime_dir)?;
            stats.removed_runtime_dirs += 1;
            continue;
        };

        for version_entry in fs::read_dir(&runtime_dir)? {
            let version_entry = version_entry?;
            if !version_entry.file_type()?.is_dir() {
                continue;
            }
            let version = version_entry.file_name().to_string_lossy().to_string();
            if !valid_token(&version) {
                tracing::warn!(
                    runtime = %runtime,
                    version = %version,
                    "runtime retention skipping non-token version directory name"
                );
                continue;
            }
            if keep_versions.contains(&version) {
                continue;
            }
            fs::remove_dir_all(version_entry.path())?;
            stats.removed_version_dirs += 1;
        }
    }
    Ok(stats)
}

fn apply_runtime_retention(
    manifest: &RuntimeManifest,
) -> Result<RuntimeRetentionStats, OrchestratorError> {
    apply_runtime_retention_with_roots(manifest, &runtimes_root(), &node_files_root())
}

fn runtimes_root() -> PathBuf {
    PathBuf::from(DIST_RUNTIME_ROOT_DIR)
}

fn local_runtime_manifest_paths() -> Vec<PathBuf> {
    vec![PathBuf::from(DIST_RUNTIME_MANIFEST_PATH)]
}

fn local_core_manifest_path() -> Option<PathBuf> {
    let primary = PathBuf::from(DIST_CORE_MANIFEST_PATH);
    primary.exists().then_some(primary)
}

fn local_core_bin_source_path(name: &str) -> PathBuf {
    Path::new(DIST_CORE_BIN_SOURCE_DIR).join(name)
}

fn local_vendor_manifest_path() -> Option<PathBuf> {
    let primary = PathBuf::from(DIST_VENDOR_MANIFEST_PATH);
    primary.exists().then_some(primary)
}

fn local_vendor_component_path(relative_path: &str) -> Option<PathBuf> {
    let rel = relative_path.trim();
    if rel.is_empty() {
        return None;
    }
    let primary = Path::new(DIST_VENDOR_ROOT_DIR).join(rel);
    primary.exists().then_some(primary)
}

fn orchestrator_runtime_dir() -> PathBuf {
    json_router::paths::storage_root_dir().join("orchestrator")
}

fn parse_runtime_manifest_file(
    path: &Path,
    data: &str,
) -> Result<RuntimeManifest, OrchestratorError> {
    parse_runtime_manifest_file_shared(path, data).map_err(Into::into)
}

fn load_runtime_manifest_result() -> Result<Option<RuntimeManifest>, OrchestratorError> {
    load_runtime_manifest_paths_shared(&local_runtime_manifest_paths()).map_err(Into::into)
}

fn load_runtime_manifest() -> Option<RuntimeManifest> {
    match load_runtime_manifest_result() {
        Ok(manifest) => manifest,
        Err(err) => {
            tracing::warn!(error = %err, "ignoring invalid local runtime manifest");
            None
        }
    }
}

fn runtime_manifest_has_current_versions(manifest: &RuntimeManifest) -> bool {
    let runtimes = match manifest.runtimes.as_object() {
        Some(value) => value,
        None => return false,
    };
    if runtimes.is_empty() {
        return false;
    }
    runtimes.values().any(|entry| {
        entry
            .get("current")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .is_some_and(|current| !current.is_empty())
    })
}

fn local_runtime_manifest_ready_for_spawn() -> bool {
    load_runtime_manifest()
        .as_ref()
        .is_some_and(runtime_manifest_has_current_versions)
}

async fn wait_for_local_runtime_manifest_ready(timeout: Duration) -> bool {
    if local_runtime_manifest_ready_for_spawn() {
        return true;
    }
    let start = Instant::now();
    while start.elapsed() < timeout {
        time::sleep(Duration::from_millis(1000)).await;
        if local_runtime_manifest_ready_for_spawn() {
            return true;
        }
    }
    false
}

async fn should_verify_runtimes(state: &OrchestratorState) -> bool {
    let mut guard = state.last_runtime_verify.lock().await;
    if guard.elapsed() < Duration::from_secs(RUNTIME_VERIFY_INTERVAL_SECS) {
        return false;
    }
    *guard = Instant::now();
    true
}

async fn should_run_blob_gc(
    state: &OrchestratorState,
    gc_enabled: bool,
    gc_interval_secs: u64,
) -> bool {
    if !gc_enabled {
        return false;
    }
    let mut guard = state.last_blob_gc.lock().await;
    if guard.elapsed() < Duration::from_secs(gc_interval_secs.max(60)) {
        return false;
    }
    *guard = Instant::now();
    true
}

fn run_blob_gc_housekeeping(
    _state: &OrchestratorState,
    blob: &BlobRuntimeConfig,
) -> Result<(), OrchestratorError> {
    if !(blob.enabled && blob.gc_enabled) {
        return Ok(());
    }
    let toolkit = BlobToolkit::new(SdkBlobConfig {
        blob_root: blob.path.clone(),
        name_max_chars: BLOB_NAME_MAX_CHARS,
        max_blob_bytes: None,
    })?;
    let report = toolkit.run_gc(BlobGcOptions {
        staging_ttl_hours: blob.gc_staging_ttl_hours,
        active_retain_days: blob.gc_active_retain_days,
        apply: blob.gc_apply,
    })?;

    if report.staging.candidate_files > 0 || report.active.candidate_files > 0 {
        tracing::info!(
            apply = report.apply,
            staging_ttl_hours = report.staging_ttl_hours,
            active_retain_days = report.active_retain_days,
            staging_candidates = report.staging.candidate_files,
            staging_deleted = report.staging.deleted_files,
            staging_deleted_bytes = report.staging.deleted_bytes,
            active_candidates = report.active.candidate_files,
            active_deleted = report.active.deleted_files,
            active_deleted_bytes = report.active.deleted_bytes,
            "blob gc housekeeping completed"
        );
    }

    if !report.staging.errors.is_empty() || !report.active.errors.is_empty() {
        tracing::warn!(
            staging_errors = report.staging.errors.len(),
            active_errors = report.active.errors.len(),
            first_staging_error = report.staging.errors.first().cloned().unwrap_or_default(),
            first_active_error = report.active.errors.first().cloned().unwrap_or_default(),
            "blob gc housekeeping finished with errors"
        );
    }
    Ok(())
}

async fn runtime_verify_and_retain(state: &OrchestratorState) -> Result<(), OrchestratorError> {
    let _lifecycle_guard = match state.runtime_lifecycle_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            tracing::info!("watchdog verify: runtime lifecycle busy; skipping retention pass");
            return Ok(());
        }
    };
    let manifest = {
        let guard = state.runtime_manifest.lock().await;
        guard.clone()
    }
    .or_else(load_runtime_manifest);

    if let Some(manifest) = manifest {
        if let Err(err) = apply_runtime_retention(&manifest) {
            tracing::warn!(error = %err, "runtime retention failed during watchdog verify");
        }
        {
            let mut guard = state.runtime_manifest.lock().await;
            *guard = Some(manifest.clone());
        }
        tracing::info!("watchdog verify: runtime retention applied (software updates use SYSTEM_UPDATE per hive)");
    }
    Ok(())
}

fn local_runtime_manifest_hash() -> Result<Option<String>, OrchestratorError> {
    for manifest_path in local_runtime_manifest_paths() {
        if !manifest_path.exists() {
            continue;
        }
        let mut cmd = Command::new("sha256sum");
        cmd.arg(&manifest_path);
        let out = run_cmd_output(cmd, "sha256sum local manifest")?;
        let hash = out
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if !hash.is_empty() {
            return Ok(Some(hash));
        }
    }
    Ok(None)
}

fn local_core_manifest_hash() -> Result<Option<String>, OrchestratorError> {
    let Some(manifest_path) = local_core_manifest_path() else {
        return Ok(None);
    };
    let mut cmd = Command::new("sha256sum");
    cmd.arg(manifest_path);
    let out = run_cmd_output(cmd, "sha256sum local core manifest")?;
    let hash = out
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if hash.is_empty() {
        return Ok(None);
    }
    Ok(Some(hash))
}

fn remote_core_manifest_hash(
    address: &str,
    key_path: &Path,
) -> Result<Option<String>, OrchestratorError> {
    let cmd = "bash -lc \"if [ -f '/var/lib/fluxbee/dist/core/manifest.json' ]; then sha256sum '/var/lib/fluxbee/dist/core/manifest.json' | awk '{print $1}'; fi\"".to_string();
    let out = ssh_with_key_output(address, key_path, &sudo_wrap(&cmd), BOOTSTRAP_SSH_USER)?;
    let hash = out
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if hash.is_empty() {
        return Ok(None);
    }
    Ok(Some(hash))
}

fn target_hive_from_payload(payload: &serde_json::Value, local_hive: &str) -> String {
    payload
        .get("target")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("hive_id").and_then(|v| v.as_str()))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| local_hive.to_string())
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn sanitize_unit_suffix(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
}

fn valid_node_local_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn normalize_node_name_for_target(
    raw_node_name: &str,
    target_hive: &str,
) -> Result<String, OrchestratorError> {
    let raw = raw_node_name.trim();
    if raw.is_empty() {
        return Err("missing node_name".into());
    }

    if let Some((local, hive)) = raw.rsplit_once('@') {
        let local = local.trim();
        let hive = hive.trim();
        if local.is_empty() || hive.is_empty() {
            return Err("invalid node_name".into());
        }
        if !valid_node_local_name(local) {
            return Err("invalid node_name".into());
        }
        if !valid_hive_id(hive) {
            return Err("invalid node_name hive".into());
        }
        return Ok(format!("{local}@{hive}"));
    }

    if !valid_node_local_name(raw) {
        return Err("invalid node_name".into());
    }
    if !valid_hive_id(target_hive) {
        return Err("invalid target hive".into());
    }
    Ok(format!("{raw}@{target_hive}"))
}

fn validate_existing_node_name_literal(raw_node_name: &str) -> Result<String, OrchestratorError> {
    let raw = raw_node_name.trim();
    if raw.is_empty() {
        return Err("missing node_name".into());
    }

    if let Some((local, hive)) = raw.rsplit_once('@') {
        let local = local.trim();
        let hive = hive.trim();
        if local.is_empty() || hive.is_empty() {
            return Err("invalid node_name".into());
        }
        if !valid_node_local_name(local) {
            return Err("invalid node_name".into());
        }
        if !valid_hive_id(hive) {
            return Err("invalid node_name hive".into());
        }
        return Ok(format!("{local}@{hive}"));
    }

    if !valid_node_local_name(raw) {
        return Err("invalid node_name".into());
    }
    Ok(raw.to_string())
}

fn node_runtime_from_name(node_name: &str) -> Option<String> {
    let local = node_name.split('@').next().unwrap_or(node_name);
    let mut parts = local.split('.');
    let p0 = parts.next()?.trim();
    let p1 = parts.next()?.trim();
    if p0.is_empty() || p1.is_empty() {
        return None;
    }
    let runtime = format!("{p0}.{p1}");
    if valid_token(&runtime) {
        Some(runtime)
    } else {
        None
    }
}

fn ensure_private_dir(path: &Path) -> Result<(), OrchestratorError> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn node_files_root() -> PathBuf {
    PathBuf::from(NODE_FILES_ROOT)
}

fn node_instance_dir(node_name: &str) -> Result<PathBuf, OrchestratorError> {
    let local = node_name
        .rsplit_once('@')
        .map(|(local, _)| local)
        .unwrap_or(node_name)
        .trim();
    if !valid_node_local_name(local) {
        return Err(format!("invalid node_name local part '{}'", local).into());
    }
    if let Some((_, hive)) = node_name.rsplit_once('@') {
        let hive = hive.trim();
        if !valid_hive_id(hive) {
            return Err(format!("invalid node_name hive part '{}'", hive).into());
        }
    }
    let node_kind = local
        .split('.')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("invalid node_name '{}': missing kind prefix", node_name))?;
    Ok(node_files_root().join(node_kind).join(node_name))
}

fn node_effective_config_path(
    _state: &OrchestratorState,
    node_name: &str,
) -> Result<PathBuf, OrchestratorError> {
    Ok(node_instance_dir(node_name)?.join("config.json"))
}

fn runtime_package_dir_with_root(runtimes_root: &Path, runtime: &str, version: &str) -> PathBuf {
    runtimes_root.join(runtime).join(version)
}

fn runtime_package_dir(runtime: &str, version: &str) -> PathBuf {
    runtime_package_dir_with_root(Path::new(DIST_RUNTIME_ROOT_DIR), runtime, version)
}

fn runtime_package_metadata(
    package_dir: &Path,
) -> Result<Option<RuntimePackageMetadata>, OrchestratorError> {
    let package_json_path = package_dir.join("package.json");
    if !package_json_path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&package_json_path)?;
    let metadata = serde_json::from_str::<RuntimePackageMetadata>(&raw).map_err(|err| {
        format!(
            "invalid runtime package.json '{}' for config template resolution: {}",
            package_json_path.display(),
            err
        )
    })?;
    Ok(Some(metadata))
}

fn normalize_runtime_template_relative_path(raw: &str) -> Result<String, OrchestratorError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("runtime config_template cannot be empty".into());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "runtime config_template must be relative, got '{}'",
            trimmed
        )
        .into());
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "runtime config_template must not contain parent traversal, got '{}'",
            trimmed
        )
        .into());
    }
    Ok(trimmed.to_string())
}

fn runtime_template_relative_path(package_dir: &Path) -> Result<String, OrchestratorError> {
    let metadata = runtime_package_metadata(package_dir)?;
    let template_rel = metadata
        .and_then(|value| value.config_template)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "config/default-config.json".to_string());
    normalize_runtime_template_relative_path(&template_rel)
}

fn load_runtime_config_template_with_root(
    runtime: &str,
    runtime_version: &str,
    runtimes_root: &Path,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, OrchestratorError> {
    let package_dir = runtime_package_dir_with_root(runtimes_root, runtime, runtime_version);
    if !package_dir.is_dir() {
        return Ok(None);
    }
    let template_rel = runtime_template_relative_path(&package_dir)?;
    let template_path = package_dir.join(template_rel);
    if !template_path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&template_path)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        format!(
            "invalid runtime config template '{}' for runtime='{}' version='{}': {}",
            template_path.display(),
            runtime,
            runtime_version,
            err
        )
    })?;
    let object = parsed.as_object().ok_or_else(|| {
        format!(
            "invalid runtime config template '{}' for runtime='{}' version='{}': expected object",
            template_path.display(),
            runtime,
            runtime_version
        )
    })?;
    Ok(Some(object.clone()))
}

fn load_runtime_config_template(
    runtime: &str,
    runtime_version: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, OrchestratorError> {
    load_runtime_config_template_with_root(
        runtime,
        runtime_version,
        Path::new(DIST_RUNTIME_ROOT_DIR),
    )
}

fn node_state_path(
    _state: &OrchestratorState,
    node_name: &str,
) -> Result<PathBuf, OrchestratorError> {
    Ok(node_instance_dir(node_name)?.join("state.json"))
}

fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), OrchestratorError> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(tmp, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn load_node_effective_config(path: &Path) -> Result<serde_json::Value, OrchestratorError> {
    let raw = fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err(format!("invalid node config '{}': expected object", path.display()).into());
    }
    Ok(value)
}

fn parse_config_patch(
    payload: &serde_json::Value,
    field: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, OrchestratorError> {
    let config_value = payload
        .get(field)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let config = config_value
        .as_object()
        .ok_or_else(|| format!("invalid {field}: expected object"))?
        .clone();
    if config.contains_key("_system") {
        return Err(format!("invalid {field}: reserved key '_system' is not allowed").into());
    }
    Ok(config)
}

fn config_version_from_value(value: &serde_json::Value) -> u64 {
    value
        .get("_system")
        .and_then(|v| v.get("config_version"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
}

fn build_node_system_block(
    node_name: &str,
    hive_id: &str,
    runtime: Option<&str>,
    runtime_version: Option<&str>,
    runtime_base: Option<&str>,
    package_path: Option<&str>,
    ilk_id: Option<&str>,
    tenant_id: Option<&str>,
    config_version: u64,
) -> serde_json::Value {
    let mut out = serde_json::json!({
        "managed_by": "SY.orchestrator",
        "node_name": node_name,
        "hive_id": hive_id,
        "relaunch_on_boot": true,
        "config_version": config_version,
        "created_at_ms": now_epoch_ms(),
        "updated_at_ms": now_epoch_ms(),
    });
    if let Some(runtime) = runtime.filter(|v| !v.trim().is_empty()) {
        out["runtime"] = serde_json::Value::String(runtime.to_string());
    }
    if let Some(runtime_version) = runtime_version.filter(|v| !v.trim().is_empty()) {
        out["runtime_version"] = serde_json::Value::String(runtime_version.to_string());
    }
    if let Some(runtime_base) = runtime_base.filter(|v| !v.trim().is_empty()) {
        out["runtime_base"] = serde_json::Value::String(runtime_base.to_string());
    }
    if let Some(package_path) = package_path.filter(|v| !v.trim().is_empty()) {
        out["package_path"] = serde_json::Value::String(package_path.to_string());
    }
    if let Some(ilk_id) = ilk_id.filter(|v| !v.trim().is_empty()) {
        out["ilk_id"] = serde_json::Value::String(ilk_id.to_string());
    }
    if let Some(tenant_id) = tenant_id.filter(|v| !v.trim().is_empty()) {
        out["tenant_id"] = serde_json::Value::String(tenant_id.to_string());
    }
    out
}

fn ensure_node_effective_config_on_spawn(
    state: &OrchestratorState,
    payload: &serde_json::Value,
    node_name: &str,
    target_hive: &str,
    runtime: &str,
    runtime_version: &str,
    runtime_base: Option<&str>,
    package_path: Option<&str>,
    ilk_id: Option<&str>,
) -> Result<serde_json::Value, OrchestratorError> {
    let path = node_effective_config_path(state, node_name)?;
    if path.exists() {
        return Err(format!("node config already exists: {}", path.display()).into());
    }

    let template = load_runtime_config_template(runtime, runtime_version)?;
    let request_patch = parse_config_patch(payload, "config")?;
    let resolved_tenant_id = resolve_tenant_id_for_node(payload);
    let mut config = assemble_spawn_config_map(template, request_patch, resolved_tenant_id, None);
    let system_block = build_node_system_block(
        node_name,
        target_hive,
        Some(runtime),
        Some(runtime_version),
        runtime_base,
        package_path,
        ilk_id,
        config.get("tenant_id").and_then(|v| v.as_str()),
        1,
    );
    config.insert("_system".to_string(), system_block);
    write_json_atomic(&path, &serde_json::Value::Object(config))?;

    Ok(serde_json::json!({
        "status": "ok",
        "path": path.display().to_string(),
        "created": true,
        "config_version": 1,
    }))
}

fn assemble_spawn_config_map(
    template: Option<serde_json::Map<String, serde_json::Value>>,
    request_patch: serde_json::Map<String, serde_json::Value>,
    resolved_tenant_id: Option<String>,
    forced_system: Option<serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut config = template.unwrap_or_default();
    for (key, value) in request_patch {
        config.insert(key, value);
    }
    if let Some(tenant_id) = resolved_tenant_id {
        config
            .entry("tenant_id".to_string())
            .or_insert(serde_json::Value::String(tenant_id));
    }
    if let Some(system) = forced_system {
        config.insert("_system".to_string(), system);
    }
    config
}

async fn send_node_config_changed_signal(
    sender: &NodeSender,
    node_name: &str,
    config_version: u64,
    patch: serde_json::Value,
) -> Result<(), OrchestratorError> {
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(node_name.to_string()),
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_CONFIG_CHANGED.to_string()),
            src_ilk: None,
            scope: Some(SCOPE_GLOBAL.to_string()),
            target: Some(node_name.to_string()),
            action: None,
            priority: None,
            context: None,
        },
        payload: serde_json::to_value(ConfigChangedPayload {
            subsystem: "node_config".to_string(),
            action: Some("set".to_string()),
            auto_apply: Some(true),
            version: config_version,
            config: serde_json::json!({
                "node_name": node_name,
                "patch": patch,
            }),
        })?,
    };
    sender.send(msg).await?;
    Ok(())
}

fn unit_from_node_name(node_name: &str) -> String {
    format!("fluxbee-node-{}", sanitize_unit_suffix(node_name))
}

fn systemd_unit_is_active(unit: &str) -> Result<bool, OrchestratorError> {
    let status = Command::new("systemctl")
        .arg("is-active")
        .arg("--quiet")
        .arg(unit)
        .status()?;
    Ok(status.success())
}

fn resolve_runtime_version(
    manifest: &RuntimeManifest,
    runtime: &str,
    requested_version: &str,
) -> Result<String, OrchestratorError> {
    let runtimes = runtime_manifest_entry_map(manifest)?;
    let entry = runtimes
        .get(runtime)
        .ok_or_else(|| format!("runtime '{runtime}' not found in manifest"))?;

    let current = entry.current.as_deref().map(str::trim).unwrap_or("");
    let available: Vec<&str> = entry
        .available
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();

    if requested_version.is_empty() || requested_version == "current" {
        if !current.is_empty() {
            return Ok(current.to_string());
        }
        return Err(format!("runtime '{runtime}' has no current version").into());
    }

    if available.contains(&requested_version)
        || (!current.is_empty() && current == requested_version)
    {
        return Ok(requested_version.to_string());
    }
    Err(format!("version '{requested_version}' not available for runtime '{runtime}'").into())
}

fn resolve_runtime_key(
    manifest: &RuntimeManifest,
    runtime: &str,
) -> Result<String, OrchestratorError> {
    let runtimes = runtime_manifest_entry_map(manifest)?;
    if runtimes.contains_key(runtime) {
        return Ok(runtime.to_string());
    }
    Err(format!("runtime '{runtime}' not found in manifest").into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeSpawnEntrypoint {
    script_runtime: String,
    script_version: String,
    script_path: String,
    runtime_base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeSpawnEntrypointError {
    RuntimeNotAvailable(String),
    MissingRuntimeBase {
        runtime: String,
        package_type: String,
    },
    BaseRuntimeNotAvailable {
        runtime: String,
        package_type: String,
        runtime_base: String,
        reason: String,
    },
    UnknownPackageType {
        runtime: String,
        package_type: String,
    },
}

impl RuntimeSpawnEntrypointError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::RuntimeNotAvailable(_) => "RUNTIME_NOT_AVAILABLE",
            Self::MissingRuntimeBase { .. } => "MISSING_RUNTIME_BASE",
            Self::BaseRuntimeNotAvailable { .. } => "BASE_RUNTIME_NOT_AVAILABLE",
            Self::UnknownPackageType { .. } => "UNKNOWN_PACKAGE_TYPE",
        }
    }

    fn message(&self) -> String {
        match self {
            Self::RuntimeNotAvailable(message) => message.clone(),
            Self::MissingRuntimeBase {
                runtime,
                package_type,
            } => format!(
                "runtime '{}' package type '{}' requires non-empty runtime_base",
                runtime, package_type
            ),
            Self::BaseRuntimeNotAvailable {
                runtime,
                package_type,
                runtime_base,
                reason,
            } => format!(
                "runtime '{}' package type '{}' runtime_base '{}' is not available: {}",
                runtime, package_type, runtime_base, reason
            ),
            Self::UnknownPackageType {
                runtime,
                package_type,
            } => format!(
                "runtime '{}' has unknown package type '{}'",
                runtime, package_type
            ),
        }
    }
}

fn runtime_package_type(entry: &RuntimeManifestEntry) -> &str {
    entry
        .package_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("full_runtime")
}

fn resolve_runtime_spawn_entrypoint(
    manifest: &RuntimeManifest,
    runtime: &str,
    resolved_runtime_version: &str,
) -> Result<RuntimeSpawnEntrypoint, RuntimeSpawnEntrypointError> {
    let runtimes = runtime_manifest_entry_map(manifest)
        .map_err(|err| RuntimeSpawnEntrypointError::RuntimeNotAvailable(err.to_string()))?;
    let entry = runtimes.get(runtime).ok_or_else(|| {
        RuntimeSpawnEntrypointError::RuntimeNotAvailable(format!(
            "runtime '{}' not found in manifest",
            runtime
        ))
    })?;
    let package_type = runtime_package_type(entry);

    let (script_runtime, script_version, runtime_base) = match package_type {
        "full_runtime" => (
            runtime.to_string(),
            resolved_runtime_version.to_string(),
            None,
        ),
        "config_only" | "workflow" => {
            let base = entry
                .runtime_base
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| RuntimeSpawnEntrypointError::MissingRuntimeBase {
                    runtime: runtime.to_string(),
                    package_type: package_type.to_string(),
                })?;
            if !valid_token(base) {
                return Err(RuntimeSpawnEntrypointError::BaseRuntimeNotAvailable {
                    runtime: runtime.to_string(),
                    package_type: package_type.to_string(),
                    runtime_base: base.to_string(),
                    reason: format!("invalid runtime_base token '{}'", base),
                });
            }
            let base_entry = runtimes.get(base).ok_or_else(|| {
                RuntimeSpawnEntrypointError::BaseRuntimeNotAvailable {
                    runtime: runtime.to_string(),
                    package_type: package_type.to_string(),
                    runtime_base: base.to_string(),
                    reason: "runtime not found in manifest".to_string(),
                }
            })?;
            let base_current = base_entry
                .current
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| RuntimeSpawnEntrypointError::BaseRuntimeNotAvailable {
                    runtime: runtime.to_string(),
                    package_type: package_type.to_string(),
                    runtime_base: base.to_string(),
                    reason: "missing current version".to_string(),
                })?;
            if !valid_token(base_current) {
                return Err(RuntimeSpawnEntrypointError::BaseRuntimeNotAvailable {
                    runtime: runtime.to_string(),
                    package_type: package_type.to_string(),
                    runtime_base: base.to_string(),
                    reason: format!("invalid current version '{}'", base_current),
                });
            }
            (
                base.to_string(),
                base_current.to_string(),
                Some(base.to_string()),
            )
        }
        _ => {
            return Err(RuntimeSpawnEntrypointError::UnknownPackageType {
                runtime: runtime.to_string(),
                package_type: package_type.to_string(),
            });
        }
    };
    let script_path = runtime_start_script(&script_runtime, &script_version);
    Ok(RuntimeSpawnEntrypoint {
        script_runtime,
        script_version,
        script_path,
        runtime_base,
    })
}

fn runtime_start_script(runtime: &str, version: &str) -> String {
    format!("/var/lib/fluxbee/dist/runtimes/{runtime}/{version}/bin/start.sh")
}

fn local_runtime_script_exists(script_path: &str) -> bool {
    Path::new(script_path).is_file()
}

fn local_runtime_script_is_executable(script_path: &Path) -> bool {
    fs::metadata(script_path)
        .map(|meta| (meta.permissions().mode() & 0o111) != 0)
        .unwrap_or(false)
}

fn runtime_start_script_preflight(
    runtime: &str,
    version: &str,
    start_script: &str,
    target_hive: &str,
    node_name: &str,
) -> Result<(), serde_json::Value> {
    let start_script_path = PathBuf::from(start_script);
    if !local_runtime_script_exists(start_script) {
        return Err(serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_NOT_PRESENT",
            "message": format!("runtime script missing in dist path: {}", start_script),
            "runtime": runtime,
            "version": version,
            "expected_path": start_script,
            "hint": "Run SYSTEM_UPDATE to materialize this runtime on the worker",
            "target": target_hive,
            "node_name": node_name,
        }));
    }
    if !local_runtime_script_is_executable(&start_script_path) {
        return Err(serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_NOT_PRESENT",
            "message": format!("runtime script exists but is not executable: {}", start_script),
            "runtime": runtime,
            "version": version,
            "expected_path": start_script,
            "hint": "Fix permissions (chmod +x) and run SYSTEM_UPDATE",
            "target": target_hive,
            "node_name": node_name,
        }));
    }
    Ok(())
}

fn runtime_base_start_script_preflight(
    requested_runtime: &str,
    requested_version: &str,
    runtime_base: &str,
    base_version: &str,
    start_script: &str,
    target_hive: &str,
    node_name: &str,
) -> Result<(), serde_json::Value> {
    let start_script_path = PathBuf::from(start_script);
    if !local_runtime_script_exists(start_script) {
        return Err(serde_json::json!({
            "status": "error",
            "error_code": "BASE_RUNTIME_NOT_PRESENT",
            "message": format!("base runtime script missing in dist path: {}", start_script),
            "runtime": requested_runtime,
            "version": requested_version,
            "runtime_base": runtime_base,
            "base_version": base_version,
            "expected_path": start_script,
            "hint": "Run SYSTEM_UPDATE to materialize runtime_base on the worker",
            "target": target_hive,
            "node_name": node_name,
        }));
    }
    if !local_runtime_script_is_executable(&start_script_path) {
        return Err(serde_json::json!({
            "status": "error",
            "error_code": "BASE_RUNTIME_NOT_PRESENT",
            "message": format!("base runtime script exists but is not executable: {}", start_script),
            "runtime": requested_runtime,
            "version": requested_version,
            "runtime_base": runtime_base,
            "base_version": base_version,
            "expected_path": start_script,
            "hint": "Fix permissions (chmod +x) for runtime_base start.sh and run SYSTEM_UPDATE",
            "target": target_hive,
            "node_name": node_name,
        }));
    }
    Ok(())
}

fn runtime_versions_from_manifest_entry(entry: &RuntimeManifestEntry) -> Vec<String> {
    let mut versions = Vec::new();
    let mut seen = HashSet::new();
    if let Some(current) = entry
        .current
        .as_deref()
        .map(str::trim)
        .filter(|value| valid_token(value))
    {
        let current_value = current.to_string();
        seen.insert(current_value.clone());
        versions.push(current_value);
    }
    for value in &entry.available {
        let version = value.trim();
        if !valid_token(version) {
            continue;
        }
        if seen.insert(version.to_string()) {
            versions.push(version.to_string());
        }
    }
    versions
}

fn runtime_readiness_for_entry(
    runtime: &str,
    entry: &RuntimeManifestEntry,
    runtime_entries: Option<&serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Value {
    runtime_readiness_for_entry_with_root(
        runtime,
        entry,
        runtime_entries,
        Path::new(DIST_RUNTIME_ROOT_DIR),
    )
}

fn runtime_readiness_for_entry_with_root(
    runtime: &str,
    entry: &RuntimeManifestEntry,
    runtime_entries: Option<&serde_json::Map<String, serde_json::Value>>,
    runtimes_root: &Path,
) -> serde_json::Value {
    if !valid_token(runtime) {
        return serde_json::json!({});
    }
    let package_type = runtime_package_type(entry);
    let base_runtime = entry
        .runtime_base
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let base_start_script = if matches!(package_type, "config_only" | "workflow") {
        if let Some(base_runtime_name) = base_runtime {
            let base_entry = runtime_entries
                .and_then(|entries| entries.get(base_runtime_name))
                .and_then(|value| runtime_manifest_entry_from_value(base_runtime_name, value).ok());
            let base_version = base_entry
                .as_ref()
                .and_then(|value| value.current.as_deref())
                .map(str::trim)
                .filter(|value| valid_token(value));
            base_version.map(|value| {
                runtimes_root
                    .join(base_runtime_name)
                    .join(value)
                    .join("bin/start.sh")
            })
        } else {
            None
        }
    } else {
        None
    };

    let base_runtime_ready = base_start_script
        .as_ref()
        .map(|start| start.is_file() && local_runtime_script_is_executable(start))
        .unwrap_or(false);

    let mut readiness = serde_json::Map::new();
    for version in runtime_versions_from_manifest_entry(entry) {
        let package_dir = runtimes_root.join(runtime).join(&version);
        let start_script = package_dir.join("bin/start.sh");
        let (runtime_present, start_sh_executable, base_runtime_ready_field) = match package_type {
            "full_runtime" => {
                let runtime_present = start_script.is_file();
                let start_sh_executable =
                    runtime_present && local_runtime_script_is_executable(&start_script);
                (runtime_present, start_sh_executable, true)
            }
            "config_only" | "workflow" => {
                let runtime_present = package_dir.is_dir();
                (runtime_present, base_runtime_ready, base_runtime_ready)
            }
            _ => (false, false, false),
        };
        readiness.insert(
            version,
            serde_json::json!({
                "runtime_present": runtime_present,
                "start_sh_executable": start_sh_executable,
                "base_runtime_ready": base_runtime_ready_field,
            }),
        );
    }
    serde_json::Value::Object(readiness)
}

fn system_forward_timeout() -> Duration {
    let secs = std::env::var("JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(45);
    Duration::from_secs(secs)
}

fn parse_prefixed_uuid(raw: &str, prefix: &str) -> Result<Uuid, OrchestratorError> {
    let value = raw.trim();
    let expected = format!("{prefix}:");
    let Some(uuid_raw) = value.strip_prefix(&expected) else {
        return Err(format!(
            "invalid {prefix} id '{}': expected '{}<uuid>'",
            value, expected
        )
        .into());
    };
    let uuid = Uuid::parse_str(uuid_raw)?;
    Ok(uuid)
}

fn node_ilk_map_path(state: &OrchestratorState) -> PathBuf {
    state
        .state_dir
        .join("orchestrator")
        .join(IDENTITY_NODE_ILK_MAP_FILE)
}

fn load_identity_node_ilk_map(path: &Path) -> IdentityNodeIlkMap {
    let Ok(raw) = fs::read_to_string(path) else {
        return IdentityNodeIlkMap::default();
    };
    serde_json::from_str::<IdentityNodeIlkMap>(&raw).unwrap_or_default()
}

fn save_identity_node_ilk_map(
    path: &Path,
    map: &IdentityNodeIlkMap,
) -> Result<(), OrchestratorError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(map)?;
    fs::write(&tmp, body)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn resolve_node_ilk_id(
    _state: &OrchestratorState,
    payload: &serde_json::Value,
    _node_name: &str,
) -> Result<String, OrchestratorError> {
    if let Some(raw) = payload.get("ilk_id").and_then(|v| v.as_str()) {
        let normalized = raw.trim().to_string();
        parse_prefixed_uuid(&normalized, "ilk")?;
        return Ok(normalized);
    }
    Ok(format!("ilk:{}", Uuid::new_v4()))
}

fn persist_node_ilk_mapping(
    state: &OrchestratorState,
    node_name: &str,
    ilk_id: &str,
) -> Result<(), OrchestratorError> {
    parse_prefixed_uuid(ilk_id, "ilk")?;
    let map_path = node_ilk_map_path(state);
    let mut map = load_identity_node_ilk_map(&map_path);
    map.nodes.insert(node_name.to_string(), ilk_id.to_string());
    save_identity_node_ilk_map(&map_path, &map)
}

fn derive_ilk_type_for_node(node_name: &str) -> &'static str {
    if node_name.starts_with("AI.") {
        "agent"
    } else {
        "system"
    }
}

fn string_list_from_payload(payload: &serde_json::Value, key: &str) -> Vec<String> {
    payload
        .get(key)
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_tenant_id_for_node(payload: &serde_json::Value) -> Option<String> {
    let from_payload = payload
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    if from_payload.is_some() {
        return from_payload;
    }
    payload
        .get("config")
        .and_then(|cfg| cfg.get("tenant_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .or_else(|| {
            std::env::var("ORCH_DEFAULT_TENANT_ID")
                .ok()
                .map(|raw| raw.trim().to_string())
                .filter(|raw| !raw.is_empty())
        })
}

fn resolve_identity_primary_hive_id(
    state: &OrchestratorState,
) -> Result<String, OrchestratorError> {
    if state.is_motherbee && state.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid local role/hive_id: role=motherbee requires hive_id='{}' (got '{}')",
            PRIMARY_HIVE_ID, state.hive_id
        )
        .into());
    }
    if !state.is_motherbee && state.hive_id == PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid local role/hive_id: hive_id='{}' is reserved for role=motherbee",
            PRIMARY_HIVE_ID
        )
        .into());
    }
    Ok(PRIMARY_HIVE_ID.to_string())
}

fn resolve_identity_change_reason(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("identity_change_reason")
        .or_else(|| payload.get("change_reason"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
}

fn identity_update_requested(payload: &serde_json::Value) -> bool {
    !string_list_from_payload(payload, "add_roles").is_empty()
        || !string_list_from_payload(payload, "remove_roles").is_empty()
        || !string_list_from_payload(payload, "add_capabilities").is_empty()
        || !string_list_from_payload(payload, "remove_capabilities").is_empty()
        || payload
            .get("add_channels")
            .and_then(|v| v.as_array())
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        || resolve_identity_change_reason(payload).is_some()
}

fn identity_add_channels_from_payload(
    payload: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    let Some(raw) = payload.get("add_channels") else {
        return Ok(Vec::new());
    };
    let channels = raw
        .as_array()
        .ok_or_else(|| "invalid add_channels: expected array".to_string())?;
    let mut out = Vec::new();
    for channel in channels {
        let obj = channel
            .as_object()
            .ok_or_else(|| "invalid add_channels entry: expected object".to_string())?;
        let ich_id = obj
            .get("ich_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "invalid add_channels entry: missing ich_id".to_string())?;
        parse_prefixed_uuid(ich_id, "ich")?;
        let channel_type = obj
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "invalid add_channels entry: missing type".to_string())?;
        let address = obj
            .get("address")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| "invalid add_channels entry: missing address".to_string())?;
        out.push(serde_json::json!({
            "ich_id": ich_id,
            "type": channel_type,
            "address": address,
        }));
    }
    Ok(out)
}

async fn relay_system_action(
    state: &OrchestratorState,
    destination: &str,
    request_msg: &str,
    response_msg: &str,
    payload: serde_json::Value,
    forward_timeout: Duration,
) -> Result<serde_json::Value, OrchestratorError> {
    let socket_dir = json_router::paths::router_socket_dir();
    let relay_name = format!("SY.orchestrator.relay.{}", now_epoch_ms());
    let relay_config = NodeConfig {
        name: relay_name,
        router_socket: socket_dir,
        uuid_persistence_dir: state.state_dir.join("nodes"),
        uuid_mode: NodeUuidMode::Ephemeral,
        config_dir: state.config_dir.clone(),
        version: "1.0".to_string(),
    };
    let connect_timeout = Duration::from_secs(5);
    let (relay_sender, mut relay_receiver) = match time::timeout(
        connect_timeout,
        connect_with_retry(&relay_config, Duration::from_millis(100)),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "system forward timeout while connecting relay node to local router ({}s)",
                connect_timeout.as_secs()
            )
            .into());
        }
    };
    tracing::info!(
        relay_name = %relay_config.name,
        relay_uuid = %relay_sender.uuid(),
        destination = %destination,
        request_msg = %request_msg,
        "relay system action connected"
    );

    let trace_id = Uuid::new_v4().to_string();
    let request = Message {
        routing: Routing {
            src: relay_sender.uuid().to_string(),
            dst: Destination::Unicast(destination.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(request_msg.to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload,
    };
    relay_sender.send(request).await?;

    let wait_response = async {
        loop {
            let incoming = relay_receiver.recv().await?;
            if incoming.meta.msg_type != SYSTEM_KIND {
                continue;
            }
            if incoming.routing.trace_id != trace_id {
                continue;
            }
            if incoming.meta.msg.as_deref() == Some(MSG_UNREACHABLE) {
                let reason = incoming
                    .payload
                    .get("reason")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                let original_dst = incoming
                    .payload
                    .get("original_dst")
                    .and_then(|value| value.as_str())
                    .unwrap_or("-");
                return Err(format!(
                    "unreachable while waiting {} trace_id={} reason={} original_dst={}",
                    response_msg, trace_id, reason, original_dst
                )
                .into());
            }
            if incoming.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED) {
                let original_dst = incoming
                    .payload
                    .get("original_dst")
                    .and_then(|value| value.as_str())
                    .unwrap_or("-");
                let last_hop = incoming
                    .payload
                    .get("last_hop")
                    .and_then(|value| value.as_str())
                    .unwrap_or("-");
                return Err(format!(
                    "ttl exceeded while waiting {} trace_id={} original_dst={} last_hop={}",
                    response_msg, trace_id, original_dst, last_hop
                )
                .into());
            }
            if incoming.meta.msg.as_deref() != Some(response_msg) {
                continue;
            }
            return Ok::<serde_json::Value, OrchestratorError>(incoming.payload);
        }
    };

    match time::timeout(forward_timeout, wait_response).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "system forward timeout msg={} response={} target={} timeout_secs={}",
            request_msg,
            response_msg,
            destination,
            forward_timeout.as_secs()
        )
        .into()),
    }
}

async fn forward_system_action_to_hive(
    state: &OrchestratorState,
    target_hive: &str,
    request_msg: &str,
    response_msg: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, OrchestratorError> {
    forward_system_action_to_hive_with_timeout(
        state,
        target_hive,
        request_msg,
        response_msg,
        payload,
        system_forward_timeout(),
    )
    .await
}

async fn forward_system_action_to_hive_with_timeout(
    state: &OrchestratorState,
    target_hive: &str,
    request_msg: &str,
    response_msg: &str,
    payload: serde_json::Value,
    forward_timeout: Duration,
) -> Result<serde_json::Value, OrchestratorError> {
    if target_hive != state.hive_id {
        if let Ok(snapshot) = load_lsa_snapshot(state) {
            let now = now_epoch_ms();
            let hive_visible = snapshot.hives.iter().any(|entry| {
                remote_hive_name(entry).as_deref() == Some(target_hive)
                    && !remote_hive_is_stale(entry, now)
            });
            if !hive_visible {
                return Err(format!(
                    "target hive '{}' not reachable in LSA (stale/missing)",
                    target_hive
                )
                .into());
            }

            let expected_orchestrator = ensure_l2_name("SY.orchestrator", target_hive);
            let mut visible_nodes = Vec::new();
            let mut orchestrator_visible = false;
            for node in &snapshot.nodes {
                let hive_idx = node.hive_index as usize;
                let Some(hive_entry) = snapshot.hives.get(hive_idx) else {
                    continue;
                };
                let Some(hive_name) = remote_hive_name(hive_entry) else {
                    continue;
                };
                if hive_name != target_hive {
                    continue;
                }
                if remote_hive_is_stale(hive_entry, now) {
                    continue;
                }
                if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
                    continue;
                }
                if node.name_len == 0 {
                    continue;
                }
                let name =
                    String::from_utf8_lossy(&node.name[..node.name_len as usize]).into_owned();
                if name == expected_orchestrator || name == "SY.orchestrator" {
                    orchestrator_visible = true;
                }
                visible_nodes.push(name);
            }
            if !orchestrator_visible {
                visible_nodes.sort();
                visible_nodes.dedup();
                let visible = if visible_nodes.is_empty() {
                    "none".to_string()
                } else {
                    visible_nodes.join(", ")
                };
                return Err(format!(
                    "target orchestrator '{}' not visible in LSA for hive '{}' (visible: {})",
                    expected_orchestrator, target_hive, visible
                )
                .into());
            }
        }
    }

    let destination = format!("SY.orchestrator@{target_hive}");
    relay_system_action(
        state,
        &destination,
        request_msg,
        response_msg,
        payload,
        forward_timeout,
    )
    .await
}

async fn relay_identity_system_call_ok(
    state: &OrchestratorState,
    target: &str,
    action: &str,
    payload: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, IdentityError> {
    let socket_dir = json_router::paths::router_socket_dir();
    let relay_name = format!("SY.orchestrator.relay.{}", now_epoch_ms());
    let relay_config = NodeConfig {
        name: relay_name,
        router_socket: socket_dir,
        uuid_persistence_dir: state.state_dir.join("nodes"),
        uuid_mode: NodeUuidMode::Ephemeral,
        config_dir: state.config_dir.clone(),
        version: "1.0".to_string(),
    };
    let connect_timeout = Duration::from_secs(5);
    let (relay_sender, mut relay_receiver) = match time::timeout(
        connect_timeout,
        connect_with_retry(&relay_config, Duration::from_millis(100)),
    )
    .await
    {
        Ok(result) => result.map_err(IdentityError::Node)?,
        Err(_) => {
            return Err(IdentityError::InvalidResponse(format!(
                "system forward timeout while connecting relay node to local router ({}s)",
                connect_timeout.as_secs()
            )));
        }
    };
    tracing::info!(
        relay_name = %relay_config.name,
        relay_uuid = %relay_sender.uuid(),
        target = %target,
        action = %action,
        "relay identity system call connected"
    );

    let result = identity_system_call_ok(
        &relay_sender,
        &mut relay_receiver,
        IdentitySystemRequest {
            target,
            fallback_target: None,
            action,
            payload,
            timeout,
        },
    )
    .await;
    let _ = relay_sender.close().await;
    result.map(|out| out.payload)
}

fn identity_error_code_and_message(err: &IdentityError) -> (String, String) {
    match err {
        IdentityError::SystemRejected {
            error_code,
            message,
            ..
        } => (error_code.clone(), message.clone()),
        IdentityError::ProvisionRejected {
            error_code,
            message,
        } => (error_code.clone(), message.clone()),
        IdentityError::Unreachable {
            reason,
            original_dst,
        } => (
            "UNREACHABLE".to_string(),
            format!("reason={} original_dst={}", reason, original_dst),
        ),
        IdentityError::TtlExceeded {
            original_dst,
            last_hop,
        } => (
            "TTL_EXCEEDED".to_string(),
            format!("original_dst={} last_hop={}", original_dst, last_hop),
        ),
        IdentityError::Timeout {
            trace_id,
            target,
            timeout_ms,
        } => (
            "TIMEOUT".to_string(),
            format!(
                "trace_id={} target={} timeout_ms={}",
                trace_id, target, timeout_ms
            ),
        ),
        IdentityError::ActionTimeout {
            action,
            trace_id,
            target,
            timeout_ms,
        } => (
            "TIMEOUT".to_string(),
            format!(
                "action={} trace_id={} target={} timeout_ms={}",
                action, trace_id, target, timeout_ms
            ),
        ),
        IdentityError::InvalidRequest(message) => ("INVALID_REQUEST".to_string(), message.clone()),
        IdentityError::InvalidResponse(message) => {
            ("INVALID_RESPONSE".to_string(), message.clone())
        }
        IdentityError::Node(err) => ("NODE_ERROR".to_string(), err.to_string()),
        IdentityError::Json(err) => ("JSON_ERROR".to_string(), err.to_string()),
    }
}

fn map_identity_action_result(
    action_label: &str,
    result: Result<serde_json::Value, IdentityError>,
) -> Result<serde_json::Value, OrchestratorError> {
    result.map_err(|err| {
        let (error_code, message) = identity_error_code_and_message(&err);
        format!(
            "identity {} failed code={} message={}",
            action_label, error_code, message
        )
        .into()
    })
}

async fn ensure_node_identity_registered(
    state: &OrchestratorState,
    payload: &serde_json::Value,
    identity_primary_hive_id: &str,
    node_name: &str,
    runtime: &str,
    runtime_version: &str,
) -> Result<Option<serde_json::Value>, OrchestratorError> {
    if !identity_available() {
        return Err("identity registration required but sy-identity is not installed".into());
    }

    let Some(tenant_id) = resolve_tenant_id_for_node(payload) else {
        return Err("identity registration required but tenant_id is missing (use payload.tenant_id, payload.config.tenant_id, or ORCH_DEFAULT_TENANT_ID)".into());
    };
    parse_prefixed_uuid(&tenant_id, "tnt")?;

    let requested_ilk_id = resolve_node_ilk_id(state, payload, node_name)?;
    let ilk_type = derive_ilk_type_for_node(node_name);
    let roles = string_list_from_payload(payload, "roles");
    let capabilities = string_list_from_payload(payload, "capabilities");

    let request_payload = serde_json::json!({
        "ilk_id": requested_ilk_id,
        "ilk_type": ilk_type,
        "tenant_id": tenant_id,
        "identification": {
            "display_name": node_name,
            "node_name": node_name,
            "runtime": runtime,
            "runtime_version": runtime_version,
        },
        "roles": roles,
        "capabilities": capabilities,
    });
    let identity_target = format!("SY.identity@{}", identity_primary_hive_id);
    let response = relay_identity_system_call_ok(
        state,
        &identity_target,
        MSG_ILK_REGISTER,
        request_payload,
        system_forward_timeout(),
    )
    .await;
    let response = map_identity_action_result("register", response)?;

    let resolved_ilk_id = response
        .get("ilk_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| requested_ilk_id.clone());
    if let Err(err) = persist_node_ilk_mapping(state, node_name, &resolved_ilk_id) {
        tracing::warn!(
            node_name = node_name,
            ilk_id = resolved_ilk_id,
            error = %err,
            "failed to persist node->ilk mapping"
        );
    }

    Ok(Some(serde_json::json!({
        "status": "ok",
        "ilk_id": resolved_ilk_id,
        "ilk_type": ilk_type,
        "target": identity_target,
    })))
}

async fn apply_node_identity_update(
    state: &OrchestratorState,
    payload: &serde_json::Value,
    identity_primary_hive_id: &str,
    ilk_id: &str,
) -> Result<Option<serde_json::Value>, OrchestratorError> {
    parse_prefixed_uuid(ilk_id, "ilk")?;
    let add_roles = string_list_from_payload(payload, "add_roles");
    let remove_roles = string_list_from_payload(payload, "remove_roles");
    let add_capabilities = string_list_from_payload(payload, "add_capabilities");
    let remove_capabilities = string_list_from_payload(payload, "remove_capabilities");
    let add_channels = identity_add_channels_from_payload(payload)?;
    let change_reason = resolve_identity_change_reason(payload);

    let has_changes = !add_roles.is_empty()
        || !remove_roles.is_empty()
        || !add_capabilities.is_empty()
        || !remove_capabilities.is_empty()
        || !add_channels.is_empty()
        || change_reason.is_some();
    if !has_changes {
        return Ok(Some(serde_json::json!({
            "status": "skipped",
            "reason": "no_changes",
        })));
    }

    let request_payload = serde_json::json!({
        "ilk_id": ilk_id,
        "add_channels": add_channels,
        "add_roles": add_roles,
        "remove_roles": remove_roles,
        "add_capabilities": add_capabilities,
        "remove_capabilities": remove_capabilities,
        "change_reason": change_reason,
    });
    let identity_target = format!("SY.identity@{}", identity_primary_hive_id);
    let update_result = relay_identity_system_call_ok(
        state,
        &identity_target,
        MSG_ILK_UPDATE,
        request_payload,
        system_forward_timeout(),
    )
    .await;
    let _ = map_identity_action_result("update", update_result)?;

    Ok(Some(serde_json::json!({
        "status": "ok",
        "ilk_id": ilk_id,
        "target": identity_target,
    })))
}

async fn get_node_config_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    let raw_node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw_node_name.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing node_name",
        });
    }
    if let Some((_, hive)) = raw_node_name.rsplit_once('@') {
        if !hive.trim().is_empty() {
            target_hive = hive.trim().to_string();
        }
    }
    let node_name = match validate_existing_node_name_literal(raw_node_name) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    if target_hive != state.hive_id {
        let mut forwarded_payload = payload.clone();
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("target".to_string(), serde_json::json!(target_hive));
            obj.insert("node_name".to_string(), serde_json::json!(node_name));
        }
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "NODE_CONFIG_GET",
            "NODE_CONFIG_GET_RESPONSE",
            forwarded_payload,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "NODE_CONFIG_GET_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            }),
        };
    }

    let path = match node_effective_config_path(state, &node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };
    if !path.exists() {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_CONFIG_NOT_FOUND",
            "message": "effective config file not found",
            "target": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
        });
    }

    match load_node_effective_config(&path) {
        Ok(config) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "hive": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
            "config_version": config_version_from_value(&config),
            "config": config,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "NODE_CONFIG_READ_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
        }),
    }
}

async fn get_node_state_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    let raw_node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw_node_name.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing node_name",
        });
    }
    if let Some((_, hive)) = raw_node_name.rsplit_once('@') {
        if !hive.trim().is_empty() {
            target_hive = hive.trim().to_string();
        }
    }
    let node_name = match validate_existing_node_name_literal(raw_node_name) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    if target_hive != state.hive_id {
        let mut forwarded_payload = payload.clone();
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("target".to_string(), serde_json::json!(target_hive));
            obj.insert("node_name".to_string(), serde_json::json!(node_name));
        }
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "NODE_STATE_GET",
            "NODE_STATE_GET_RESPONSE",
            forwarded_payload,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "NODE_STATE_GET_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            }),
        };
    }

    let path = match node_state_path(state, &node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };
    if !path.exists() {
        return serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "hive": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
            "state": serde_json::Value::Null,
        });
    }

    match fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|raw| {
            serde_json::from_str::<serde_json::Value>(&raw).map_err(|err| err.to_string())
        }) {
        Ok(state_payload) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "hive": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
            "state": state_payload,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "NODE_STATE_READ_FAILED",
            "message": err,
            "target": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
        }),
    }
}

fn node_status_version_path(node_name: &str) -> Result<PathBuf, OrchestratorError> {
    Ok(node_instance_dir(node_name)?.join("status_version"))
}

fn node_status_fingerprint_path(node_name: &str) -> Result<PathBuf, OrchestratorError> {
    Ok(node_instance_dir(node_name)?.join("status_fingerprint.sha256"))
}

fn read_node_status_version(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn write_private_file_atomic(path: &Path, contents: &[u8]) -> Result<(), OrchestratorError> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&tmp, contents)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, &path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn status_fingerprint_hash(value: &serde_json::Value) -> Result<String, OrchestratorError> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn advance_node_status_version(
    node_name: &str,
    fingerprint: &serde_json::Value,
) -> Result<u64, OrchestratorError> {
    let version_path = node_status_version_path(node_name)?;
    let fingerprint_path = node_status_fingerprint_path(node_name)?;
    if let Some(parent) = version_path.parent() {
        ensure_private_dir(parent)?;
    }

    let current = read_node_status_version(&version_path);
    let new_fingerprint = status_fingerprint_hash(fingerprint)?;
    let old_fingerprint = fs::read_to_string(&fingerprint_path)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty());

    let next = if current == 0 {
        1
    } else if old_fingerprint.as_deref() == Some(new_fingerprint.as_str()) {
        current
    } else {
        current.saturating_add(1)
    };

    if current != next || !version_path.exists() {
        write_private_file_atomic(&version_path, next.to_string().as_bytes())?;
    }
    if old_fingerprint.as_deref() != Some(new_fingerprint.as_str()) || !fingerprint_path.exists() {
        write_private_file_atomic(&fingerprint_path, new_fingerprint.as_bytes())?;
    }

    Ok(next)
}

fn local_inventory_has_node(state: &OrchestratorState, expected_node_name: &str) -> bool {
    let Ok(snapshot) = load_lsa_snapshot(state) else {
        return false;
    };
    let now = now_epoch_ms();
    snapshot.nodes.iter().any(|entry| {
        if entry.name_len == 0 {
            return false;
        }
        let hive_idx = entry.hive_index as usize;
        let Some(hive_entry) = snapshot.hives.get(hive_idx) else {
            return false;
        };
        let Some(hive_name) = remote_hive_name(hive_entry) else {
            return false;
        };
        if hive_name != state.hive_id {
            return false;
        }
        if remote_hive_is_stale(hive_entry, now) {
            return false;
        }
        if entry.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
            return false;
        }
        let name = String::from_utf8_lossy(&entry.name[..entry.name_len as usize]).into_owned();
        let (node_name_l2, hive) = node_l2_and_hive(&name, &state.hive_id);
        hive == state.hive_id && node_name_l2 == expected_node_name
    })
}

fn systemd_unit_is_failed(unit: &str) -> Result<bool, OrchestratorError> {
    let status = Command::new("systemctl")
        .arg("is-failed")
        .arg("--quiet")
        .arg(unit)
        .status()?;
    Ok(status.success())
}

fn epoch_ms_to_iso_utc(epoch_ms: u64) -> Option<String> {
    let secs = (epoch_ms / 1000) as i64;
    let nanos = ((epoch_ms % 1000) * 1_000_000) as u32;
    let ts = Utc.timestamp_opt(secs, nanos).single()?;
    Some(ts.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn parse_systemd_u64(value: Option<&String>) -> Option<u64> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| raw.parse::<u64>().ok())
}

fn parse_systemd_i32(value: Option<&String>) -> Option<i32> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| raw.parse::<i32>().ok())
}

fn systemd_unit_show(unit: &str, properties: &[&str]) -> HashMap<String, String> {
    if properties.is_empty() {
        return HashMap::new();
    }
    let property_arg = format!("--property={}", properties.join(","));
    let output = Command::new("systemctl")
        .arg("show")
        .arg(unit)
        .arg(property_arg)
        .output();
    let Ok(output) = output else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn normalize_reported_health_state(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => Some("HEALTHY"),
        "DEGRADED" => Some("DEGRADED"),
        "ERROR" => Some("ERROR"),
        "UNKNOWN" => Some("UNKNOWN"),
        _ => None,
    }
}

fn extract_node_status_payload_value<'a>(
    response: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    response
        .get(key)
        .or_else(|| response.get("payload").and_then(|payload| payload.get(key)))
}

fn extract_reported_health_state(response: &serde_json::Value) -> Option<&'static str> {
    let raw = extract_node_status_payload_value(response, "health_state")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    normalize_reported_health_state(raw)
}

async fn get_node_status_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    let raw_node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw_node_name.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing node_name",
        });
    }
    if let Some((_, hive)) = raw_node_name.rsplit_once('@') {
        if !hive.trim().is_empty() {
            target_hive = hive.trim().to_string();
        }
    }
    let node_name = match validate_existing_node_name_literal(raw_node_name) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    if target_hive != state.hive_id {
        let mut forwarded_payload = payload.clone();
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("target".to_string(), serde_json::json!(target_hive));
            obj.insert("node_name".to_string(), serde_json::json!(node_name));
        }
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "NODE_STATUS_GET",
            "NODE_STATUS_GET_RESPONSE",
            forwarded_payload,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "NODE_STATUS_GET_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            }),
        };
    }

    let config_path = match node_effective_config_path(state, &node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };
    let state_path = match node_state_path(state, &node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };

    let config_exists = config_path.exists();
    let state_exists = state_path.exists();
    let unit = unit_from_node_name(&node_name);
    let unit_active = systemd_unit_is_active(&unit).unwrap_or(false);
    let unit_failed = systemd_unit_is_failed(&unit).unwrap_or(false);
    let inventory_visible = local_inventory_has_node(state, &node_name);

    if !config_exists && !state_exists && !unit_active && !unit_failed && !inventory_visible {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_NOT_FOUND",
            "message": format!("node '{}' not found", node_name),
            "target": target_hive,
            "node_name": node_name,
        });
    }

    let mut config_valid = false;
    let mut config_version: Option<u64> = None;
    let mut config_updated_at_ms: Option<u64> = None;
    let mut runtime_requested_version: Option<String> = None;
    let mut runtime_name: Option<String> = None;
    let mut runtime_version: Option<String> = None;
    let mut identity_ilk_id: Option<String> = None;
    let mut identity_tenant_id: Option<String> = None;
    if config_exists {
        match load_node_effective_config(&config_path) {
            Ok(config_payload) => {
                config_valid = true;
                config_version = Some(config_version_from_value(&config_payload));
                if let Some(system) = config_payload.get("_system").and_then(|v| v.as_object()) {
                    config_updated_at_ms = system.get("updated_at_ms").and_then(|v| v.as_u64());
                    runtime_requested_version = system
                        .get("requested_runtime_version")
                        .or_else(|| system.get("requested_version"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    runtime_name = system
                        .get("runtime")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    runtime_version = system
                        .get("runtime_version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    identity_ilk_id = system
                        .get("ilk_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    identity_tenant_id = system
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                if identity_tenant_id.is_none() {
                    identity_tenant_id = config_payload
                        .get("tenant_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
            Err(err) => {
                tracing::debug!(
                    node_name = node_name,
                    error = %err,
                    "unable to parse node config while building status"
                );
            }
        }
    }

    let mut state_payload: Option<serde_json::Value> = None;
    if state_exists {
        match fs::read_to_string(&state_path)
            .map_err(|err| err.to_string())
            .and_then(|raw| {
                serde_json::from_str::<serde_json::Value>(&raw).map_err(|err| err.to_string())
            }) {
            Ok(value) => {
                state_payload = Some(value);
            }
            Err(err) => {
                tracing::debug!(
                    node_name = node_name,
                    error = %err,
                    "unable to parse node state while building status"
                );
            }
        }
    }

    let mut last_error = state_payload
        .as_ref()
        .and_then(|value| value.get("last_error"))
        .filter(|value| !value.is_null())
        .cloned();
    let mut extensions = state_payload
        .as_ref()
        .and_then(|value| value.get("extensions"))
        .filter(|value| !value.is_null())
        .cloned();

    let lifecycle_state = if unit_active {
        if inventory_visible {
            "RUNNING"
        } else {
            "STARTING"
        }
    } else if unit_failed {
        "FAILED"
    } else if config_exists || state_exists {
        "STOPPED"
    } else {
        "UNKNOWN"
    };

    let (fallback_health_state, fallback_health_source) = if lifecycle_state == "RUNNING" {
        if !config_exists || !config_valid {
            ("ERROR", "ORCHESTRATOR_INFERRED")
        } else if last_error.is_some() {
            ("DEGRADED", "ORCHESTRATOR_INFERRED")
        } else {
            ("HEALTHY", "ORCHESTRATOR_INFERRED")
        }
    } else {
        ("UNKNOWN", "UNKNOWN")
    };
    let mut health_state = fallback_health_state.to_string();
    let mut health_source = fallback_health_source.to_string();

    if lifecycle_state == "RUNNING" {
        let node_status_request = serde_json::json!({
            "node_name": node_name,
        });
        match relay_system_action(
            state,
            &node_name,
            "NODE_STATUS_GET",
            "NODE_STATUS_GET_RESPONSE",
            node_status_request,
            Duration::from_secs(NODE_STATUS_FORWARD_TIMEOUT_SECS),
        )
        .await
        {
            Ok(response) => {
                if let Some(reported) = extract_reported_health_state(&response) {
                    health_state = reported.to_string();
                    health_source = "NODE_REPORTED".to_string();
                }
                if let Some(value) = extract_node_status_payload_value(&response, "last_error")
                    .filter(|value| !value.is_null())
                    .cloned()
                {
                    last_error = Some(value);
                }
                if let Some(value) = extract_node_status_payload_value(&response, "extensions")
                    .filter(|value| !value.is_null())
                    .cloned()
                {
                    extensions = Some(value);
                }
            }
            Err(err) => {
                tracing::debug!(
                    node_name = node_name,
                    error = %err,
                    "node status request timed out/unreachable; using inferred fallback"
                );
            }
        }
    }

    let systemd_props = systemd_unit_show(
        &unit,
        &[
            "MainPID",
            "ExecMainStatus",
            "NRestarts",
            "ActiveEnterTimestampUSec",
        ],
    );
    let process_pid = parse_systemd_u64(systemd_props.get("MainPID")).filter(|value| *value > 0);
    let process_exit_code = parse_systemd_i32(systemd_props.get("ExecMainStatus"));
    let process_restart_count = parse_systemd_u64(systemd_props.get("NRestarts"));
    let process_started_at = parse_systemd_u64(systemd_props.get("ActiveEnterTimestampUSec"))
        .and_then(|usec| {
            if usec == 0 {
                None
            } else {
                epoch_ms_to_iso_utc(usec / 1000)
            }
        });
    let observed_at_ms = now_epoch_ms();
    let observed_at =
        epoch_ms_to_iso_utc(observed_at_ms).unwrap_or_else(|| observed_at_ms.to_string());
    let config_updated_at = config_updated_at_ms
        .and_then(epoch_ms_to_iso_utc)
        .or_else(|| config_updated_at_ms.map(|value| value.to_string()));

    let status_fingerprint = serde_json::json!({
        "lifecycle_state": lifecycle_state,
        "health_state": health_state,
        "health_source": health_source,
        "runtime_requested_version": runtime_requested_version.clone(),
        "runtime_name": runtime_name.clone(),
        "runtime_version": runtime_version.clone(),
        "config_exists": config_exists,
        "config_valid": config_valid,
        "config_version": config_version,
        "state_exists": state_exists,
        "process_pid": process_pid,
        "process_exit_code": process_exit_code,
        "process_restart_count": process_restart_count,
        "process_started_at": process_started_at.clone(),
        "identity_ilk_id": identity_ilk_id.clone(),
        "identity_tenant_id": identity_tenant_id.clone(),
        "last_error": last_error.clone(),
    });
    let status_version = match advance_node_status_version(&node_name, &status_fingerprint) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                node_name = node_name,
                error = %err,
                "failed to persist node status version"
            );
            1
        }
    };
    let mut node_status = serde_json::json!({
        "schema_version": NODE_STATUS_SCHEMA_VERSION,
        "node_name": node_name,
        "hive_id": target_hive,
        "observed_at": observed_at,
        "lifecycle_state": lifecycle_state,
        "health_state": health_state,
        "health_source": health_source,
        "status_version": status_version,
        "runtime": {
            "name": runtime_name,
            "requested_version": runtime_requested_version,
            "resolved_version": runtime_version,
        },
        "process": {
            "unit": unit,
            "pid": process_pid,
            "exit_code": process_exit_code,
            "restart_count": process_restart_count,
            "started_at": process_started_at,
        },
        "config": {
            "path": config_path.display().to_string(),
            "exists": config_exists,
            "valid": config_valid,
            "config_version": config_version,
            "updated_at": config_updated_at,
        },
        "state": {
            "path": state_path.display().to_string(),
            "exists": state_exists,
        },
        "identity": {
            "ilk_id": identity_ilk_id,
            "tenant_id": identity_tenant_id,
        },
    });
    if let Some(value) = last_error {
        node_status["last_error"] = value;
    }
    if let Some(value) = extensions {
        node_status["extensions"] = value;
    }

    serde_json::json!({
        "status": "ok",
        "target": target_hive,
        "hive": target_hive,
        "node_name": node_name,
        "node_status": node_status,
    })
}

async fn set_node_config_flow(
    sender: &NodeSender,
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    let raw_node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if raw_node_name.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing node_name",
        });
    }
    if let Some((_, hive)) = raw_node_name.rsplit_once('@') {
        if !hive.trim().is_empty() {
            target_hive = hive.trim().to_string();
        }
    }
    let node_name = match validate_existing_node_name_literal(raw_node_name) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    if target_hive != state.hive_id {
        let mut forwarded_payload = payload.clone();
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("target".to_string(), serde_json::json!(target_hive));
            obj.insert("node_name".to_string(), serde_json::json!(node_name));
        }
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "NODE_CONFIG_SET",
            "NODE_CONFIG_SET_RESPONSE",
            forwarded_payload,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "NODE_CONFIG_SET_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            }),
        };
    }

    let patch = match parse_config_patch(payload, "config") {
        Ok(cfg) => cfg,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };
    let replace = payload
        .get("replace")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let notify = payload
        .get("notify")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let path = match node_effective_config_path(state, &node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };

    let mut config = if path.exists() {
        match load_node_effective_config(&path) {
            Ok(value) => value,
            Err(err) => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "NODE_CONFIG_READ_FAILED",
                    "message": err.to_string(),
                    "target": target_hive,
                    "node_name": node_name,
                    "path": path.display().to_string(),
                });
            }
        }
    } else {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_CONFIG_NOT_FOUND",
            "message": "config.json not found for node",
            "target": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
        });
    };
    let next_version = config_version_from_value(&config).saturating_add(1);
    let Some(config_obj) = config.as_object_mut() else {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_CONFIG_INVALID",
            "message": "effective config root must be object",
            "target": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
        });
    };

    if replace {
        let existing_system = config_obj
            .get("_system")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        config_obj.clear();
        config_obj.extend(patch);
        config_obj.insert("_system".to_string(), existing_system);
    } else {
        for (key, value) in patch {
            config_obj.insert(key, value);
        }
    }

    let mut system = config_obj
        .remove("_system")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    system.insert(
        "managed_by".to_string(),
        serde_json::json!("SY.orchestrator"),
    );
    system.insert("node_name".to_string(), serde_json::json!(node_name));
    system.insert("hive_id".to_string(), serde_json::json!(target_hive));
    system.insert(
        "config_version".to_string(),
        serde_json::json!(next_version),
    );
    system.insert(
        "updated_at_ms".to_string(),
        serde_json::json!(now_epoch_ms()),
    );
    if !system.contains_key("created_at_ms") {
        system.insert(
            "created_at_ms".to_string(),
            serde_json::json!(now_epoch_ms()),
        );
    }
    config_obj.insert("_system".to_string(), serde_json::Value::Object(system));

    if let Err(err) = write_json_atomic(&path, &config) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_WRITE_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": node_name,
            "path": path.display().to_string(),
        });
    }

    let notify_status = if notify {
        let patch_payload = payload
            .get("config")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        match send_node_config_changed_signal(sender, &node_name, next_version, patch_payload).await
        {
            Ok(()) => "sent".to_string(),
            Err(err) => {
                tracing::warn!(
                    node_name = node_name,
                    error = %err,
                    "failed to dispatch node CONFIG_CHANGED signal"
                );
                format!("dispatch_failed: {}", err)
            }
        }
    } else {
        "skipped".to_string()
    };

    serde_json::json!({
        "status": "ok",
        "target": target_hive,
        "hive": target_hive,
        "node_name": node_name,
        "path": path.display().to_string(),
        "config_version": next_version,
        "notify_status": notify_status,
    })
}

async fn run_node_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    if payload.get("name").is_some() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "legacy field 'name' is not supported; use 'node_name'",
        });
    }
    if payload.get("version").is_some() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "legacy field 'version' is not supported; use 'runtime_version'",
        });
    }

    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    let raw_node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if let Some((_, hive)) = raw_node_name.rsplit_once('@') {
        if !hive.trim().is_empty() {
            target_hive = hive.trim().to_string();
        }
    }
    let node_name = match normalize_node_name_for_target(raw_node_name, &target_hive) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };
    if let Some(message) = managed_spawn_disallowed_reason(&node_name) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": message,
            "target": target_hive,
            "node_name": node_name,
        });
    }
    let identity_primary_hive_id = match resolve_identity_primary_hive_id(state) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    let runtime = payload
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .or_else(|| node_runtime_from_name(&node_name))
        .unwrap_or_default();
    if runtime.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing runtime (or runtime not derivable from node_name)",
        });
    }
    if !valid_token(&runtime) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "invalid runtime",
        });
    }

    let requested_version = payload
        .get("runtime_version")
        .and_then(|v| v.as_str())
        .unwrap_or("current")
        .trim();
    if !valid_token(requested_version) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "invalid runtime_version",
        });
    }

    let unit = payload
        .get("unit")
        .and_then(|v| v.as_str())
        .map(|v| sanitize_unit_suffix(v.trim()))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| unit_from_node_name(&node_name));

    if target_hive != state.hive_id {
        let mut forwarded_payload = payload.clone();
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("target".to_string(), serde_json::json!(target_hive));
            obj.insert("node_name".to_string(), serde_json::json!(node_name));
            obj.insert("runtime".to_string(), serde_json::json!(runtime));
            obj.insert(
                "runtime_version".to_string(),
                serde_json::json!(requested_version),
            );
            obj.insert("unit".to_string(), serde_json::json!(unit));
        }
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "SPAWN_NODE",
            "SPAWN_NODE_RESPONSE",
            forwarded_payload,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "SPAWN_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
                "unit": unit,
            }),
        };
    }

    let manifest = match load_runtime_manifest_result() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_MANIFEST_MISSING",
                "message": "runtime manifest not found",
            });
        }
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "MANIFEST_INVALID",
                "message": err.to_string(),
            });
        }
    };
    let runtime_key = match resolve_runtime_key(&manifest, &runtime) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_NOT_AVAILABLE",
                "message": err.to_string(),
            });
        }
    };

    let version = match resolve_runtime_version(&manifest, &runtime_key, requested_version) {
        Ok(version) => version,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_NOT_AVAILABLE",
                "message": err.to_string(),
            });
        }
    };

    let entrypoint = match resolve_runtime_spawn_entrypoint(&manifest, &runtime_key, &version) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": err.error_code(),
                "message": err.message(),
            });
        }
    };
    if let Some(runtime_base) = entrypoint.runtime_base.as_deref() {
        if let Err(payload) = runtime_base_start_script_preflight(
            &runtime_key,
            &version,
            runtime_base,
            &entrypoint.script_version,
            &entrypoint.script_path,
            &target_hive,
            &node_name,
        ) {
            return payload;
        }
    } else if let Err(payload) = runtime_start_script_preflight(
        &entrypoint.script_runtime,
        &entrypoint.script_version,
        &entrypoint.script_path,
        &target_hive,
        &node_name,
    ) {
        return payload;
    }

    let identity_register = match ensure_node_identity_registered(
        state,
        payload,
        &identity_primary_hive_id,
        &node_name,
        &runtime_key,
        &version,
    )
    .await
    {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "IDENTITY_REGISTER_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
                "unit": unit,
            });
        }
    };
    let identity_ilk_id = identity_register
        .as_ref()
        .and_then(|value| value.get("ilk_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let identity_update = match identity_ilk_id.as_deref() {
        Some(ilk_id) => {
            match apply_node_identity_update(state, payload, &identity_primary_hive_id, ilk_id)
                .await
            {
                Ok(value) => value,
                Err(err) => {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "IDENTITY_UPDATE_FAILED",
                        "message": err.to_string(),
                        "target": target_hive,
                        "node_name": node_name,
                        "unit": unit,
                    });
                }
            }
        }
        None => {
            if identity_update_requested(payload) {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "IDENTITY_UPDATE_FAILED",
                    "message": "identity update requested but no ilk_id is available",
                    "target": target_hive,
                    "node_name": node_name,
                    "unit": unit,
                });
            }
            Some(serde_json::json!({
                "status": "skipped",
                "reason": "missing_ilk_id",
            }))
        }
    };
    let identity = serde_json::json!({
        "requested_hive": target_hive,
        "identity_primary_hive_id": identity_primary_hive_id,
        "identity_target": format!("SY.identity@{}", identity_primary_hive_id),
        "register": identity_register,
        "update": identity_update,
    });
    let config_path = match node_effective_config_path(state, &node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
                "unit": unit,
                "identity": identity,
            });
        }
    };
    if config_path.exists() {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_ALREADY_EXISTS",
            "message": format!("node config already exists: {}", config_path.display()),
            "target": target_hive,
            "node_name": node_name,
            "unit": unit,
            "identity": identity,
            "config": {
                "path": config_path.display().to_string(),
            }
        });
    }
    let package_path = entrypoint.runtime_base.as_ref().map(|_| {
        runtime_package_dir(&runtime_key, &version)
            .display()
            .to_string()
    });
    let node_config = match ensure_node_effective_config_on_spawn(
        state,
        payload,
        &node_name,
        &target_hive,
        &runtime_key,
        &version,
        entrypoint.runtime_base.as_deref(),
        package_path.as_deref(),
        identity_ilk_id.as_deref(),
    ) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "CONFIG_WRITE_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
                "unit": unit,
                "identity": identity,
            });
        }
    };

    match systemd_unit_is_active(&unit) {
        Ok(true) => {
            return serde_json::json!({
                "status": "ok",
                "state": "already_running",
                "node_name": node_name,
                "runtime": runtime_key,
                "version": version,
                "requested_version": requested_version,
                "hive": target_hive,
                "target": target_hive,
                "unit": unit,
                "identity": identity,
                "config": node_config,
            });
        }
        Ok(false) => {}
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("unable to check unit activity: {err}"),
                "target": target_hive,
                "node_name": node_name,
                "unit": unit,
                "identity": identity,
                "config": node_config,
            });
        }
    }

    let cmd = build_managed_node_run_command(&unit, &node_name, &entrypoint.script_path);

    match execute_on_hive(state, &target_hive, &cmd, "run_node") {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "node_name": node_name,
            "runtime": runtime_key,
            "version": version,
            "requested_version": requested_version,
            "hive": target_hive,
            "target": target_hive,
            "unit": unit,
            "identity": identity,
            "config": node_config,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SPAWN_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": node_name,
            "unit": unit,
            "identity": identity,
            "config": node_config,
        }),
    }
}

async fn kill_node_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    if payload.get("name").is_some() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "legacy field 'name' is not supported; use 'node_name'",
        });
    }

    let node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .unwrap_or_default();

    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        if let Some((_, hive)) = node_name.split_once('@') {
            if !hive.trim().is_empty() {
                target_hive = hive.trim().to_string();
            }
        }
    }

    let validated_node_name = if node_name.is_empty() {
        None
    } else {
        match validate_existing_node_name_literal(&node_name) {
            Ok(value) => Some(value),
            Err(err) => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": err.to_string(),
                });
            }
        }
    };

    let unit = payload
        .get("unit")
        .and_then(|v| v.as_str())
        .map(|v| sanitize_unit_suffix(v.trim()))
        .filter(|v| !v.is_empty())
        .or_else(|| {
            validated_node_name
                .as_ref()
                .map(|name| unit_from_node_name(name))
        });

    let Some(unit) = unit else {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing unit or node_name",
        });
    };

    let force = payload
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let signal = if force {
        "SIGKILL".to_string()
    } else {
        payload
            .get("signal")
            .and_then(|v| v.as_str())
            .unwrap_or("SIGTERM")
            .to_ascii_uppercase()
    };

    let cmd = if signal == "SIGKILL" {
        format!(
            "systemctl kill -s KILL {unit} || true; systemctl stop {unit} || true; systemctl reset-failed {unit} || true"
        )
    } else {
        format!("systemctl stop {unit} || true; systemctl reset-failed {unit} || true")
    };

    if target_hive != state.hive_id {
        let mut forwarded_payload = payload.clone();
        if let Some(obj) = forwarded_payload.as_object_mut() {
            obj.insert("target".to_string(), serde_json::json!(target_hive));
            obj.insert("unit".to_string(), serde_json::json!(unit));
            obj.insert("signal".to_string(), serde_json::json!(signal));
            obj.insert("force".to_string(), serde_json::json!(force));
            if let Some(name) = validated_node_name.as_ref() {
                obj.insert("node_name".to_string(), serde_json::json!(name));
            }
        }
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "KILL_NODE",
            "KILL_NODE_RESPONSE",
            forwarded_payload,
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "KILL_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": validated_node_name,
                "unit": unit,
            }),
        };
    }

    match systemd_unit_is_active(&unit) {
        Ok(false) => {
            return serde_json::json!({
                "status": "not_found",
                "state": "not_found",
                "hive": target_hive,
                "target": target_hive,
                "node_name": validated_node_name,
                "unit": unit,
                "signal": signal,
                "force": force,
            });
        }
        Ok(true) => {}
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("unable to check unit activity: {err}"),
                "target": target_hive,
                "node_name": validated_node_name,
                "unit": unit,
            });
        }
    }

    match execute_on_hive(state, &target_hive, &cmd, "kill_node") {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "hive": target_hive,
            "target": target_hive,
            "node_name": validated_node_name,
            "unit": unit,
            "signal": signal,
            "force": force,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "KILL_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": validated_node_name,
            "unit": unit,
        }),
    }
}

fn remove_node_instance_dir_with_root(
    node_name: &str,
    root: &Path,
) -> Result<(PathBuf, bool), OrchestratorError> {
    let local = node_name
        .split_once('@')
        .map(|(local, _)| local)
        .unwrap_or(node_name);
    let node_kind = local
        .split('.')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("invalid node_name '{}': missing kind prefix", node_name))?;
    let kind_dir = root.join(node_kind);
    let node_dir = kind_dir.join(node_name);
    if !node_dir.exists() {
        return Err("NODE_NOT_FOUND".into());
    }
    fs::remove_dir_all(&node_dir)?;
    let mut removed_kind_dir = false;
    if kind_dir.is_dir() && kind_dir.read_dir()?.next().is_none() {
        fs::remove_dir(&kind_dir)?;
        removed_kind_dir = true;
    }
    Ok((node_dir, removed_kind_dir))
}

async fn remove_node_instance_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    if payload.get("name").is_some() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "legacy field 'name' is not supported; use 'node_name'",
        });
    }

    let node_name = payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    if node_name.is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing node_name",
        });
    }

    let mut target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        if let Some((_, hive)) = node_name.split_once('@') {
            if !hive.trim().is_empty() {
                target_hive = hive.trim().to_string();
            }
        }
    }

    let node_name = match validate_existing_node_name_literal(&node_name) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
            });
        }
    };

    if target_hive != state.hive_id {
        return match forward_system_action_to_hive(
            state,
            &target_hive,
            "REMOVE_NODE_INSTANCE",
            "REMOVE_NODE_INSTANCE_RESPONSE",
            serde_json::json!({
                "target": target_hive,
                "node_name": node_name,
            }),
        )
        .await
        {
            Ok(response) => response,
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "NODE_INSTANCE_REMOVE_FAILED",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            }),
        };
    }

    let instance_dir = match node_instance_dir(&node_name) {
        Ok(path) => path,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "INVALID_REQUEST",
                "message": err.to_string(),
                "target": target_hive,
                "node_name": node_name,
            });
        }
    };
    if !instance_dir.exists() {
        return serde_json::json!({
            "status": "not_found",
            "error_code": serde_json::Value::Null,
            "message": format!("node '{}' not found", node_name),
            "target": target_hive,
            "node_name": node_name,
        });
    }

    let unit = unit_from_node_name(&node_name);
    let unit_active = systemd_unit_is_active(&unit).unwrap_or(false);
    let inventory_visible = local_inventory_has_node(state, &node_name);
    if unit_active || inventory_visible {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_INSTANCE_RUNNING",
            "message": "node instance is still running/visible; stop it before removing",
            "target": target_hive,
            "node_name": node_name,
            "unit": unit,
            "visible_in_router": inventory_visible,
        });
    }

    let cleanup_cmd =
        format!("systemctl stop {unit} || true; systemctl reset-failed {unit} || true");
    if let Err(err) = execute_on_hive(state, &target_hive, &cleanup_cmd, "remove_node_instance") {
        return serde_json::json!({
            "status": "error",
            "error_code": "NODE_INSTANCE_REMOVE_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": node_name,
            "unit": unit,
        });
    }

    match remove_node_instance_dir_with_root(&node_name, &node_files_root()) {
        Ok((removed_path, removed_kind_dir)) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "hive": target_hive,
            "node_name": node_name,
            "unit": unit,
            "removed_path": removed_path.display().to_string(),
            "removed_kind_dir": removed_kind_dir,
        }),
        Err(err) => {
            if err.to_string() == "NODE_NOT_FOUND" {
                serde_json::json!({
                    "status": "not_found",
                    "error_code": serde_json::Value::Null,
                    "message": format!("node '{}' not found", node_name),
                    "target": target_hive,
                    "node_name": node_name,
                    "unit": unit,
                })
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "NODE_INSTANCE_REMOVE_FAILED",
                    "message": err.to_string(),
                    "target": target_hive,
                    "node_name": node_name,
                    "unit": unit,
                })
            }
        }
    }
}

fn execute_on_hive(
    state: &OrchestratorState,
    target_hive: &str,
    command: &str,
    label: &str,
) -> Result<(), OrchestratorError> {
    if target_hive != state.hive_id {
        return Err(format!(
            "remote execute disabled for '{label}' in v2 path; forward via SYSTEM message to SY.orchestrator@{target_hive}"
        )
        .into());
    }
    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(command);
    run_cmd(cmd, label)
}

fn build_managed_node_run_command(unit: &str, node_name: &str, script_path: &str) -> String {
    format!(
        "systemd-run --unit {unit} --setenv=FLUXBEE_NODE_NAME='{node_name}' --collect --property Restart=always --property RestartSec=5 '{script_path}'",
        unit = shell_single_quote(unit),
        node_name = shell_single_quote(node_name),
        script_path = shell_single_quote(script_path),
    )
}

fn shell_single_quote(value: &str) -> String {
    value.replace('\'', "'\"'\"'")
}

fn sha256_file(path: &Path) -> Result<String, OrchestratorError> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn load_core_manifest() -> Result<CoreManifest, OrchestratorError> {
    let manifest_path = local_core_manifest_path().ok_or_else(|| {
        format!(
            "core manifest missing at '{}' (run scripts/install.sh)",
            DIST_CORE_MANIFEST_PATH
        )
    })?;
    let data = fs::read_to_string(manifest_path)?;
    let manifest: CoreManifest = serde_json::from_str(&data)?;
    if manifest.schema_version == 0 {
        return Err("core manifest invalid: schema_version must be >= 1".into());
    }
    if manifest.components.is_empty() {
        return Err("core manifest has no components".into());
    }
    Ok(manifest)
}

fn validate_core_manifest_for_bins(bin_paths: &[String]) -> Result<(), OrchestratorError> {
    let manifest = load_core_manifest()?;

    for bin_path in bin_paths {
        let path = Path::new(bin_path);
        let component_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("invalid core binary path '{}'", path.display()))?
            .to_string();

        let expected = manifest.components.get(&component_name).ok_or_else(|| {
            format!(
                "core manifest missing component '{component_name}' for '{}'",
                path.display()
            )
        })?;
        if expected.service.trim().is_empty()
            || expected.version.trim().is_empty()
            || expected.build_id.trim().is_empty()
        {
            return Err(format!(
                "core manifest component '{}' missing service/version/build_id",
                component_name
            )
            .into());
        }
        if expected.service.trim() != component_name {
            return Err(format!(
                "core manifest component '{}' has service='{}' (expected '{}')",
                component_name, expected.service, component_name
            )
            .into());
        }

        let actual_hash = sha256_file(path)?;
        if actual_hash.is_empty() {
            return Err(format!("failed to compute sha256 for '{}'", path.display()).into());
        }
        if !actual_hash.eq_ignore_ascii_case(expected.sha256.trim()) {
            return Err(format!(
                "core manifest hash mismatch for '{}': expected={} actual={}",
                component_name, expected.sha256, actual_hash
            )
            .into());
        }

        if let Some(expected_size) = expected.size {
            let actual_size = fs::metadata(path)?.len();
            if actual_size != expected_size {
                return Err(format!(
                    "core manifest size mismatch for '{}': expected={} actual={}",
                    component_name, expected_size, actual_size
                )
                .into());
            }
        }
    }

    Ok(())
}

fn core_component_names_for_role(
    manifest: &CoreManifest,
    is_motherbee: bool,
) -> Result<Vec<String>, OrchestratorError> {
    if is_motherbee {
        return Ok(manifest.components.keys().cloned().collect());
    }

    let mut out = Vec::new();
    for name in WORKER_MIN_CORE_COMPONENTS {
        if !manifest.components.contains_key(name) {
            return Err(format!("core manifest missing required worker component '{name}'").into());
        }
        out.push(name.to_string());
    }
    Ok(out)
}

fn core_bin_paths_for_role(
    manifest: &CoreManifest,
    is_motherbee: bool,
) -> Result<Vec<String>, OrchestratorError> {
    Ok(core_component_names_for_role(manifest, is_motherbee)?
        .into_iter()
        .map(|name| local_core_bin_source_path(&name).display().to_string())
        .collect())
}

fn sync_core_to_worker(
    hive_id: &str,
    address: &str,
    key_path: &Path,
    restart_services_with_health_gate: bool,
    worker_bootstrap_only: bool,
) -> Result<(), OrchestratorError> {
    let manifest = load_core_manifest()?;
    let component_names = if worker_bootstrap_only {
        WORKER_BOOTSTRAP_CORE_COMPONENTS
            .iter()
            .map(|name| name.to_string())
            .collect()
    } else {
        core_component_names_for_role(&manifest, true)?
    };
    tracing::info!(
        hive_id = hive_id,
        mode = if worker_bootstrap_only {
            "worker-bootstrap-minimal"
        } else {
            "full-core-sync"
        },
        components = ?component_names,
        "sync core to worker"
    );
    if component_names.is_empty() {
        return Err("core manifest has no components to sync".into());
    }

    let mut local_paths = Vec::new();
    for name in &component_names {
        if !valid_token(name) {
            return Err(format!("core manifest has invalid component name '{}'", name).into());
        }
        let path = local_core_bin_source_path(name);
        if !path.exists() {
            return Err(format!("missing core source binary '{}'", path.display()).into());
        }
        local_paths.push(path.display().to_string());
    }
    validate_core_manifest_for_bins(&local_paths)?;

    if worker_bootstrap_only {
        let manifest_source_path = local_core_manifest_path()
            .ok_or_else(|| format!("core manifest missing at '{}'", DIST_CORE_MANIFEST_PATH))?;
        let mut upload_paths = local_paths.clone();
        upload_paths.push(manifest_source_path.display().to_string());
        let upload_refs: Vec<&str> = upload_paths.iter().map(String::as_str).collect();
        let remote_stage = format!("/tmp/fluxbee-core-sync-{}", sanitize_unit_suffix(hive_id));
        let prepare_stage = format!(
            "rm -rf '{stage}' && mkdir -p '{stage}'",
            stage = shell_single_quote(&remote_stage)
        );
        ssh_with_key(address, key_path, &prepare_stage, BOOTSTRAP_SSH_USER)?;
        scp_with_key(
            address,
            key_path,
            &upload_refs,
            &format!("{remote_stage}/"),
            BOOTSTRAP_SSH_USER,
        )?;

        let mut commands = vec![
            "set -euo pipefail".to_string(),
            "mkdir -p /var/lib/fluxbee/dist/core".to_string(),
            "mkdir -p /var/lib/fluxbee/dist/core/bin".to_string(),
        ];
        commands.push(format!(
            "install -m 0644 '{stage}/manifest.json' '/var/lib/fluxbee/dist/core/manifest.json'",
            stage = shell_single_quote(&remote_stage),
        ));
        for name in &component_names {
            commands.push(format!(
                "rm -f '/usr/bin/{name}' '/var/lib/fluxbee/dist/core/bin/{name}'",
                name = shell_single_quote(name),
            ));
            commands.push(format!(
                "install -m 0755 '{stage}/{name}' '/usr/bin/{name}'",
                stage = shell_single_quote(&remote_stage),
                name = shell_single_quote(name),
            ));
            commands.push(format!(
                "install -m 0755 '{stage}/{name}' '/var/lib/fluxbee/dist/core/bin/{name}'",
                stage = shell_single_quote(&remote_stage),
                name = shell_single_quote(name),
            ));
        }
        commands.push(format!(
            "rm -rf '{stage}'",
            stage = shell_single_quote(&remote_stage)
        ));

        let promote_cmd = commands.join(" && ");
        let promote_cmd_q = shell_single_quote(&promote_cmd);
        ssh_with_key(
            address,
            key_path,
            &sudo_wrap(&format!("bash -lc '{}'", promote_cmd_q)),
            BOOTSTRAP_SSH_USER,
        )?;
        verify_remote_core_components(address, key_path, &manifest, &component_names)?;
        if let Some(local_hash) = local_core_manifest_hash()? {
            let remote_hash = remote_core_manifest_hash(address, key_path)?;
            if remote_hash.as_deref() != Some(local_hash.as_str()) {
                return Err(format!(
                    "core manifest hash mismatch after bootstrap sync hive='{}' local={} remote={}",
                    hive_id,
                    local_hash,
                    remote_hash.unwrap_or_else(|| "<none>".to_string())
                )
                .into());
            }
        }
        return Ok(());
    }

    let mut upload_paths = local_paths.clone();
    let manifest_source_path = local_core_manifest_path()
        .ok_or_else(|| format!("core manifest missing at '{}'", DIST_CORE_MANIFEST_PATH))?;
    upload_paths.push(manifest_source_path.display().to_string());
    let upload_refs: Vec<&str> = upload_paths.iter().map(String::as_str).collect();

    let remote_stage = format!("/tmp/fluxbee-core-sync-{}", sanitize_unit_suffix(hive_id));
    let prepare_stage = format!(
        "rm -rf '{stage}' && mkdir -p '{stage}'",
        stage = shell_single_quote(&remote_stage)
    );
    ssh_with_key(address, key_path, &prepare_stage, BOOTSTRAP_SSH_USER)?;
    scp_with_key(
        address,
        key_path,
        &upload_refs,
        &format!("{remote_stage}/"),
        BOOTSTRAP_SSH_USER,
    )?;

    let mut commands = vec![
        "set -euo pipefail".to_string(),
        "mkdir -p /var/lib/fluxbee/dist/core".to_string(),
        "rm -rf /var/lib/fluxbee/dist/core/bin.next".to_string(),
        "mkdir -p /var/lib/fluxbee/dist/core/bin.next".to_string(),
    ];

    for name in &component_names {
        commands.push(format!(
            "install -m 0755 '{stage}/{name}' '/var/lib/fluxbee/dist/core/bin.next/{name}'",
            stage = shell_single_quote(&remote_stage),
            name = shell_single_quote(name),
        ));
    }
    commands.push(format!(
        "install -m 0644 '{stage}/manifest.json' '/var/lib/fluxbee/dist/core/manifest.next.json'",
        stage = shell_single_quote(&remote_stage)
    ));

    for name in &component_names {
        let expected = manifest
            .components
            .get(name)
            .ok_or_else(|| format!("core manifest missing component '{}'", name))?;
        commands.push(format!(
            "test \"$(sha256sum '/var/lib/fluxbee/dist/core/bin.next/{name}' | awk '{{print $1}}')\" = '{sha}'",
            name = shell_single_quote(name),
            sha = shell_single_quote(expected.sha256.trim()),
        ));
        if let Some(size) = expected.size {
            commands.push(format!(
                "test \"$(stat -c %s '/var/lib/fluxbee/dist/core/bin.next/{name}')\" = '{size}'",
                name = shell_single_quote(name),
                size = size
            ));
        }
    }

    commands.push("rm -rf /var/lib/fluxbee/dist/core/bin.prev".to_string());
    commands.push(
        "if [ -d /var/lib/fluxbee/dist/core/bin ]; then mv /var/lib/fluxbee/dist/core/bin /var/lib/fluxbee/dist/core/bin.prev; fi"
            .to_string(),
    );
    commands
        .push("mv /var/lib/fluxbee/dist/core/bin.next /var/lib/fluxbee/dist/core/bin".to_string());
    commands.push(
        "if [ -f /var/lib/fluxbee/dist/core/manifest.json ]; then cp /var/lib/fluxbee/dist/core/manifest.json /var/lib/fluxbee/dist/core/manifest.prev.json; fi"
            .to_string(),
    );
    commands.push(
        "mv /var/lib/fluxbee/dist/core/manifest.next.json /var/lib/fluxbee/dist/core/manifest.json"
            .to_string(),
    );
    for name in &component_names {
        commands.push(format!(
            "install -m 0755 '/var/lib/fluxbee/dist/core/bin/{name}' '/usr/bin/{name}'",
            name = shell_single_quote(name),
        ));
    }
    commands.push(format!(
        "rm -rf '{stage}'",
        stage = shell_single_quote(&remote_stage)
    ));

    let promote_cmd = commands.join(" && ");
    let promote_cmd_q = shell_single_quote(&promote_cmd);
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&format!("bash -lc '{}'", promote_cmd_q)),
        BOOTSTRAP_SSH_USER,
    )?;
    verify_remote_core_components(address, key_path, &manifest, &component_names)?;

    if let Some(local_hash) = local_core_manifest_hash()? {
        let remote_hash = remote_core_manifest_hash(address, key_path)?;
        if remote_hash.as_deref() != Some(local_hash.as_str()) {
            return Err(format!(
                "core manifest hash mismatch after sync hive='{}' local={} remote={}",
                hive_id,
                local_hash,
                remote_hash.unwrap_or_else(|| "<none>".to_string())
            )
            .into());
        }
    }

    if restart_services_with_health_gate {
        if let Err(err) = restart_remote_core_services_with_health_gate(address, key_path) {
            let rollback_result = rollback_remote_core_to_prev(address, key_path);
            let rollback_note = match rollback_result {
                Ok(()) => {
                    if let Err(rb_err) =
                        restart_remote_core_services_with_health_gate(address, key_path)
                    {
                        format!("rollback applied but restart after rollback failed: {rb_err}")
                    } else {
                        "rollback applied".to_string()
                    }
                }
                Err(rb_err) => format!("rollback failed: {rb_err}"),
            };
            return Err(
                format!("core service restart health-gate failed: {err}; {rollback_note}").into(),
            );
        }
    }

    Ok(())
}

fn verify_remote_core_components(
    address: &str,
    key_path: &Path,
    manifest: &CoreManifest,
    component_names: &[String],
) -> Result<(), OrchestratorError> {
    if component_names.is_empty() {
        return Ok(());
    }
    let mut commands = vec!["set -euo pipefail".to_string()];
    for name in component_names {
        let expected = manifest
            .components
            .get(name)
            .ok_or_else(|| format!("core manifest missing component '{}'", name))?;
        commands.push(format!(
            "test -f '/var/lib/fluxbee/dist/core/bin/{name}'",
            name = shell_single_quote(name),
        ));
        commands.push(format!(
            "test \"$(sha256sum '/var/lib/fluxbee/dist/core/bin/{name}' | awk '{{print $1}}')\" = '{sha}'",
            name = shell_single_quote(name),
            sha = shell_single_quote(expected.sha256.trim()),
        ));
        commands.push(format!(
            "test -f '/usr/bin/{name}'",
            name = shell_single_quote(name),
        ));
        commands.push(format!(
            "test \"$(sha256sum '/usr/bin/{name}' | awk '{{print $1}}')\" = '{sha}'",
            name = shell_single_quote(name),
            sha = shell_single_quote(expected.sha256.trim()),
        ));
        if let Some(size) = expected.size {
            commands.push(format!(
                "test \"$(stat -c %s '/var/lib/fluxbee/dist/core/bin/{name}')\" = '{size}'",
                name = shell_single_quote(name),
                size = size,
            ));
            commands.push(format!(
                "test \"$(stat -c %s '/usr/bin/{name}')\" = '{size}'",
                name = shell_single_quote(name),
                size = size,
            ));
        }
    }
    let verify_cmd = commands.join(" && ");
    let verify_cmd_q = shell_single_quote(&verify_cmd);
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&format!("bash -lc '{}'", verify_cmd_q)),
        BOOTSTRAP_SSH_USER,
    )
}

fn remote_service_exists(
    address: &str,
    key_path: &Path,
    service: &str,
) -> Result<bool, OrchestratorError> {
    let service_unit = if service.ends_with(".service") {
        service.to_string()
    } else {
        format!("{service}.service")
    };
    let service_q = shell_single_quote(&service_unit);
    let cmd = format!(
        "systemctl show '{service}' --property=LoadState --value 2>/dev/null || true",
        service = service_q
    );
    let out = ssh_with_key_output(address, key_path, &sudo_wrap(&cmd), BOOTSTRAP_SSH_USER)?;
    let state = out.trim();
    Ok(!state.is_empty() && state != "not-found")
}

fn remote_wait_service_active(
    address: &str,
    key_path: &Path,
    service: &str,
    timeout_secs: u64,
) -> Result<(), OrchestratorError> {
    let service_unit = if service.ends_with(".service") {
        service.to_string()
    } else {
        format!("{service}.service")
    };
    let service_q = shell_single_quote(&service_unit);
    let is_active_cmd = format!(
        "systemctl is-active --quiet '{service}'",
        service = service_q
    );
    let substate_cmd = format!(
        "systemctl show '{service}' --property=SubState --value 2>/dev/null || true",
        service = service_q
    );
    let state_summary_cmd = format!(
        "systemctl show '{service}' --property=ActiveState,SubState,Result,ExecMainCode,ExecMainStatus --value 2>/dev/null | tr '\\n' ' ' || true",
        service = service_q
    );
    let check_running = || -> Result<bool, OrchestratorError> {
        ssh_with_key(
            address,
            key_path,
            &sudo_wrap(&is_active_cmd),
            BOOTSTRAP_SSH_USER,
        )?;
        let substate = ssh_with_key_output(
            address,
            key_path,
            &sudo_wrap(&substate_cmd),
            BOOTSTRAP_SSH_USER,
        )
        .unwrap_or_default();
        Ok(substate.trim() == "running")
    };

    let mut stable = 0_u8;
    let mut last_err: Option<String> = None;
    let mut last_state_summary: Option<String> = None;
    for _ in 0..timeout_secs {
        match check_running() {
            Ok(true) => {
                stable = stable.saturating_add(1);
                if stable >= 3 {
                    return Ok(());
                }
            }
            Ok(false) => {
                stable = 0;
            }
            Err(err) => {
                stable = 0;
                last_err = Some(err.to_string());
                let summary = ssh_with_key_output(
                    address,
                    key_path,
                    &sudo_wrap(&state_summary_cmd),
                    BOOTSTRAP_SSH_USER,
                )
                .unwrap_or_default();
                let summary = summary.trim();
                if !summary.is_empty() {
                    last_state_summary = Some(summary.to_string());
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    // Avoid timeout-edge false negatives, but still require stability.
    let mut edge_stable = 0_u8;
    for _ in 0..3 {
        match check_running() {
            Ok(true) => {
                edge_stable = edge_stable.saturating_add(1);
                if edge_stable >= 3 {
                    tracing::warn!(
                        service = service,
                        timeout_secs = timeout_secs,
                        "service reached running state at timeout edge; accepting health gate"
                    );
                    return Ok(());
                }
            }
            Ok(false) | Err(_) => {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let detail = last_err.unwrap_or_else(|| "timed out waiting for active/running".to_string());
    let detail = if let Some(summary) = last_state_summary {
        format!("{detail}; state={summary}")
    } else {
        detail
    };
    Err(format!(
        "service '{}' did not become active in {}s: {}",
        service, timeout_secs, detail
    )
    .into())
}

fn restart_remote_core_services_with_health_gate(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    for service in CORE_SYNC_RESTART_ORDER {
        let exists = remote_service_exists(address, key_path, service)?;
        if !exists {
            tracing::info!(
                service = *service,
                "core sync: remote service not present; skipping restart"
            );
            continue;
        }
        tracing::info!(service = *service, "core sync: restarting remote service");
        let restart_cmd = format!("systemctl restart {}", service);
        ssh_with_key(
            address,
            key_path,
            &sudo_wrap(&restart_cmd),
            BOOTSTRAP_SSH_USER,
        )
        .map_err(|err| format!("failed to restart service '{}': {}", service, err))?;
        remote_wait_service_active(address, key_path, service, CORE_SERVICE_HEALTH_TIMEOUT_SECS)?;
        tracing::info!(
            service = *service,
            "core sync: remote service active after restart"
        );
    }
    Ok(())
}

fn rollback_remote_core_to_prev(address: &str, key_path: &Path) -> Result<(), OrchestratorError> {
    let rollback_cmd = "set -euo pipefail && \
if [ ! -d /var/lib/fluxbee/dist/core/bin.prev ]; then echo 'missing /var/lib/fluxbee/dist/core/bin.prev' >&2; exit 1; fi && \
rm -rf /var/lib/fluxbee/dist/core/bin.bad && \
if [ -d /var/lib/fluxbee/dist/core/bin ]; then mv /var/lib/fluxbee/dist/core/bin /var/lib/fluxbee/dist/core/bin.bad; fi && \
mv /var/lib/fluxbee/dist/core/bin.prev /var/lib/fluxbee/dist/core/bin && \
if [ -f /var/lib/fluxbee/dist/core/manifest.prev.json ]; then mv /var/lib/fluxbee/dist/core/manifest.prev.json /var/lib/fluxbee/dist/core/manifest.json; fi && \
for b in /var/lib/fluxbee/dist/core/bin/*; do [ -f \"$b\" ] || continue; install -m 0755 \"$b\" \"/usr/bin/$(basename \"$b\")\"; done";
    let rollback_cmd_q = shell_single_quote(rollback_cmd);
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&format!("bash -lc '{}'", rollback_cmd_q)),
        BOOTSTRAP_SSH_USER,
    )
}

fn truncate_for_error(value: &str, max_len: usize) -> String {
    let compact = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if compact.len() <= max_len {
        return compact;
    }
    let mut out = compact
        .chars()
        .take(max_len.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn remote_service_journal_tail(
    address: &str,
    key_path: &Path,
    service: &str,
    lines: usize,
) -> Option<String> {
    let service_unit = if service.ends_with(".service") {
        service.to_string()
    } else {
        format!("{service}.service")
    };
    let service_q = shell_single_quote(&service_unit);
    let cmd = format!(
        "bash -lc \"\
echo '--- journalctl ---'; \
journalctl -u '{service}' --no-pager -n {lines} -o short-precise 2>/dev/null || true; \
echo '--- systemctl status ---'; \
systemctl --no-pager -l status '{service}' 2>/dev/null | tail -n {lines} || true; \
echo '--- syslog tail ---'; \
tail -n {lines} /var/log/syslog 2>/dev/null | grep -E '{service_raw}|sy_orchestrator' || true\"",
        service = service_q,
        service_raw = service.replace('\'', "'\"'\"'"),
        lines = lines
    );
    match ssh_with_key_output(address, key_path, &sudo_wrap(&cmd), BOOTSTRAP_SSH_USER) {
        Ok(out) => {
            let trimmed = out.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(truncate_for_error(trimmed, 6000))
            }
        }
        Err(_) => None,
    }
}

fn list_managed_hive_ids() -> Vec<String> {
    let mut out = Vec::new();
    let root = hives_root();
    let read = match fs::read_dir(root) {
        Ok(read) => read,
        Err(_) => return out,
    };
    for entry in read.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let hive_id = entry.file_name().to_string_lossy().to_string();
        if !hive_id.is_empty() {
            out.push(hive_id);
        }
    }
    out.sort();
    out
}

async fn add_hive_finalize_local_flow(
    state: &OrchestratorState,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let desired_dist = current_dist_runtime_config(state);
    let desired_blob = current_blob_runtime_config(state);
    let desired_sync = effective_syncthing_runtime_config(&desired_blob, &desired_dist);
    let require_dist_sync = payload
        .get("require_dist_sync")
        .and_then(parse_bool_value)
        .unwrap_or(false);
    let dist_sync_probe_timeout_secs = payload
        .get("dist_sync_probe_timeout_secs")
        .and_then(|value| value.as_u64())
        .unwrap_or(DIST_SYNC_PROBE_TIMEOUT_SECS)
        .clamp(5, 600);
    let syncthing_peer_device_id = payload
        .get("syncthing_peer_device_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let syncthing_peer_name = payload
        .get("syncthing_peer_name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("motherbee")
        .to_string();
    let mut updated: Vec<String> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    let mut restarted: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut core_sync_pending = false;

    let core_result = match apply_system_update_local(state, "core").await {
        Ok(result) => result,
        Err(err) => {
            let err_text = err.to_string();
            if is_add_hive_core_sync_pending_error(&err_text) {
                core_sync_pending = true;
                unchanged.push("core-sync-pending".to_string());
                tracing::warn!(
                    hive_id = %state.hive_id,
                    error = %err_text,
                    "core finalize pending (dist/core not converged yet); continuing"
                );
                SystemUpdateApplyResult {
                    status: "ok".to_string(),
                    updated: Vec::new(),
                    unchanged: Vec::new(),
                    restarted: Vec::new(),
                    errors: Vec::new(),
                }
            } else {
                errors.push(err_text.clone());
                return serde_json::json!({
                    "status": "error",
                    "error_code": "CORE_FINALIZE_FAILED",
                    "message": err_text,
                    "hive_id": state.hive_id,
                    "require_dist_sync": require_dist_sync,
                    "dist_sync_enabled": desired_dist.sync_enabled,
                    "updated": updated,
                    "unchanged": unchanged,
                    "restarted": restarted,
                    "errors": errors,
                });
            }
        }
    };
    if core_result.status != "ok" {
        let detail = if core_result.errors.is_empty() {
            format!("core finalize returned status '{}'", core_result.status)
        } else {
            core_result.errors.join("; ")
        };
        errors.push(detail.clone());
        return serde_json::json!({
            "status": "error",
            "error_code": "CORE_FINALIZE_FAILED",
            "message": detail,
            "hive_id": state.hive_id,
            "require_dist_sync": require_dist_sync,
            "dist_sync_enabled": desired_dist.sync_enabled,
            "updated": core_result.updated,
            "unchanged": core_result.unchanged,
            "restarted": core_result.restarted,
            "errors": errors,
        });
    }

    updated.extend(core_result.updated);
    unchanged.extend(core_result.unchanged);
    restarted.extend(core_result.restarted);

    let vendor_result = match apply_system_update_local(state, "vendor").await {
        Ok(result) => result,
        Err(err) => {
            errors.push(err.to_string());
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_SETUP_FAILED",
                "message": err.to_string(),
                "hive_id": state.hive_id,
                "require_dist_sync": require_dist_sync,
                "dist_sync_enabled": desired_dist.sync_enabled,
                "updated": updated,
                "unchanged": unchanged,
                "restarted": restarted,
                "errors": errors,
            });
        }
    };
    if vendor_result.status != "ok" {
        let detail = if vendor_result.errors.is_empty() {
            format!("vendor finalize returned status '{}'", vendor_result.status)
        } else {
            vendor_result.errors.join("; ")
        };
        errors.push(detail.clone());
        return serde_json::json!({
            "status": "error",
            "error_code": "SYNC_SETUP_FAILED",
            "message": detail,
            "hive_id": state.hive_id,
            "dist_sync_enabled": desired_dist.sync_enabled,
            "require_dist_sync": require_dist_sync,
            "updated": updated,
            "unchanged": unchanged,
            "restarted": restarted,
            "errors": errors,
        });
    }

    updated.extend(vendor_result.updated);
    unchanged.extend(vendor_result.unchanged);
    restarted.extend(vendor_result.restarted);

    let mut syncthing_peer_linked = false;
    if let Some(peer_device_id) = syncthing_peer_device_id.as_deref() {
        if let Err(err) = ensure_syncthing_peer_link_runtime(
            &desired_sync,
            &desired_blob,
            &desired_dist,
            peer_device_id,
            &syncthing_peer_name,
        )
        .await
        {
            errors.push(err.to_string());
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_SETUP_FAILED",
                "message": err.to_string(),
                "hive_id": state.hive_id,
                "dist_sync_enabled": desired_dist.sync_enabled,
                "require_dist_sync": require_dist_sync,
                "updated": updated,
                "unchanged": unchanged,
                "restarted": restarted,
                "errors": errors,
            });
        }
        syncthing_peer_linked = true;
    }

    let syncthing_device_id = if desired_sync.sync_enabled
        && (blob_sync_tool_is_syncthing(&desired_sync)
            || dist_sync_tool_is_syncthing(&desired_dist))
    {
        local_syncthing_device_id(&desired_sync).ok()
    } else {
        None
    };
    let service_active = if desired_sync.sync_enabled {
        systemd_is_active(SYNCTHING_SERVICE_NAME)
    } else {
        false
    };
    let api_healthy = if desired_sync.sync_enabled {
        syncthing_api_healthy(desired_sync.sync_api_port).await
    } else {
        false
    };
    let runtime_manifest_ready =
        if !desired_dist.sync_enabled || !dist_sync_tool_is_syncthing(&desired_dist) {
            local_runtime_manifest_ready_for_spawn()
        } else if service_active && api_healthy {
            wait_for_local_runtime_manifest_ready(Duration::from_secs(dist_sync_probe_timeout_secs))
                .await
        } else {
            false
        };
    let dist_sync_ready =
        if !desired_dist.sync_enabled || !dist_sync_tool_is_syncthing(&desired_dist) {
            true
        } else {
            service_active && api_healthy && runtime_manifest_ready
        };

    serde_json::json!({
        "status": "ok",
        "hive_id": state.hive_id,
        "blob_sync_enabled": desired_blob.sync_enabled,
        "dist_sync_enabled": desired_dist.sync_enabled,
        "service_active": service_active,
        "api_healthy": api_healthy,
        "runtime_manifest_ready": runtime_manifest_ready,
        "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
        "dist_sync_ready": dist_sync_ready,
        "core_sync_pending": core_sync_pending,
        "syncthing_peer_linked": syncthing_peer_linked,
        "syncthing_device_id": syncthing_device_id,
        "require_dist_sync": require_dist_sync,
        "dist_path": desired_dist.path.display().to_string(),
        "updated": updated,
        "unchanged": unchanged,
        "restarted": restarted,
        "errors": errors,
    })
}

fn is_add_hive_core_sync_pending_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("core manifest missing")
        || lower.contains("missing core source binary")
        || lower.contains("core manifest missing required worker component")
}

fn is_socket_only_unreachable_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("system forward timeout")
        || (lower.contains("target hive")
            && (lower.contains("not reachable in lsa") || lower.contains("stale/missing")))
        || lower.contains("unreachable")
        || lower.contains("ttl_exceeded")
}

async fn add_hive_finalize_via_socket(
    state: &OrchestratorState,
    hive_id: &str,
    address: &str,
    require_dist_sync: bool,
    dist_sync_probe_timeout_secs: u64,
    syncthing_peer_device_id: Option<&str>,
    syncthing_peer_name: Option<&str>,
) -> Result<serde_json::Value, OrchestratorError> {
    // Socket-only path is an optimization. Keep it bounded so add_hive still has
    // enough budget to fall back to SSH bootstrap within admin timeout (180s).
    let timeout_secs = dist_sync_probe_timeout_secs.max(45).clamp(30, 75);
    let payload = forward_system_action_to_hive_with_timeout(
        state,
        hive_id,
        "ADD_HIVE_FINALIZE",
        "ADD_HIVE_FINALIZE_RESPONSE",
        serde_json::json!({
            "hive_id": hive_id,
            "target": hive_id,
            "address": address,
            "require_dist_sync": require_dist_sync,
            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
            "syncthing_peer_device_id": syncthing_peer_device_id,
            "syncthing_peer_name": syncthing_peer_name,
        }),
        Duration::from_secs(timeout_secs),
    )
    .await?;

    if payload
        .get("status")
        .and_then(|value| value.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("ok"))
    {
        return Ok(payload);
    }

    Err(format!("worker finalize returned non-ok payload: {}", payload).into())
}

async fn probe_remote_orchestrator_socket_ready(
    state: &OrchestratorState,
    hive_id: &str,
    address: &str,
) -> Result<(), OrchestratorError> {
    let payload = forward_system_action_to_hive_with_timeout(
        state,
        hive_id,
        "GET_VERSIONS",
        "GET_VERSIONS_RESPONSE",
        serde_json::json!({
            "hive_id": hive_id,
            "target": hive_id,
            "address": address,
        }),
        Duration::from_secs(ADD_HIVE_SOCKET_READY_PROBE_TIMEOUT_SECS),
    )
    .await?;

    if payload
        .get("status")
        .and_then(|value| value.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("ok"))
    {
        return Ok(());
    }

    Err(format!("GET_VERSIONS probe returned non-ok payload: {payload}").into())
}

fn append_add_hive_finalize_history(
    state: &OrchestratorState,
    hive_id: &str,
    status: &str,
    reason: Option<String>,
) {
    let manifest_hash = local_syncthing_vendor_hash().ok().flatten();
    append_single_deployment_history(
        state,
        "vendor",
        "add_hive_finalize_socket",
        hive_id,
        status,
        reason,
        manifest_hash,
    );
}

fn dist_sync_ready_from_finalize_payload(
    finalize: &serde_json::Value,
    dist: &DistRuntimeConfig,
) -> bool {
    if !dist.sync_enabled || !dist_sync_tool_is_syncthing(dist) {
        return true;
    }
    finalize
        .get("dist_sync_ready")
        .and_then(|value| value.as_bool())
        .or_else(|| {
            finalize
                .get("api_healthy")
                .and_then(|value| value.as_bool())
        })
        .unwrap_or(false)
}

fn syncthing_device_id_from_finalize_payload(finalize: &serde_json::Value) -> Option<String> {
    finalize
        .get("syncthing_device_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| valid_syncthing_device_id(value))
        .map(str::to_string)
}

async fn add_hive_flow(
    state: &OrchestratorState,
    hive_id: &str,
    address: &str,
    harden_ssh: bool,
    restrict_ssh: bool,
    require_dist_sync: bool,
    dist_sync_probe_timeout_secs: u64,
) -> serde_json::Value {
    let desired_blob = current_blob_runtime_config(state);
    let desired_dist = current_dist_runtime_config(state);
    let desired_sync = effective_syncthing_runtime_config(&desired_blob, &desired_dist);
    let syncthing_expected = desired_sync.sync_enabled
        && (blob_sync_tool_is_syncthing(&desired_sync)
            || dist_sync_tool_is_syncthing(&desired_dist));
    let mother_syncthing_device_id = if syncthing_expected {
        match local_syncthing_device_id(&desired_sync) {
            Ok(device_id) => Some(device_id),
            Err(err) => {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "SYNC_SETUP_FAILED",
                    "message": format!("local syncthing device id unavailable: {err}"),
                });
            }
        }
    } else {
        None
    };
    let mut dist_sync_ready =
        !desired_dist.sync_enabled || !dist_sync_tool_is_syncthing(&desired_dist);
    let root = hives_root();
    let hive_dir = root.join(hive_id);
    if hive_exists(&state.state_dir, hive_id) {
        append_single_deployment_history(
            state,
            "core",
            "add_hive",
            hive_id,
            "error",
            Some("HIVE_EXISTS".to_string()),
            local_core_manifest_hash().ok().flatten(),
        );
        return serde_json::json!({
            "status": "error",
            "error_code": "HIVE_EXISTS",
            "message": "hive already exists",
        });
    }
    if hive_partial_exists(hive_id) {
        tracing::warn!(
            hive_id = hive_id,
            "stale hive state detected (missing info.yaml); cleaning before bootstrap"
        );
        if let Err(err) = fs::remove_dir_all(&hive_dir) {
            return serde_json::json!({
                "status": "error",
                "error_code": "IO_ERROR",
                "message": format!("failed to clean stale hive dir: {err}"),
            });
        }
    }
    if !valid_hive_id(hive_id) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_HIVE_ID",
            "message": "invalid hive_id",
        });
    }
    if !valid_address(address) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_ADDRESS",
            "message": "invalid address",
        });
    }
    if state.wan_listen.as_deref().unwrap_or("").is_empty() {
        return serde_json::json!({
            "status": "error",
            "error_code": "MISSING_WAN_LISTEN",
            "message": "wan.listen missing in hive.yaml",
        });
    }
    if !state.wan_authorized_hives.is_empty()
        && !state
            .wan_authorized_hives
            .iter()
            .any(|allowed| allowed.trim() == hive_id)
    {
        return serde_json::json!({
            "status": "error",
            "error_code": "WAN_NOT_AUTHORIZED",
            "message": format!(
                "hive '{}' not present in wan.authorized_hives; update /etc/fluxbee/hive.yaml or leave authorized_hives empty",
                hive_id
            ),
            "hive_id": hive_id,
        });
    }

    if let Err(err) = fs::create_dir_all(&hive_dir) {
        return serde_json::json!({
            "status": "error",
            "error_code": "IO_ERROR",
            "message": err.to_string(),
        });
    }

    // Socket-first fast path: if worker orchestrator is already visible in LSA,
    // register hive without rerunning SSH bootstrap/provisioning.
    let socket_only_ready =
        wait_for_remote_orchestrator_node(&state.hive_id, hive_id, Duration::from_secs(3)).is_ok();
    if socket_only_ready {
        if let Err(err) = probe_remote_orchestrator_socket_ready(state, hive_id, address).await {
            tracing::warn!(
                hive_id = hive_id,
                address = address,
                error = %err,
                "socket-only add_hive precheck failed; falling back to bootstrap path"
            );
        } else {
            let finalize = match add_hive_finalize_via_socket(
                state,
                hive_id,
                address,
                require_dist_sync,
                dist_sync_probe_timeout_secs,
                mother_syncthing_device_id.as_deref(),
                Some(&state.hive_id),
            )
            .await
            {
                Ok(payload) => {
                    append_add_hive_finalize_history(state, hive_id, "ok", None);
                    Some(payload)
                }
                Err(err) => {
                    let err_text = err.to_string();
                    if is_socket_only_unreachable_error(&err_text) {
                        tracing::warn!(
                            hive_id = hive_id,
                            address = address,
                            error = %err_text,
                            "socket-only add_hive finalize unreachable/timeout; falling back to bootstrap path"
                        );
                        None
                    } else {
                        append_add_hive_finalize_history(
                            state,
                            hive_id,
                            "error",
                            Some(err_text.clone()),
                        );
                        return serde_json::json!({
                            "status": "error",
                            "error_code": "FINALIZE_FAILED",
                            "message": format!("worker socket-only finalize failed: {}", err_text),
                            "hive_id": hive_id,
                            "address": address,
                            "bootstrap_mode": "socket_only_existing_orchestrator",
                            "harden_ssh": harden_ssh,
                            "restrict_ssh_requested": restrict_ssh,
                            "require_dist_sync": require_dist_sync,
                            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                        });
                    }
                }
            };
            if let Some(finalize) = finalize {
                let mut syncthing_peer_linked = false;
                let mut worker_syncthing_device_id: Option<String> = None;
                if syncthing_expected {
                    let Some(peer_device_id) = syncthing_device_id_from_finalize_payload(&finalize)
                    else {
                        return serde_json::json!({
                            "status": "error",
                            "error_code": "FINALIZE_FAILED",
                            "message": "worker finalize missing syncthing_device_id",
                            "hive_id": hive_id,
                            "address": address,
                            "bootstrap_mode": "socket_only_existing_orchestrator",
                            "require_dist_sync": require_dist_sync,
                            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                            "finalize": finalize,
                        });
                    };
                    if let Err(err) = ensure_syncthing_peer_link_runtime(
                        &desired_sync,
                        &desired_blob,
                        &desired_dist,
                        &peer_device_id,
                        hive_id,
                    )
                    .await
                    {
                        return serde_json::json!({
                            "status": "error",
                            "error_code": "SYNC_SETUP_FAILED",
                            "message": format!("local syncthing peer reconcile failed: {err}"),
                            "hive_id": hive_id,
                            "address": address,
                            "bootstrap_mode": "socket_only_existing_orchestrator",
                            "require_dist_sync": require_dist_sync,
                            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                            "finalize": finalize,
                        });
                    }
                    worker_syncthing_device_id = Some(peer_device_id);
                    syncthing_peer_linked = true;
                }
                dist_sync_ready = dist_sync_ready_from_finalize_payload(&finalize, &desired_dist);
                if require_dist_sync && !dist_sync_ready {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "DIST_SYNC_TIMEOUT",
                        "message": "worker finalize completed but dist sync is not ready",
                        "hive_id": hive_id,
                        "address": address,
                        "bootstrap_mode": "socket_only_existing_orchestrator",
                        "require_dist_sync": require_dist_sync,
                        "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                        "dist_sync_ready": dist_sync_ready,
                        "finalize": finalize,
                    });
                }
                if harden_ssh || restrict_ssh {
                    tracing::warn!(
                        hive_id = hive_id,
                        address = address,
                        harden_ssh = harden_ssh,
                        restrict_ssh_requested = restrict_ssh,
                        "socket-only add_hive: skipping SSH controls because SSH is bootstrap-only"
                    );
                }
                let mut info_payload = serde_json::json!({
                    "hive_id": hive_id,
                    "address": address,
                    "created_at": now_epoch_ms().to_string(),
                    "status": "connected",
                });
                if let Some(device_id) = worker_syncthing_device_id.clone() {
                    info_payload["syncthing_device_id"] = serde_json::json!(device_id);
                }
                info_payload["syncthing_peer_linked"] = serde_json::json!(syncthing_peer_linked);
                if let Err(err) = write_hive_info(&root, hive_id, &info_payload) {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "IO_ERROR",
                        "message": err.to_string(),
                    });
                }
                tracing::info!(
                    hive_id = hive_id,
                    address = address,
                    "add_hive socket-only mode: worker orchestrator already online; skipping SSH bootstrap"
                );
                return serde_json::json!({
                    "status": "ok",
                    "hive_id": hive_id,
                    "address": address,
                    "bootstrap_mode": "socket_only_existing_orchestrator",
                    "harden_ssh": harden_ssh,
                    "harden_ssh_applied": false,
                    "restrict_ssh": false,
                    "restrict_ssh_mode": "socket_only_no_ssh",
                    "restrict_ssh_requested": restrict_ssh,
                    "require_dist_sync": require_dist_sync,
                    "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                    "wan_connected": true,
                    "orchestrator_connected": true,
                    "dist_sync_ready": dist_sync_ready,
                    "syncthing_peer_linked": syncthing_peer_linked,
                    "syncthing_device_id": worker_syncthing_device_id,
                    "finalize": finalize,
                });
            }
        }
    }

    let key_path = PathBuf::from(MOTHERBEE_SSH_KEY_PATH);
    if !key_path.exists() {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": format!(
                "motherbee ssh key missing (expected '{}'); run scripts/install.sh",
                key_path.display()
            ),
        });
    }

    let pub_key = match public_key_from_private_key(&key_path) {
        Ok(data) => data,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "SSH_KEY_FAILED",
                "message": format!(
                    "failed to derive public key from '{}': {}",
                    key_path.display(),
                    err
                ),
            })
        }
    };

    let mut password_channel_available = true;
    if let Err(pass_err) = ssh_with_pass_any(address, "true", BOOTSTRAP_SSH_USER) {
        password_channel_available = false;
        match ssh_with_key(address, &key_path, "true", BOOTSTRAP_SSH_USER) {
            Ok(()) => {
                tracing::warn!(
                    target = address,
                    error = %pass_err,
                    "password bootstrap channel unavailable; continuing via key channel"
                );
            }
            Err(key_err) => {
                return ssh_bootstrap_error_payload(&format!(
                    "password probe failed: {pass_err}; key probe failed: {key_err}"
                ));
            }
        }
    }

    if password_channel_available {
        if let Err(err) = apply_remote_unrestricted_authorized_key_with_pass(address, &pub_key) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SSH_KEY_FAILED",
                "message": format!("failed to seed bootstrap key via password channel: {err}"),
            });
        }
    } else {
        tracing::info!(
            target = address,
            "password bootstrap channel unavailable; key channel already active, skipping key reseed"
        );
    }

    if let Err(err) = ensure_remote_orchestrator_sudoers_with_access(address, &key_path) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SUDO_SETUP_FAILED",
            "message": err.to_string(),
        });
    }

    let mut restrict_ssh_applied = false;
    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        "sudo -n /bin/bash -lc 'exit 0'",
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": format!("key access verification failed after bootstrap seed: {err}"),
        });
    }

    let core_manifest = match load_core_manifest() {
        Ok(manifest) => manifest,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "MANIFEST_INVALID",
                "message": err.to_string(),
            });
        }
    };
    let has_identity_source = core_manifest.components.contains_key("sy-identity");

    let core_deploy_started_at = now_epoch_ms();
    let core_deploy_started = Instant::now();
    let local_core_hash = local_core_manifest_hash().ok().flatten();
    let remote_core_hash_before = remote_core_manifest_hash(address, &key_path).ok().flatten();
    if let Err(err) = sync_core_to_worker(hive_id, address, &key_path, false, true) {
        let entry = DeploymentHistoryEntry {
            deployment_id: Uuid::new_v4().to_string(),
            category: "core".to_string(),
            trigger: "add_hive".to_string(),
            actor: default_deployment_actor(state),
            started_at: core_deploy_started_at,
            finished_at: core_deploy_started_at + core_deploy_started.elapsed().as_millis() as u64,
            manifest_version: None,
            manifest_hash: local_core_hash.clone(),
            target_hives: vec![hive_id.to_string()],
            result: "error".to_string(),
            workers: vec![DeploymentWorkerOutcome {
                hive_id: hive_id.to_string(),
                status: "error".to_string(),
                reason: Some(err.to_string()),
                duration_ms: core_deploy_started.elapsed().as_millis() as u64,
                local_hash: local_core_hash.clone(),
                remote_hash_before: remote_core_hash_before,
                remote_hash_after: None,
            }],
        };
        if let Err(history_err) = append_deployment_history(&entry) {
            tracing::warn!(error = %history_err, "failed to persist add_hive core deployment history");
        }
        return serde_json::json!({
            "status": "error",
            "error_code": "COPY_FAILED",
            "message": err.to_string(),
        });
    }
    let remote_core_hash_after = remote_core_manifest_hash(address, &key_path).ok().flatten();
    let entry = DeploymentHistoryEntry {
        deployment_id: Uuid::new_v4().to_string(),
        category: "core".to_string(),
        trigger: "add_hive".to_string(),
        actor: default_deployment_actor(state),
        started_at: core_deploy_started_at,
        finished_at: core_deploy_started_at + core_deploy_started.elapsed().as_millis() as u64,
        manifest_version: None,
        manifest_hash: local_core_hash.clone(),
        target_hives: vec![hive_id.to_string()],
        result: "ok".to_string(),
        workers: vec![DeploymentWorkerOutcome {
            hive_id: hive_id.to_string(),
            status: "ok".to_string(),
            reason: None,
            duration_ms: core_deploy_started.elapsed().as_millis() as u64,
            local_hash: local_core_hash,
            remote_hash_before: remote_core_hash_before,
            remote_hash_after: remote_core_hash_after,
        }],
    };
    if let Err(history_err) = append_deployment_history(&entry) {
        tracing::warn!(error = %history_err, "failed to persist add_hive core deployment history");
    }

    let blob_active_dir = state.blob.path.join("active");
    let blob_staging_dir = state.blob.path.join("staging");
    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap(&format!(
            "mkdir -p /etc/fluxbee /var/lib/fluxbee/state/nodes /var/lib/fluxbee/opa/current /var/lib/fluxbee/opa/staged /var/lib/fluxbee/opa/backup /var/lib/fluxbee/nats /var/lib/fluxbee/dist /var/lib/fluxbee/dist/runtimes /var/lib/fluxbee/dist/core/bin /var/lib/fluxbee/dist/vendor /var/run/fluxbee/routers '{}' '{}' '{}' '{}'",
            state.blob.path.display(),
            blob_active_dir.display(),
            blob_staging_dir.display(),
            state.blob.sync_data_dir.display()
        )),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_FAILED",
            "message": err.to_string(),
        });
    }

    let wan_listen = state.wan_listen.clone().unwrap_or_default();
    let worker_uplink = match resolve_worker_uplink_address(&wan_listen, address) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "CONFIG_FAILED",
                "message": format!("resolve worker uplink failed: {err}"),
            });
        }
    };
    let storage_path = state
        .storage_path
        .try_lock()
        .map(|guard| guard.clone())
        .unwrap_or_else(|_| {
            json_router::paths::storage_root_dir()
                .to_string_lossy()
                .to_string()
        });
    let local_hive = load_hive(&state.config_dir).ok();
    let identity_frontdesk_node_name = local_hive
        .as_ref()
        .map(identity_frontdesk_node_name_from_hive)
        .unwrap_or_else(|| format!("AI.frontdesk@{}", state.hive_id));
    let identity_sync_port = local_hive
        .as_ref()
        .map(identity_sync_port_from_hive)
        .unwrap_or(DEFAULT_IDENTITY_SYNC_PORT);
    let (worker_uplink_host, _) = match parse_host_port(&worker_uplink) {
        Ok(value) => value,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "CONFIG_FAILED",
                "message": format!("resolve identity sync upstream failed: {err}"),
            });
        }
    };
    let identity_sync_upstream = format_host_port(&worker_uplink_host, identity_sync_port);
    let hive_yaml = format!(
        "hive_id: {}\nrole: worker\nwan:\n  gateway_name: RT.gateway\n  uplinks:\n    - address: \"{}\"\nnats:\n  mode: embedded\n  port: 4222\nstorage:\n  path: \"{}\"\ngovernment:\n  identity_frontdesk: \"{}\"\nidentity:\n  sync:\n    upstream: \"{}\"\nblob:\n  enabled: {}\n  path: \"{}\"\n  sync:\n    enabled: {}\n    tool: \"{}\"\n    api_port: {}\n    data_dir: \"{}\"\n  gc:\n    enabled: {}\n    interval_secs: {}\n    apply: {}\n    staging_ttl_hours: {}\n    active_retain_days: {}\ndist:\n  path: \"{}\"\n  sync:\n    enabled: {}\n    tool: \"{}\"\n",
        hive_id,
        worker_uplink,
        storage_path,
        identity_frontdesk_node_name,
        identity_sync_upstream,
        desired_blob.enabled,
        desired_blob.path.display(),
        desired_blob.sync_enabled,
        desired_blob.sync_tool,
        desired_blob.sync_api_port,
        desired_blob.sync_data_dir.display(),
        desired_blob.gc_enabled,
        desired_blob.gc_interval_secs,
        desired_blob.gc_apply,
        desired_blob.gc_staging_ttl_hours,
        desired_blob.gc_active_retain_days,
        desired_dist.path.display(),
        desired_dist.sync_enabled,
        desired_dist.sync_tool
    );
    if let Err(err) = write_remote_file(address, &key_path, "/etc/fluxbee/hive.yaml", &hive_yaml) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_FAILED",
            "message": err.to_string(),
        });
    }

    let config_routes_yaml = format!(
        "version: 1\nupdated_at: \"{}\"\nroutes: []\nvpns: []\n",
        now_epoch_ms()
    );
    if let Err(err) = write_remote_file(
        address,
        &key_path,
        "/etc/fluxbee/sy-config-routes.yaml",
        &config_routes_yaml,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_FAILED",
            "message": err.to_string(),
        });
    }

    let mut worker_units = vec![
        ("rt-gateway", "/usr/bin/rt-gateway"),
        ("sy-config-routes", "/usr/bin/sy-config-routes"),
        ("sy-opa-rules", "/usr/bin/sy-opa-rules"),
        ("sy-orchestrator", "/usr/bin/sy-orchestrator"),
    ];
    if has_identity_source {
        worker_units.push(("sy-identity", "/usr/bin/sy-identity"));
    }

    for (name, exec_path) in &worker_units {
        let unit = if *name == "sy-orchestrator" {
            format!(
                "[Unit]\nDescription=Fluxbee {name}\nAfter=network.target rt-gateway.service\nWants=rt-gateway.service\nRequires=rt-gateway.service\n\n[Service]\nType=simple\nExecStart={exec_path}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
            )
        } else {
            format!(
                "[Unit]\nDescription=Fluxbee {name}\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={exec_path}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
            )
        };
        let unit_path = format!("/etc/systemd/system/{name}.service");
        if let Err(err) = write_remote_file(address, &key_path, &unit_path, &unit) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("{name}: {err}"),
            });
        }
    }

    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("systemctl daemon-reload"),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SERVICE_FAILED",
            "message": err.to_string(),
        });
    }

    let bootstrap_units = ["rt-gateway", "sy-orchestrator"];
    for name in bootstrap_units {
        if let Err(err) = ssh_with_key(
            address,
            &key_path,
            &sudo_wrap(&format!("systemctl enable {name}")),
            BOOTSTRAP_SSH_USER,
        ) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("enable {name}: {err}"),
            });
        }
        let service_inner = if name == "sy-orchestrator" {
            format!(
                "systemctl start {name} || (systemctl reset-failed {name} || true; sleep 1; systemctl start {name})"
            )
        } else {
            format!(
                "systemctl restart {name} || (systemctl reset-failed {name} || true; sleep 1; systemctl start {name})"
            )
        };
        let service_cmd = sudo_wrap(&format!(
            "bash -lc '{}'",
            shell_single_quote(&service_inner)
        ));
        if let Err(err) = ssh_with_key(address, &key_path, &service_cmd, BOOTSTRAP_SSH_USER) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("start/restart bootstrap unit {name}: {err}"),
            });
        }
    }
    if let Err(err) = remote_wait_service_active(
        address,
        &key_path,
        "sy-orchestrator",
        CORE_SERVICE_HEALTH_TIMEOUT_SECS,
    ) {
        let journal_tail = remote_service_journal_tail(address, &key_path, "sy-orchestrator", 120);
        return serde_json::json!({
            "status": "error",
            "error_code": "SERVICE_FAILED",
            "message": match journal_tail {
                Some(tail) => format!("sy-orchestrator failed health gate: {err}; journal={tail}"),
                None => format!("sy-orchestrator failed health gate: {err}"),
            },
        });
    }

    tracing::info!(
        hive_id = hive_id,
        "add_hive bootstrap completed; vendor/dist finalize will run via worker socket"
    );

    let mut wan_connected = true;
    let mut wan_wait_error = None;
    if let Err(err) = wait_for_wan(&state.hive_id, hive_id, Duration::from_secs(60)) {
        tracing::warn!(hive_id = hive_id, error = %err, "worker WAN did not become ready in time");
        wan_connected = false;
        wan_wait_error = Some(err.to_string());
    }
    let mut orchestrator_connected = true;
    let mut orchestrator_wait_error = None;
    if wan_connected {
        if let Err(err) =
            wait_for_remote_orchestrator_node(&state.hive_id, hive_id, Duration::from_secs(60))
        {
            tracing::warn!(
                hive_id = hive_id,
                error = %err,
                "worker orchestrator did not appear in LSA in time"
            );
            orchestrator_connected = false;
            orchestrator_wait_error = Some(err.to_string());
        }
    }

    let info_payload = serde_json::json!({
        "hive_id": hive_id,
        "address": address,
        "created_at": now_epoch_ms().to_string(),
        "status": if wan_connected && orchestrator_connected { "connected" } else { "pending" },
    });
    if let Err(err) = write_hive_info(&root, hive_id, &info_payload) {
        return serde_json::json!({
            "status": "error",
            "error_code": "IO_ERROR",
            "message": err.to_string(),
        });
    }

    if !wan_connected {
        let detail = wan_wait_error.unwrap_or_else(|| "wan timeout".to_string());
        return serde_json::json!({
            "status": "error",
            "error_code": "WAN_TIMEOUT",
            "message": detail,
            "hive_id": hive_id,
            "address": address,
            "harden_ssh": harden_ssh,
            "restrict_ssh": restrict_ssh_applied,
            "restrict_ssh_requested": restrict_ssh,
            "require_dist_sync": require_dist_sync,
            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
            "wan_connected": false,
            "dist_sync_ready": dist_sync_ready,
        });
    }
    if !orchestrator_connected {
        let detail = orchestrator_wait_error
            .unwrap_or_else(|| "worker orchestrator not observed in LSA".to_string());
        let journal_tail = remote_service_journal_tail(address, &key_path, "sy-orchestrator", 120);
        return serde_json::json!({
            "status": "error",
            "error_code": "WORKER_ORCHESTRATOR_TIMEOUT",
            "message": match journal_tail {
                Some(tail) => format!("{detail}; sy-orchestrator journal={tail}"),
                None => detail,
            },
            "hive_id": hive_id,
            "address": address,
            "harden_ssh": harden_ssh,
            "restrict_ssh": restrict_ssh_applied,
            "restrict_ssh_requested": restrict_ssh,
            "require_dist_sync": require_dist_sync,
            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
            "wan_connected": true,
            "orchestrator_connected": false,
            "dist_sync_ready": dist_sync_ready,
        });
    }

    let finalize = match add_hive_finalize_via_socket(
        state,
        hive_id,
        address,
        require_dist_sync,
        dist_sync_probe_timeout_secs,
        mother_syncthing_device_id.as_deref(),
        Some(&state.hive_id),
    )
    .await
    {
        Ok(payload) => {
            append_add_hive_finalize_history(state, hive_id, "ok", None);
            payload
        }
        Err(err) => {
            append_add_hive_finalize_history(state, hive_id, "error", Some(err.to_string()));
            return serde_json::json!({
                "status": "error",
                "error_code": "FINALIZE_FAILED",
                "message": format!("worker finalize failed after bootstrap: {err}"),
                "hive_id": hive_id,
                "address": address,
                "harden_ssh": harden_ssh,
                "restrict_ssh": restrict_ssh_applied,
                "restrict_ssh_requested": restrict_ssh,
                "require_dist_sync": require_dist_sync,
                "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                "wan_connected": true,
                "orchestrator_connected": true,
                "dist_sync_ready": dist_sync_ready,
            });
        }
    };
    let mut syncthing_peer_linked = false;
    let mut worker_syncthing_device_id: Option<String> = None;
    if syncthing_expected {
        let Some(peer_device_id) = syncthing_device_id_from_finalize_payload(&finalize) else {
            return serde_json::json!({
                "status": "error",
                "error_code": "FINALIZE_FAILED",
                "message": "worker finalize missing syncthing_device_id",
                "hive_id": hive_id,
                "address": address,
                "harden_ssh": harden_ssh,
                "restrict_ssh": restrict_ssh_applied,
                "restrict_ssh_requested": restrict_ssh,
                "require_dist_sync": require_dist_sync,
                "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                "wan_connected": true,
                "orchestrator_connected": true,
                "dist_sync_ready": dist_sync_ready,
                "finalize": finalize,
            });
        };
        if let Err(err) = ensure_syncthing_peer_link_runtime(
            &desired_sync,
            &desired_blob,
            &desired_dist,
            &peer_device_id,
            hive_id,
        )
        .await
        {
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_SETUP_FAILED",
                "message": format!("local syncthing peer reconcile failed: {err}"),
                "hive_id": hive_id,
                "address": address,
                "harden_ssh": harden_ssh,
                "restrict_ssh": restrict_ssh_applied,
                "restrict_ssh_requested": restrict_ssh,
                "require_dist_sync": require_dist_sync,
                "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                "wan_connected": true,
                "orchestrator_connected": true,
                "dist_sync_ready": dist_sync_ready,
                "finalize": finalize,
            });
        }
        worker_syncthing_device_id = Some(peer_device_id);
        syncthing_peer_linked = true;
    }
    dist_sync_ready = dist_sync_ready_from_finalize_payload(&finalize, &desired_dist);
    if require_dist_sync && !dist_sync_ready {
        return serde_json::json!({
            "status": "error",
            "error_code": "DIST_SYNC_TIMEOUT",
            "message": "worker finalize completed but dist sync is not ready",
            "hive_id": hive_id,
            "address": address,
            "harden_ssh": harden_ssh,
            "restrict_ssh": restrict_ssh_applied,
            "restrict_ssh_requested": restrict_ssh,
            "require_dist_sync": require_dist_sync,
            "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
            "wan_connected": true,
            "orchestrator_connected": true,
            "dist_sync_ready": dist_sync_ready,
            "finalize": finalize,
        });
    }

    let ssh_controls = match apply_add_hive_ssh_controls_after_finalize(
        address,
        &key_path,
        &pub_key,
        restrict_ssh,
        harden_ssh,
    ) {
        Ok(result) => result,
        Err(err) => {
            let err_text = err.to_string();
            let error_code = if err_text.to_ascii_lowercase().contains("harden") {
                "SSH_HARDEN_FAILED"
            } else {
                "SSH_KEY_FAILED"
            };
            return serde_json::json!({
                "status": "error",
                "error_code": error_code,
                "message": err_text,
                "hive_id": hive_id,
                "address": address,
                "harden_ssh": harden_ssh,
                "restrict_ssh": false,
                "restrict_ssh_requested": restrict_ssh,
                "require_dist_sync": require_dist_sync,
                "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                "wan_connected": true,
                "orchestrator_connected": true,
                "dist_sync_ready": dist_sync_ready,
                "finalize": finalize,
            });
        }
    };
    restrict_ssh_applied = ssh_controls.restrict_ssh_applied;
    let restrict_ssh_mode = ssh_controls.restrict_ssh_mode;

    let mut final_info_payload = serde_json::json!({
        "hive_id": hive_id,
        "address": address,
        "created_at": now_epoch_ms().to_string(),
        "status": "connected",
        "syncthing_peer_linked": syncthing_peer_linked,
    });
    if let Some(device_id) = worker_syncthing_device_id.clone() {
        final_info_payload["syncthing_device_id"] = serde_json::json!(device_id);
    }
    if let Err(err) = write_hive_info(&root, hive_id, &final_info_payload) {
        return serde_json::json!({
            "status": "error",
            "error_code": "IO_ERROR",
            "message": err.to_string(),
        });
    }

    serde_json::json!({
        "status": "ok",
        "hive_id": hive_id,
        "address": address,
        "harden_ssh": harden_ssh,
        "harden_ssh_applied": harden_ssh,
        "restrict_ssh": restrict_ssh_applied,
        "restrict_ssh_mode": restrict_ssh_mode,
        "restrict_ssh_requested": restrict_ssh,
        "require_dist_sync": require_dist_sync,
        "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
        "wan_connected": true,
        "orchestrator_connected": true,
        "dist_sync_ready": dist_sync_ready,
        "syncthing_peer_linked": syncthing_peer_linked,
        "syncthing_device_id": worker_syncthing_device_id,
        "finalize": finalize,
    })
}

fn ssh_bootstrap_error_payload(error: &str) -> serde_json::Value {
    let lower = error.to_ascii_lowercase();
    let code = if lower.contains("connection timed out") || lower.contains("operation timed out") {
        "SSH_TIMEOUT"
    } else if lower.contains("connection refused") {
        "SSH_CONNECTION_REFUSED"
    } else {
        "SSH_AUTH_FAILED"
    };
    serde_json::json!({
        "status": "error",
        "error_code": code,
        "message": error,
    })
}

fn hive_exists(state_dir: &Path, hive_id: &str) -> bool {
    let _ = state_dir;
    hives_root().join(hive_id).join("info.yaml").exists()
}

fn hive_partial_exists(hive_id: &str) -> bool {
    let dir = hives_root().join(hive_id);
    dir.exists() && !dir.join("info.yaml").exists()
}

fn valid_hive_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_address(value: &str) -> bool {
    if value.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let value = value.trim();
    if value.is_empty() || value.len() > 253 {
        return false;
    }
    let labels = value.split('.').collect::<Vec<_>>();
    if labels
        .iter()
        .any(|label| label.is_empty() || label.len() > 63)
    {
        return false;
    }
    labels.iter().all(|label| {
        let bytes = label.as_bytes();
        if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
            return false;
        }
        bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
    })
}

fn resolve_add_hive_harden_ssh(payload: &serde_json::Value) -> bool {
    if let Some(value) = payload.get("harden_ssh").and_then(parse_bool_value) {
        return value;
    }
    false
}

fn resolve_add_hive_restrict_ssh(payload: &serde_json::Value, harden_ssh: bool) -> bool {
    if let Some(value) = payload.get("restrict_ssh").and_then(parse_bool_value) {
        return value;
    }
    if matches!(
        payload.get("harden_ssh").and_then(parse_bool_value),
        Some(false)
    ) {
        return false;
    }
    if !harden_ssh {
        return false;
    }
    true
}

fn resolve_add_hive_require_dist_sync(payload: &serde_json::Value) -> bool {
    if let Some(value) = payload.get("require_dist_sync").and_then(parse_bool_value) {
        return value;
    }
    if let Ok(raw) = std::env::var("FLUXBEE_ADD_HIVE_REQUIRE_DIST_SYNC") {
        if let Some(value) = parse_bool_str(&raw) {
            return value;
        }
    }
    if let Ok(raw) = std::env::var("JSR_ADD_HIVE_REQUIRE_DIST_SYNC") {
        if let Some(value) = parse_bool_str(&raw) {
            return value;
        }
    }
    false
}

fn resolve_add_hive_dist_sync_probe_timeout_secs(payload: &serde_json::Value) -> u64 {
    let from_payload = payload
        .get("dist_sync_probe_timeout_secs")
        .or_else(|| payload.get("dist_sync_timeout_secs"))
        .and_then(|value| value.as_u64());
    let from_env = std::env::var("FLUXBEE_ADD_HIVE_DIST_SYNC_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .or_else(|| {
            std::env::var("JSR_ADD_HIVE_DIST_SYNC_TIMEOUT_SECS")
                .ok()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
        });
    let raw = from_payload
        .or(from_env)
        .unwrap_or(DIST_SYNC_PROBE_TIMEOUT_SECS);
    raw.clamp(5, 600)
}

fn resolve_add_hive_authkey_source_patterns(address: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(raw) = std::env::var("ORCH_AUTHKEY_FROM_PATTERNS") {
        out.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| item.to_string()),
        );
    }
    if out.is_empty() {
        match detect_source_ip_for_target(address) {
            Ok(ip) if !ip.trim().is_empty() => out.push(ip),
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(
                    target = address,
                    error = %err,
                    "could not auto-resolve source ip for authorized_keys from= restriction; leaving from filter disabled"
                );
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn parse_bool_value(value: &serde_json::Value) -> Option<bool> {
    if let Some(boolean) = value.as_bool() {
        return Some(boolean);
    }
    let raw = value.as_str()?;
    parse_bool_str(raw)
}

fn parse_bool_str(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn hives_root() -> PathBuf {
    json_router::paths::storage_root_dir().join("hives")
}

fn run_cmd(mut cmd: Command, label: &str) -> Result<(), OrchestratorError> {
    let output = cmd.output()?;
    if output.status.success() {
        return Ok(());
    }
    let code = output
        .status
        .code()
        .map_or("signal".to_string(), |c| c.to_string());
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        format!("stderr={stderr}")
    } else if !stdout.is_empty() {
        format!("stdout={stdout}")
    } else {
        "no stdout/stderr".to_string()
    };
    Err(format!("{label} failed (exit={code}): {detail}").into())
}

fn run_cmd_output(mut cmd: Command, label: &str) -> Result<String, OrchestratorError> {
    let output = cmd.output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let code = output
        .status
        .code()
        .map_or("signal".to_string(), |c| c.to_string());
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        format!("stderr={stderr}")
    } else if !stdout.is_empty() {
        format!("stdout={stdout}")
    } else {
        "no stdout/stderr".to_string()
    };
    Err(format!("{label} failed (exit={code}): {detail}").into())
}

fn systemd_is_active(service: &str) -> bool {
    Command::new("systemctl")
        .arg("is-active")
        .arg("--quiet")
        .arg(service)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn systemd_unit_exists(service: &str) -> bool {
    Command::new("systemctl")
        .arg("show")
        .arg(format!("{service}.service"))
        .arg("--property=LoadState")
        .arg("--value")
        .output()
        .ok()
        .map(|output| {
            if !output.status.success() {
                return false;
            }
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !state.is_empty() && state != "not-found"
        })
        .unwrap_or(false)
}

fn systemd_start(service: &str) -> Result<(), OrchestratorError> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("start").arg(service);
    run_cmd(cmd, "systemctl start")
}

fn systemd_stop(service: &str) -> Result<(), OrchestratorError> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("stop").arg(service);
    run_cmd(cmd, "systemctl stop")
}

fn systemd_disable(service: &str) -> Result<(), OrchestratorError> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("disable").arg(service);
    run_cmd(cmd, "systemctl disable")
}

struct AddHiveSshControlsResult {
    restrict_ssh_applied: bool,
    restrict_ssh_mode: String,
}

fn apply_add_hive_ssh_controls_after_finalize(
    address: &str,
    key_path: &Path,
    pub_key: &str,
    restrict_ssh_requested: bool,
    harden_ssh: bool,
) -> Result<AddHiveSshControlsResult, OrchestratorError> {
    let mut restrict_ssh_applied = false;
    let mut restrict_ssh_mode = "unrestricted".to_string();

    if restrict_ssh_requested {
        let source_patterns = resolve_add_hive_authkey_source_patterns(address);
        let restrict_result = apply_remote_from_only_authorized_key_with_access(
            address,
            key_path,
            pub_key,
            &source_patterns,
        )
        .and_then(|_| {
            ssh_with_key(
                address,
                key_path,
                "sudo -n /bin/bash -lc 'exit 0'",
                BOOTSTRAP_SSH_USER,
            )
        });
        match restrict_result {
            Ok(()) => {
                restrict_ssh_applied = true;
                restrict_ssh_mode = "from_only".to_string();
                tracing::info!(
                    target = address,
                    from_patterns = ?source_patterns,
                    "authorized_keys restriction applied in from-only mode (post-finalize)"
                );
            }
            Err(err) => {
                return Err(format!(
                    "restrict_ssh requested but from-only restriction failed: {err}"
                )
                .into());
            }
        }
    } else {
        tracing::info!(
            target = address,
            "authorized_keys restriction not requested; leaving bootstrap key unrestricted"
        );
    }

    if harden_ssh {
        disable_remote_password_auth_with_access(address, key_path)
            .map_err(|err| format!("ssh hardening failed: {err}"))?;
        verify_remote_ssh_hardening_with_access(address, key_path)
            .map_err(|err| format!("ssh hardening verification failed: {err}"))?;
    }

    Ok(AddHiveSshControlsResult {
        restrict_ssh_applied,
        restrict_ssh_mode,
    })
}

fn disable_remote_password_auth_with_access(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let set_password_auth_cmd = r#"bash -lc 'set -euo pipefail
cfg="/etc/ssh/sshd_config"
drop="/etc/ssh/sshd_config.d/00-fluxbee-hardening.conf"
mkdir -p /etc/ssh/sshd_config.d

upsert_opt() {
  local key="$1"
  local val="$2"
  local file="$3"
  if grep -Eq "^[[:space:]]*#?[[:space:]]*${key}[[:space:]]+" "$file"; then
    sed -i -E "s|^[[:space:]]*#?[[:space:]]*${key}[[:space:]]+.*|${key} ${val}|" "$file"
  else
    printf "\n%s %s\n" "$key" "$val" >> "$file"
  fi
}

upsert_opt "PasswordAuthentication" "no" "$cfg"
upsert_opt "KbdInteractiveAuthentication" "no" "$cfg"
upsert_opt "ChallengeResponseAuthentication" "no" "$cfg"

for f in /etc/ssh/sshd_config.d/*.conf; do
  [ -f "$f" ] || continue
  sed -i -E "s|^[[:space:]]*#?[[:space:]]*PasswordAuthentication[[:space:]]+.*|PasswordAuthentication no|" "$f" || true
  sed -i -E "s|^[[:space:]]*#?[[:space:]]*KbdInteractiveAuthentication[[:space:]]+.*|KbdInteractiveAuthentication no|" "$f" || true
  sed -i -E "s|^[[:space:]]*#?[[:space:]]*ChallengeResponseAuthentication[[:space:]]+.*|ChallengeResponseAuthentication no|" "$f" || true
done

cat > "$drop" <<EOF
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
EOF
chmod 0644 "$drop"

if command -v sshd >/dev/null 2>&1; then
  sshd -t >/dev/null 2>&1 || true
elif [ -x /usr/sbin/sshd ]; then
  /usr/sbin/sshd -t >/dev/null 2>&1 || true
fi'"#;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(set_password_auth_cmd),
        BOOTSTRAP_SSH_USER,
    )?;

    let restart_ssh_cmd = r#"bash -lc "systemctl restart sshd || systemctl restart ssh || service sshd restart || service ssh restart""#;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(restart_ssh_cmd),
        BOOTSTRAP_SSH_USER,
    )?;
    Ok(())
}

fn verify_remote_ssh_hardening_with_access(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    verify_remote_key_access_after_hardening(address, key_path)?;
    verify_remote_password_auth_is_rejected(address)?;
    Ok(())
}

fn verify_remote_key_access_after_hardening(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let cmd = "sudo -n /bin/bash -lc 'exit 0'";
    let mut last_err: Option<String> = None;
    for attempt in 1..=SSH_HARDEN_VERIFY_RETRIES {
        match ssh_with_key(address, key_path, cmd, BOOTSTRAP_SSH_USER) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_err = Some(err.to_string());
                if attempt < SSH_HARDEN_VERIFY_RETRIES {
                    std::thread::sleep(Duration::from_millis(SSH_HARDEN_VERIFY_DELAY_MS));
                }
            }
        }
    }
    Err(format!(
        "post-harden key verification failed (sudo -n not reachable after {} attempts): {}",
        SSH_HARDEN_VERIFY_RETRIES,
        last_err.unwrap_or_else(|| "unknown error".to_string())
    )
    .into())
}

fn verify_remote_password_auth_is_rejected(address: &str) -> Result<(), OrchestratorError> {
    let mut last_err: Option<String> = None;
    for attempt in 1..=SSH_HARDEN_VERIFY_RETRIES {
        match ssh_with_pass_any(address, "true", BOOTSTRAP_SSH_USER) {
            Ok(()) => {
                return Err(
                    "password authentication still accepted after hardening; expected rejection"
                        .into(),
                )
            }
            Err(err) => {
                let msg = err.to_string();
                if is_password_auth_rejection_error(&msg) {
                    return Ok(());
                }
                last_err = Some(msg);
                if attempt < SSH_HARDEN_VERIFY_RETRIES {
                    std::thread::sleep(Duration::from_millis(SSH_HARDEN_VERIFY_DELAY_MS));
                }
            }
        }
    }

    Err(format!(
        "password-auth rejection verification failed after {} attempts: {}",
        SSH_HARDEN_VERIFY_RETRIES,
        last_err.unwrap_or_else(|| "unknown error".to_string())
    )
    .into())
}

fn is_password_auth_rejection_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("permission denied")
        || lower.contains("no supported authentication methods available")
        || lower.contains("authentications that can continue")
        || lower.contains("(publickey")
}

fn apply_remote_from_only_authorized_key_with_access(
    address: &str,
    key_path: &Path,
    pub_key: &str,
    source_patterns: &[String],
) -> Result<(), OrchestratorError> {
    let key_material = pub_key
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "invalid public key format: missing key material".to_string())?;
    let from_value = source_patterns.join(",");
    let entry = if from_value.is_empty() {
        format!(
            "no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty {pub_key}",
            pub_key = pub_key
        )
    } else {
        format!(
            "from=\"{from_value}\",no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty {pub_key}",
            from_value = from_value,
            pub_key = pub_key
        )
    };
    let script = format!(
        "set -euo pipefail\n\
user='{user}'\n\
home_dir=\"$(getent passwd \"$user\" | cut -d: -f6)\"\n\
if [[ -z \"$home_dir\" ]]; then home_dir=\"/home/$user\"; fi\n\
ssh_dir=\"$home_dir/.ssh\"\n\
auth_keys=\"$ssh_dir/authorized_keys\"\n\
mkdir -p \"$ssh_dir\"\n\
chown \"$user:$user\" \"$ssh_dir\"\n\
chmod 700 \"$ssh_dir\"\n\
touch \"$auth_keys\"\n\
chown \"$user:$user\" \"$auth_keys\"\n\
{{ grep -Fv '{gate_path}' \"$auth_keys\" | grep -Fv '{key_material}' > \"$auth_keys.tmp\"; }} || true\n\
mv \"$auth_keys.tmp\" \"$auth_keys\"\n\
printf '%s\\n' '{entry}' >> \"$auth_keys\"\n\
chown \"$user:$user\" \"$auth_keys\"\n\
chmod 600 \"$auth_keys\"\n",
        user = BOOTSTRAP_SSH_USER,
        gate_path = shell_single_quote(ORCH_SSH_GATE_PATH),
        key_material = shell_single_quote(key_material),
        entry = shell_single_quote(&entry),
    );
    let cmd = sudo_wrap(&format!("bash -lc '{}'", shell_single_quote(&script)));
    ssh_with_key(address, key_path, &cmd, BOOTSTRAP_SSH_USER)?;
    Ok(())
}

fn apply_remote_unrestricted_authorized_key_with_pass(
    address: &str,
    pub_key: &str,
) -> Result<(), OrchestratorError> {
    let key_material = pub_key
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "invalid public key format: missing key material".to_string())?;
    let script = format!(
        "set -euo pipefail\n\
mkdir -p ~/.ssh\n\
chmod 700 ~/.ssh\n\
touch ~/.ssh/authorized_keys\n\
{{ grep -Fv '{gate_path}' ~/.ssh/authorized_keys | grep -Fv '{key_material}' > ~/.ssh/authorized_keys.tmp; }} || true\n\
mv ~/.ssh/authorized_keys.tmp ~/.ssh/authorized_keys\n\
printf '%s\\n' '{entry}' >> ~/.ssh/authorized_keys\n\
chmod 600 ~/.ssh/authorized_keys\n",
        gate_path = shell_single_quote(ORCH_SSH_GATE_PATH),
        key_material = shell_single_quote(key_material),
        entry = shell_single_quote(pub_key),
    );
    let cmd = format!("bash -lc '{}'", shell_single_quote(&script));
    ssh_with_pass_any(address, &cmd, BOOTSTRAP_SSH_USER)?;
    Ok(())
}

fn remote_orchestrator_sudoers_contents() -> String {
    format!(
        "Defaults:{} !requiretty\n{} ALL=(root) NOPASSWD: /bin/systemctl, /usr/bin/systemctl, /bin/systemd-run, /usr/bin/systemd-run, /usr/bin/install, /bin/mkdir, /usr/bin/mkdir, /bin/rm, /usr/bin/rm, /bin/cp, /usr/bin/cp, /bin/mv, /usr/bin/mv, /bin/cat, /usr/bin/cat, /usr/bin/sha256sum, /usr/bin/stat, /usr/bin/tee, /bin/chmod, /usr/bin/chmod, /bin/chown, /usr/bin/chown, /usr/bin/rsync, /usr/sbin/ufw, /usr/bin/firewall-cmd, /usr/sbin/service, /bin/bash, /usr/bin/bash\n",
        BOOTSTRAP_SSH_USER, BOOTSTRAP_SSH_USER
    )
}

fn ensure_remote_orchestrator_sudoers_with_access(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    // Always rewrite sudoers from known-good template.
    // This avoids stale/partial states that can leave sudo -n inconsistent.

    let local_tmp = std::env::temp_dir().join(format!(
        "fluxbee-orchestrator-sudoers-{}.tmp",
        now_epoch_ms()
    ));
    fs::write(&local_tmp, remote_orchestrator_sudoers_contents())?;
    let local_tmp_str = local_tmp.to_string_lossy().to_string();
    let local_refs = [local_tmp_str.as_str()];
    let remote_tmp = format!("/tmp/fluxbee-orchestrator-sudoers-{}", now_epoch_ms());
    let upload_result = scp_with_key(
        address,
        key_path,
        &local_refs,
        &remote_tmp,
        BOOTSTRAP_SSH_USER,
    );
    if let Err(err) = upload_result {
        let _ = fs::remove_file(&local_tmp);
        return Err(format!("failed to upload sudoers bootstrap: {err}").into());
    }
    let script = format!(
        "set -euo pipefail && install -m 0440 '{remote_tmp}' '{sudoers}' && visudo -cf '{sudoers}' >/dev/null && rm -f '{remote_tmp}'",
        remote_tmp = shell_single_quote(&remote_tmp),
        sudoers = shell_single_quote(ORCH_SUDOERS_PATH),
    );
    let apply_cmd = sudo_wrap_with_pass(&format!("bash -lc '{}'", shell_single_quote(&script)));
    let apply_result = ssh_with_key(address, key_path, &apply_cmd, BOOTSTRAP_SSH_USER);
    let _ = fs::remove_file(&local_tmp);
    apply_result?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("/bin/systemctl --version"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (systemctl): {err}"))?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("systemd-run --version"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (systemd-run): {err}"))?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("/usr/bin/install --version"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (install): {err}"))?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("/bin/chmod --version"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (chmod): {err}"))?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("/bin/bash -lc 'exit 0'"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (bash): {err}"))?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("/usr/bin/mkdir -p /tmp"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (mkdir): {err}"))?;
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("/usr/bin/cat /etc/hosts >/dev/null"),
        BOOTSTRAP_SSH_USER,
    )
    .map_err(|err| format!("sudo -n unavailable after sudoers bootstrap (cat): {err}"))?;
    Ok(())
}

fn identity_available() -> bool {
    Path::new("/usr/bin/sy-identity").exists()
}

fn askpass_script(password: &str) -> Result<PathBuf, OrchestratorError> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("jsr-askpass-{}.sh", now_epoch_ms()));
    let contents = format!("#!/bin/sh\necho \"{}\"\n", password.replace('"', "\\\""));
    fs::write(&path, contents)?;
    let mut perms = fs::metadata(&path)?.permissions();
    perms.set_readonly(false);
    fs::set_permissions(&path, perms)?;
    let mut chmod = Command::new("chmod");
    chmod.arg("700").arg(&path);
    run_cmd(chmod, "chmod")?;
    Ok(path)
}

fn ssh_with_pass(address: &str, command: &str, user: &str) -> Result<(), OrchestratorError> {
    let askpass = askpass_script(BOOTSTRAP_SSH_PASS)?;
    let mut cmd = Command::new("setsid");
    cmd.arg("ssh")
        .arg("-o")
        .arg("PreferredAuthentications=password")
        .arg("-o")
        .arg("PubkeyAuthentication=no")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(format!("{user}@{address}"))
        .arg(command)
        .env("SSH_ASKPASS", &askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "jsr");
    let result = run_cmd(cmd, "ssh");
    let _ = fs::remove_file(&askpass);
    result
}

fn ssh_with_pass_kbd(address: &str, command: &str, user: &str) -> Result<(), OrchestratorError> {
    let askpass = askpass_script(BOOTSTRAP_SSH_PASS)?;
    let mut cmd = Command::new("setsid");
    cmd.arg("ssh")
        .arg("-o")
        .arg("PreferredAuthentications=keyboard-interactive,password")
        .arg("-o")
        .arg("KbdInteractiveAuthentication=yes")
        .arg("-o")
        .arg("PasswordAuthentication=yes")
        .arg("-o")
        .arg("PubkeyAuthentication=no")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(format!("{user}@{address}"))
        .arg(command)
        .env("SSH_ASKPASS", &askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "jsr");
    let result = run_cmd(cmd, "ssh");
    let _ = fs::remove_file(&askpass);
    result
}

fn ssh_with_pass_any(address: &str, command: &str, user: &str) -> Result<(), OrchestratorError> {
    match ssh_with_pass(address, command, user) {
        Ok(()) => Ok(()),
        Err(pass_err) => match ssh_with_pass_kbd(address, command, user) {
            Ok(()) => {
                tracing::warn!(
                    target = address,
                    error = %pass_err,
                    "password auth fallback: keyboard-interactive succeeded"
                );
                Ok(())
            }
            Err(kbd_err) => {
                Err(format!("{pass_err}; keyboard-interactive auth failed: {kbd_err}").into())
            }
        },
    }
}

fn ssh_with_key(
    address: &str,
    key_path: &Path,
    command: &str,
    user: &str,
) -> Result<(), OrchestratorError> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-i")
        .arg(key_path)
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg("PreferredAuthentications=publickey")
        .arg("-o")
        .arg("PasswordAuthentication=no")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(format!("{user}@{address}"))
        .arg(command);
    run_cmd(cmd, "ssh")
}

fn ssh_with_key_output(
    address: &str,
    key_path: &Path,
    command: &str,
    user: &str,
) -> Result<String, OrchestratorError> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-i")
        .arg(key_path)
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg("PreferredAuthentications=publickey")
        .arg("-o")
        .arg("PasswordAuthentication=no")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(format!("{user}@{address}"))
        .arg(command);
    run_cmd_output(cmd, "ssh")
}

fn scp_with_key(
    address: &str,
    key_path: &Path,
    sources: &[&str],
    dest: &str,
    user: &str,
) -> Result<(), OrchestratorError> {
    let mut cmd = Command::new("scp");
    cmd.arg("-i")
        .arg(key_path)
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg("PreferredAuthentications=publickey")
        .arg("-o")
        .arg("PasswordAuthentication=no")
        .arg("-o")
        .arg("LogLevel=ERROR")
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("ConnectTimeout=10");
    for src in sources {
        cmd.arg(src);
    }
    cmd.arg(format!("{user}@{address}:{dest}"));
    run_cmd(cmd, "scp")
}

fn public_key_from_private_key(key_path: &Path) -> Result<String, OrchestratorError> {
    let mut cmd = Command::new("ssh-keygen");
    cmd.arg("-y").arg("-f").arg(key_path);
    let out = run_cmd_output(cmd, "ssh-keygen -y")?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Err("derived empty public key".into());
    }
    Ok(trimmed.to_string())
}

fn write_remote_file(
    address: &str,
    key_path: &Path,
    remote_path: &str,
    contents: &str,
) -> Result<(), OrchestratorError> {
    let escaped = contents.replace('\'', "'\"'\"'");
    let cmd = format!("cat > {} <<'EOF'\n{}\nEOF", remote_path, escaped);
    let sudo_cmd = sudo_wrap(&format!("bash -lc \"{}\"", cmd.replace('"', "\\\"")));
    ssh_with_key(address, key_path, &sudo_cmd, BOOTSTRAP_SSH_USER)
}

fn wait_for_wan(
    hive_id: &str,
    remote_id: &str,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    let shm_name = format!("/jsr-lsa-{}", hive_id);
    let reader = LsaRegionReader::open_read_only(&shm_name)?;
    let deadline = Instant::now() + timeout;
    let mut last_visible: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        if let Some(snapshot) = reader.read_snapshot() {
            let now = now_epoch_ms();
            let mut visible: Vec<String> = snapshot
                .hives
                .iter()
                .filter_map(|entry| {
                    let hive = remote_hive_name(entry)?;
                    let status = remote_hive_status(entry, now);
                    let age_ms = now.saturating_sub(entry.last_updated);
                    Some(format!("{hive}({status},age_ms={age_ms})"))
                })
                .collect();
            visible.sort();
            visible.dedup();
            last_visible = visible;
            if snapshot.hives.iter().any(|entry| {
                remote_hive_match(entry, remote_id) && !remote_hive_is_stale(entry, now)
            }) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if last_visible.is_empty() {
        Err(format!("wan timeout: no remote hives observed in lsa for '{remote_id}'").into())
    } else {
        Err(format!(
            "wan timeout: remote hive '{remote_id}' not fresh/alive in lsa (visible: {})",
            last_visible.join(", ")
        )
        .into())
    }
}

fn wait_for_remote_orchestrator_node(
    hive_id: &str,
    remote_id: &str,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    let expected_node = ensure_l2_name("SY.orchestrator", remote_id);
    let shm_name = format!("/jsr-lsa-{}", hive_id);
    let reader = LsaRegionReader::open_read_only(&shm_name)?;
    let deadline = Instant::now() + timeout;
    let mut last_visible_nodes: Vec<String> = Vec::new();

    while Instant::now() < deadline {
        if let Some(snapshot) = reader.read_snapshot() {
            let now = now_epoch_ms();
            let mut visible_nodes = Vec::new();
            for node in &snapshot.nodes {
                let hive_idx = node.hive_index as usize;
                let Some(hive_entry) = snapshot.hives.get(hive_idx) else {
                    continue;
                };
                let Some(hive_name) = remote_hive_name(hive_entry) else {
                    continue;
                };
                if hive_name != remote_id {
                    continue;
                }
                if remote_hive_is_stale(hive_entry, now) {
                    continue;
                }
                if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
                    continue;
                }
                if node.name_len == 0 {
                    continue;
                }
                let node_name =
                    String::from_utf8_lossy(&node.name[..node.name_len as usize]).into_owned();
                visible_nodes.push(node_name.clone());
                if node_name == expected_node || node_name == "SY.orchestrator" {
                    return Ok(());
                }
            }
            visible_nodes.sort();
            visible_nodes.dedup();
            last_visible_nodes = visible_nodes;
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    if last_visible_nodes.is_empty() {
        Err(format!(
            "orchestrator timeout: node '{}' not observed on hive '{}' (no remote nodes visible)",
            expected_node, remote_id
        )
        .into())
    } else {
        Err(format!(
            "orchestrator timeout: node '{}' not observed on hive '{}' (visible: {})",
            expected_node,
            remote_id,
            last_visible_nodes.join(", ")
        )
        .into())
    }
}

fn remote_hive_match(entry: &RemoteHiveEntry, target: &str) -> bool {
    if entry.hive_id_len == 0 {
        return false;
    }
    let len = entry.hive_id_len as usize;
    let name = String::from_utf8_lossy(&entry.hive_id[..len]);
    name == target
}

fn remote_hive_name(entry: &RemoteHiveEntry) -> Option<String> {
    if entry.hive_id_len == 0 {
        return None;
    }
    let len = entry.hive_id_len as usize;
    Some(String::from_utf8_lossy(&entry.hive_id[..len]).into_owned())
}

fn remote_flags_status(flags: u16) -> &'static str {
    if flags & FLAG_DELETED != 0 {
        "deleted"
    } else if flags & FLAG_STALE != 0 {
        "stale"
    } else {
        "active"
    }
}

fn remote_hive_is_stale(entry: &RemoteHiveEntry, now: u64) -> bool {
    (entry.flags & (FLAG_DELETED | FLAG_STALE)) != 0
        || now.saturating_sub(entry.last_updated) > HEARTBEAT_STALE_MS
}

fn remote_hive_status(entry: &RemoteHiveEntry, now: u64) -> &'static str {
    if entry.flags & FLAG_DELETED != 0 {
        "deleted"
    } else if remote_hive_is_stale(entry, now) {
        "stale"
    } else {
        "alive"
    }
}

fn resolve_worker_uplink_address(
    wan_listen: &str,
    worker_address: &str,
) -> Result<String, OrchestratorError> {
    let listen = wan_listen.trim();
    if listen.is_empty() {
        return Err("wan.listen empty".into());
    }

    let (host, port) = parse_host_port(listen)?;
    if host == "0.0.0.0" || host == "::" || host == "[::]" || host == "*" {
        let src = detect_source_ip_for_target(worker_address)?;
        return Ok(format!("{src}:{port}"));
    }
    Ok(format!("{host}:{port}"))
}

fn parse_host_port(listen: &str) -> Result<(String, u16), OrchestratorError> {
    if let Ok(addr) = listen.parse::<std::net::SocketAddr>() {
        return Ok((addr.ip().to_string(), addr.port()));
    }
    if let Some((host, port_raw)) = listen.rsplit_once(':') {
        let port = port_raw
            .parse::<u16>()
            .map_err(|_| format!("invalid port in wan.listen: {listen}"))?;
        return Ok((host.trim().to_string(), port));
    }
    Ok((listen.to_string(), 9000))
}

fn detect_source_ip_for_target(target: &str) -> Result<String, OrchestratorError> {
    let mut cmd = Command::new("ip");
    cmd.arg("-4").arg("route").arg("get").arg(target);
    let out = run_cmd_output(cmd, "ip route get")?;
    let mut parts = out.split_whitespace();
    while let Some(part) = parts.next() {
        if part == "src" {
            if let Some(src) = parts.next() {
                return Ok(src.to_string());
            }
        }
    }
    Err(format!("could not resolve source ip for target {target}").into())
}

fn sudo_wrap(cmd: &str) -> String {
    format!("sudo -n {}", cmd)
}

fn sudo_wrap_with_pass(cmd: &str) -> String {
    let pass = BOOTSTRAP_SSH_PASS.replace('\'', "'\"'\"'");
    format!("echo '{}' | sudo -S -p '' {}", pass, cmd)
}

fn storage_path_from_hive(hive: &HiveFile) -> String {
    let default_root = json_router::paths::storage_root_dir()
        .to_string_lossy()
        .to_string();
    if let Some(path) = hive
        .storage
        .as_ref()
        .and_then(|storage| storage.path.as_ref())
    {
        if !path.trim().is_empty() {
            return path.to_string();
        }
    }
    default_root
}

fn identity_frontdesk_node_name_from_hive(hive: &HiveFile) -> String {
    let configured = hive
        .government
        .as_ref()
        .and_then(|government| government.identity_frontdesk.as_ref())
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    match configured {
        Some(value) if value.contains('@') => value.to_string(),
        Some(value) => format!("{value}@{}", hive.hive_id),
        None => format!("AI.frontdesk@{}", hive.hive_id),
    }
}

fn identity_sync_port_from_hive(hive: &HiveFile) -> u16 {
    hive.identity
        .as_ref()
        .and_then(|identity| identity.sync.as_ref())
        .and_then(|sync| sync.port)
        .unwrap_or(DEFAULT_IDENTITY_SYNC_PORT)
}

fn format_host_port(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn persist_storage_path_in_hive(config_dir: &Path, path: &str) -> Result<(), OrchestratorError> {
    let hive_path = config_dir.join("hive.yaml");
    let data = fs::read_to_string(&hive_path)?;
    let mut root: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let Some(root_map) = root.as_mapping_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "hive.yaml root must be a map",
        )
        .into());
    };

    let storage_key = serde_yaml::Value::String("storage".to_string());
    let storage_entry = root_map
        .entry(storage_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !storage_entry.is_mapping() {
        *storage_entry = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let Some(storage_map) = storage_entry.as_mapping_mut() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "hive.yaml storage must be a map",
        )
        .into());
    };
    storage_map.insert(
        serde_yaml::Value::String("path".to_string()),
        serde_yaml::Value::String(path.to_string()),
    );

    let serialized = serde_yaml::to_string(&root)?;
    fs::write(hive_path, serialized)?;
    Ok(())
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee")
}

fn is_worker_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "worker")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_DEVICE_ID: &str = "V7TZE22-7TDF4XG-KXILPOJ-NXFPHYF-HPXR2AH-YCZKW5G-XAOGXS3-AKHUFAC";
    const PEER_DEVICE_ID: &str = "I7MM32M-LYH7OVN-TCPA6G4-MIFHRC6-T7RKRVN-Q35OZRX-MGZMVA3-X7Y24QF";

    fn write_name(buf: &mut [u8], value: &str) -> u16 {
        let bytes = value.as_bytes();
        let len = bytes.len().min(buf.len());
        buf[..len].copy_from_slice(&bytes[..len]);
        len as u16
    }

    fn sample_syncthing_config_with_peer() -> String {
        format!(
            "<configuration version=\"37\">
  <device id=\"{local}\" name=\"sandbox\"></device>
  <device id=\"{peer}\" name=\"worker-220\"></device>
  <folder id=\"{blob_id}\" label=\"Fluxbee Blob\" path=\"/var/lib/fluxbee/blob/active\" type=\"sendreceive\">
    <device id=\"{local}\" introducedBy=\"\"/>
    <device id=\"{peer}\" introducedBy=\"\"/>
    <minDiskFree unit=\"%\">1</minDiskFree>
  </folder>
  <folder id=\"{dist_id}\" label=\"Fluxbee Dist\" path=\"/var/lib/fluxbee/dist\" type=\"sendreceive\">
    <device id=\"{local}\" introducedBy=\"\"/>
    <device id=\"{peer}\" introducedBy=\"\"/>
    <minDiskFree unit=\"%\">1</minDiskFree>
  </folder>
</configuration>",
            local = LOCAL_DEVICE_ID,
            peer = PEER_DEVICE_ID,
            blob_id = SYNCTHING_FOLDER_BLOB_ID,
            dist_id = SYNCTHING_FOLDER_DIST_ID
        )
    }

    fn sample_blob_config() -> BlobRuntimeConfig {
        BlobRuntimeConfig {
            enabled: true,
            path: PathBuf::from("/var/lib/fluxbee/blob"),
            sync_enabled: true,
            sync_tool: "syncthing".to_string(),
            sync_api_port: 8384,
            sync_data_dir: PathBuf::from("/var/lib/fluxbee/syncthing"),
            sync_service_user: DEFAULT_BLOB_SYNC_SERVICE_USER.to_string(),
            sync_allow_root_fallback: DEFAULT_BLOB_SYNC_ALLOW_ROOT_FALLBACK,
            gc_enabled: DEFAULT_BLOB_GC_ENABLED,
            gc_interval_secs: DEFAULT_BLOB_GC_INTERVAL_SECS,
            gc_apply: DEFAULT_BLOB_GC_APPLY,
            gc_staging_ttl_hours: DEFAULT_BLOB_GC_STAGING_TTL_HOURS,
            gc_active_retain_days: DEFAULT_BLOB_GC_ACTIVE_RETAIN_DAYS,
        }
    }

    fn sample_dist_config() -> DistRuntimeConfig {
        DistRuntimeConfig {
            path: PathBuf::from("/var/lib/fluxbee/dist"),
            sync_enabled: true,
            sync_tool: "syncthing".to_string(),
        }
    }

    #[test]
    fn remove_hive_cleanup_script_cleans_persisted_node_dirs() {
        let script = remove_hive_cleanup_script();
        assert!(script.contains("/var/lib/fluxbee/nodes/*"));
        assert!(script.contains("/var/lib/fluxbee/state/nodes/*"));
    }

    #[test]
    fn remove_hive_cleanup_shell_command_runs_via_sudo_bash() {
        let cmd = remove_hive_cleanup_shell_command(remove_hive_cleanup_script());
        assert!(cmd.starts_with("sudo -n bash -lc "));
        assert!(cmd.contains("systemctl stop --no-block"));
        assert!(cmd.contains("/var/lib/fluxbee/state/nodes/*"));
    }

    #[test]
    fn resolve_syncthing_service_user_accepts_existing_user() {
        let mut blob = sample_blob_config();
        blob.sync_service_user = "root".to_string();
        blob.sync_allow_root_fallback = false;
        let effective = resolve_syncthing_service_user(&blob).expect("resolve user");
        assert_eq!(effective, "root");
    }

    #[test]
    fn resolve_syncthing_service_user_rejects_missing_without_fallback() {
        let mut blob = sample_blob_config();
        blob.sync_service_user = format!("missing-{}", Uuid::new_v4());
        blob.sync_allow_root_fallback = false;
        let err = resolve_syncthing_service_user(&blob).expect_err("must fail");
        assert!(err.to_string().contains("allow_root_fallback=false"));
    }

    #[test]
    fn blob_runtime_from_hive_defaults_include_gc() {
        let hive: HiveFile = serde_yaml::from_str("hive_id: sandbox\n").expect("parse hive");
        let blob = blob_runtime_from_hive(&hive);
        assert_eq!(blob.gc_enabled, DEFAULT_BLOB_GC_ENABLED);
        assert_eq!(blob.gc_interval_secs, DEFAULT_BLOB_GC_INTERVAL_SECS);
        assert_eq!(blob.gc_apply, DEFAULT_BLOB_GC_APPLY);
        assert_eq!(blob.gc_staging_ttl_hours, DEFAULT_BLOB_GC_STAGING_TTL_HOURS);
        assert_eq!(
            blob.gc_active_retain_days,
            DEFAULT_BLOB_GC_ACTIVE_RETAIN_DAYS
        );
    }

    #[test]
    fn blob_runtime_from_hive_parses_gc_overrides() {
        let hive_yaml = r#"
hive_id: sandbox
blob:
  gc:
    enabled: true
    interval_secs: 1800
    apply: true
    staging_ttl_hours: 12
    active_retain_days: 45
"#;
        let hive: HiveFile = serde_yaml::from_str(hive_yaml).expect("parse hive");
        let blob = blob_runtime_from_hive(&hive);
        assert!(blob.gc_enabled);
        assert_eq!(blob.gc_interval_secs, 1800);
        assert!(blob.gc_apply);
        assert_eq!(blob.gc_staging_ttl_hours, 12);
        assert_eq!(blob.gc_active_retain_days, 45);
    }

    #[test]
    fn remote_node_projection_reports_status_from_flags() {
        let mut node = RemoteNodeEntry {
            uuid: *Uuid::new_v4().as_bytes(),
            name: [0u8; 256],
            name_len: 0,
            vpn_id: 20,
            hive_index: 0,
            flags: 0,
            _reserved: [0u8; 6],
        };
        node.name_len = write_name(&mut node.name, "WF.echo@worker-220");

        let active = remote_node_to_json(&node, 1000, 0, "worker-220").expect("active node");
        assert_eq!(active["status"], "active");

        let stale = remote_node_to_json(&node, 1000, FLAG_STALE, "worker-220").expect("stale node");
        assert_eq!(stale["status"], "stale");

        let deleted =
            remote_node_to_json(&node, 1000, FLAG_DELETED, "worker-220").expect("deleted node");
        assert_eq!(deleted["status"], "deleted");
    }

    #[test]
    fn reconcile_syncthing_peer_removal_removes_peer_from_top_level_and_folders() {
        let config = sample_syncthing_config_with_peer();
        let blob = sample_blob_config();
        let dist = sample_dist_config();

        let (updated, changed) =
            reconcile_syncthing_peer_removal_xml(&config, &blob, &dist, PEER_DEVICE_ID)
                .expect("peer removal must succeed");

        assert!(changed);
        assert!(!updated.contains(PEER_DEVICE_ID));
        assert!(updated.contains(LOCAL_DEVICE_ID));
        assert!(updated.contains(SYNCTHING_FOLDER_BLOB_ID));
        assert!(updated.contains(SYNCTHING_FOLDER_DIST_ID));
    }

    #[test]
    fn reconcile_syncthing_peer_removal_noop_when_peer_missing() {
        let config = sample_syncthing_config_with_peer().replace(PEER_DEVICE_ID, "");
        let blob = sample_blob_config();
        let dist = sample_dist_config();

        let (updated, changed) =
            reconcile_syncthing_peer_removal_xml(&config, &blob, &dist, PEER_DEVICE_ID)
                .expect("peer removal must succeed");

        assert!(!changed);
        assert_eq!(updated, config);
    }

    #[test]
    fn identity_error_code_and_message_maps_system_rejected() {
        let err = IdentityError::SystemRejected {
            action: MSG_ILK_REGISTER.to_string(),
            error_code: "INVALID_TENANT".to_string(),
            message: "tenant missing".to_string(),
        };
        let (code, message) = identity_error_code_and_message(&err);
        assert_eq!(code, "INVALID_TENANT");
        assert_eq!(message, "tenant missing");
    }

    #[test]
    fn identity_error_code_and_message_maps_action_timeout() {
        let err = IdentityError::ActionTimeout {
            action: MSG_ILK_UPDATE.to_string(),
            trace_id: "trace-123".to_string(),
            target: "SY.identity@motherbee".to_string(),
            timeout_ms: 2500,
        };
        let (code, message) = identity_error_code_and_message(&err);
        assert_eq!(code, "TIMEOUT");
        assert!(message.contains("action=ILK_UPDATE"));
        assert!(message.contains("trace_id=trace-123"));
    }

    #[test]
    fn map_identity_action_result_returns_payload_for_ok() {
        let payload = serde_json::json!({"status": "ok", "ilk_id": "ilk:demo"});
        let out = map_identity_action_result("register", Ok(payload.clone())).expect("ok");
        assert_eq!(out, payload);
    }

    #[test]
    fn map_identity_action_result_formats_register_error() {
        let err = IdentityError::SystemRejected {
            action: MSG_ILK_REGISTER.to_string(),
            error_code: "INVALID_TENANT".to_string(),
            message: "tenant missing".to_string(),
        };
        let out = map_identity_action_result("register", Err(err)).expect_err("must fail");
        let message = out.to_string();
        assert!(message.contains("identity register failed"));
        assert!(message.contains("code=INVALID_TENANT"));
        assert!(message.contains("message=tenant missing"));
    }

    #[test]
    fn map_identity_action_result_formats_update_error() {
        let err = IdentityError::ActionTimeout {
            action: MSG_ILK_UPDATE.to_string(),
            trace_id: "trace-999".to_string(),
            target: "SY.identity@motherbee".to_string(),
            timeout_ms: 5000,
        };
        let out = map_identity_action_result("update", Err(err)).expect_err("must fail");
        let message = out.to_string();
        assert!(message.contains("identity update failed"));
        assert!(message.contains("code=TIMEOUT"));
        assert!(message.contains("action=ILK_UPDATE"));
    }

    #[test]
    fn parse_runtime_manifest_accepts_schema_v1() {
        let path = PathBuf::from("/tmp/runtime-manifest-v1.json");
        let manifest = parse_runtime_manifest_file(
            &path,
            r#"{
                "schema_version": 1,
                "version": 1710000000000,
                "updated_at": "2026-03-16T00:00:00Z",
                "runtimes": {}
            }"#,
        )
        .expect("schema v1 should be accepted");
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.version, 1710000000000);
    }

    #[test]
    fn parse_runtime_manifest_accepts_schema_v2() {
        let path = PathBuf::from("/tmp/runtime-manifest-v2.json");
        let manifest = parse_runtime_manifest_file(
            &path,
            r#"{
                "schema_version": 2,
                "version": 1710000000001,
                "updated_at": "2026-03-16T00:00:01Z",
                "runtimes": {}
            }"#,
        )
        .expect("schema v2 should be accepted");
        assert_eq!(manifest.schema_version, 2);
        assert_eq!(manifest.version, 1710000000001);
    }

    #[test]
    fn parse_runtime_manifest_rejects_unknown_schema_explicitly() {
        let path = PathBuf::from("/tmp/runtime-manifest-v3.json");
        let err = parse_runtime_manifest_file(
            &path,
            r#"{
                "schema_version": 3,
                "version": 1710000000002,
                "updated_at": "2026-03-16T00:00:02Z",
                "runtimes": {}
            }"#,
        )
        .expect_err("unsupported schema must fail");
        let msg = err.to_string();
        assert!(msg.contains("unsupported schema_version=3"));
        assert!(msg.contains("supported=1..=2"));
    }

    #[test]
    fn remove_runtime_version_from_manifest_removes_non_current_version() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test.gov": {
                    "available": ["0.9.0", "1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let updated = remove_runtime_version_from_manifest(&manifest, "ai.test.gov", "0.9.0")
            .expect("remove");
        let entry = updated
            .runtimes
            .get("ai.test.gov")
            .expect("runtime entry must remain");
        assert_eq!(entry["current"], serde_json::json!("1.0.0"));
        assert_eq!(entry["available"], serde_json::json!(["1.0.0"]));
        assert!(updated.version > manifest.version);
    }

    #[test]
    fn remove_runtime_version_from_manifest_rejects_current_version() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test.gov": {
                    "available": ["1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let err = remove_runtime_version_from_manifest(&manifest, "ai.test.gov", "1.0.0")
            .expect_err("current delete must fail");
        assert_eq!(err.to_string(), "RUNTIME_CURRENT_CONFLICT");
    }

    #[test]
    fn apply_runtime_retention_preserves_runtime_referenced_by_persisted_instance() {
        let root = std::env::temp_dir().join(format!("fluxbee-retention-test-{}", Uuid::new_v4()));
        let runtimes_root = root.join("dist").join("runtimes");
        let nodes_root = root.join("nodes");

        let kept_runtime_dir = runtimes_root.join("AI.chat.test").join("1.0.0");
        fs::create_dir_all(&kept_runtime_dir).expect("create kept runtime dir");
        fs::write(kept_runtime_dir.join("marker"), "keep").expect("write kept marker");

        let pruned_runtime_dir = runtimes_root.join("AI.prune.test").join("1.0.0");
        fs::create_dir_all(&pruned_runtime_dir).expect("create pruned runtime dir");
        fs::write(pruned_runtime_dir.join("marker"), "prune").expect("write pruned marker");

        let node_dir = nodes_root.join("AI").join("AI.chat@motherbee");
        fs::create_dir_all(&node_dir).expect("create node dir");
        fs::write(
            node_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "AI.chat.test",
                    "runtime_version": "1.0.0",
                    "relaunch_on_boot": true
                }
            })
            .to_string(),
        )
        .expect("write config");

        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };

        let stats = apply_runtime_retention_with_roots(&manifest, &runtimes_root, &nodes_root)
            .expect("retention should succeed");

        assert!(
            kept_runtime_dir.exists(),
            "persisted runtime should be kept"
        );
        assert!(
            !pruned_runtime_dir.exists(),
            "unreferenced runtime should be pruned"
        );
        assert_eq!(stats.removed_runtime_dirs, 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn runtime_dependents_summary_detects_published_dependents() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.base": {
                    "available": ["1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                },
                "ai.child.config": {
                    "available": ["2.0.0"],
                    "current": "2.0.0",
                    "type": "config_only",
                    "runtime_base": "ai.base"
                },
                "wf.child.flow": {
                    "available": ["3.0.0"],
                    "current": "3.0.0",
                    "type": "workflow",
                    "runtime_base": "ai.base"
                }
            }),
            hash: None,
        };

        let dependents = runtime_dependents_summary(&manifest, "ai.base").expect("dependents");
        assert_eq!(dependents.len(), 2);
        assert_eq!(
            dependents[0]["runtime"],
            serde_json::json!("ai.child.config")
        );
        assert_eq!(dependents[1]["runtime"], serde_json::json!("wf.child.flow"));
    }

    fn sample_orchestrator_state_for_tests() -> OrchestratorState {
        OrchestratorState {
            hive_id: PRIMARY_HIVE_ID.to_string(),
            is_motherbee: true,
            started_at: Instant::now(),
            config_dir: PathBuf::from("/tmp/fluxbee-test-config"),
            state_dir: PathBuf::from("/tmp/fluxbee-test-state"),
            gateway_name: "RT.gateway".to_string(),
            storage_path: Mutex::new("/var/lib/fluxbee/blob".to_string()),
            wan_listen: None,
            wan_authorized_hives: Vec::new(),
            tracked_nodes: Mutex::new(HashSet::new()),
            system_allowed_origins: HashSet::new(),
            runtime_manifest: Mutex::new(None),
            runtime_lifecycle_lock: Mutex::new(()),
            last_runtime_verify: Mutex::new(Instant::now()),
            last_blob_gc: Mutex::new(Instant::now()),
            nats_endpoint: "nats://127.0.0.1:4222".to_string(),
            identity_sync_port: 0,
            blob: sample_blob_config(),
            dist: sample_dist_config(),
            blob_sync_last_desired: Mutex::new(sample_blob_config()),
        }
    }

    #[tokio::test]
    async fn remove_runtime_version_flow_returns_busy_when_lifecycle_lock_is_held() {
        let state = sample_orchestrator_state_for_tests();
        let _guard = state.runtime_lifecycle_lock.lock().await;
        let payload = serde_json::json!({
            "runtime": "wf.busy.test",
            "runtime_version": "1.0.0-test"
        });

        let out = remove_runtime_version_flow(&state, &payload).await;
        assert_eq!(out["status"], "error");
        assert_eq!(out["error_code"], "BUSY");
    }

    #[test]
    fn managed_node_inventory_lists_persisted_instances_without_router_visibility() {
        let state = sample_orchestrator_state_for_tests();
        let root = std::env::temp_dir().join(format!(
            "fluxbee-managed-node-inventory-{}",
            Uuid::new_v4().simple()
        ));
        let node_dir = root.join("AI").join("AI.persisted.test@motherbee");
        std::fs::create_dir_all(&node_dir).expect("create node dir");
        std::fs::write(
            node_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "ai.persisted.test",
                    "runtime_version": "1.0.0-diag",
                    "tenant_id": "tnt:test"
                }
            })
            .to_string(),
        )
        .expect("write config");

        let inventory =
            managed_node_inventory_with_root(&state, &root).expect("managed inventory must load");
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0]["node_name"], "AI.persisted.test@motherbee");
        assert_eq!(inventory[0]["inventory_source"], "managed_instance");
        assert_eq!(inventory[0]["lifecycle_state"], "STOPPED");
        assert_eq!(inventory[0]["runtime"]["name"], "ai.persisted.test");
        assert_eq!(inventory[0]["runtime"]["version"], "1.0.0-diag");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_node_instance_dir_with_root_removes_instance_and_empty_kind_dir() {
        let root = std::env::temp_dir().join(format!(
            "fluxbee-remove-node-instance-{}",
            Uuid::new_v4().simple()
        ));
        let node_dir = root.join("AI").join("AI.remove.test@motherbee");
        std::fs::create_dir_all(&node_dir).expect("create node dir");
        std::fs::write(node_dir.join("config.json"), "{}").expect("write config");

        let (removed_path, removed_kind_dir) =
            remove_node_instance_dir_with_root("AI.remove.test@motherbee", &root)
                .expect("remove instance");
        assert_eq!(removed_path, node_dir);
        assert!(removed_kind_dir);
        assert!(!removed_path.exists());
        assert!(!root.join("AI").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_node_instance_dir_with_root_preserves_non_empty_kind_dir() {
        let root = std::env::temp_dir().join(format!(
            "fluxbee-remove-node-instance-{}",
            Uuid::new_v4().simple()
        ));
        let first_node_dir = root.join("AI").join("AI.remove.first@motherbee");
        let second_node_dir = root.join("AI").join("AI.remove.second@motherbee");
        std::fs::create_dir_all(&first_node_dir).expect("create first node dir");
        std::fs::create_dir_all(&second_node_dir).expect("create second node dir");
        std::fs::write(first_node_dir.join("config.json"), "{}").expect("write config 1");
        std::fs::write(second_node_dir.join("config.json"), "{}").expect("write config 2");

        let (removed_path, removed_kind_dir) =
            remove_node_instance_dir_with_root("AI.remove.first@motherbee", &root)
                .expect("remove instance");
        assert_eq!(removed_path, first_node_dir);
        assert!(!removed_kind_dir);
        assert!(!removed_path.exists());
        assert!(root.join("AI").exists());
        assert!(second_node_dir.exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn persisted_custom_nodes_with_root_skips_core_and_invalid_entries() {
        let root = std::env::temp_dir().join(format!(
            "fluxbee-persisted-custom-nodes-{}",
            Uuid::new_v4().simple()
        ));

        let ai_dir = root.join("AI").join("AI.keep.test@motherbee");
        std::fs::create_dir_all(&ai_dir).expect("create ai dir");
        std::fs::write(
            ai_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "ai.keep.test",
                    "runtime_version": "1.0.0-diag",
                    "relaunch_on_boot": true
                }
            })
            .to_string(),
        )
        .expect("write ai config");

        let sy_dir = root.join("SY").join("SY.admin@motherbee");
        std::fs::create_dir_all(&sy_dir).expect("create sy dir");
        std::fs::write(
            sy_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "sy.admin",
                    "runtime_version": "1.0.0",
                    "relaunch_on_boot": true
                }
            })
            .to_string(),
        )
        .expect("write sy config");

        let rt_gateway_dir = root.join("RT").join("RT.gateway@motherbee");
        std::fs::create_dir_all(&rt_gateway_dir).expect("create rt gateway dir");
        std::fs::write(
            rt_gateway_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "rt.gateway",
                    "runtime_version": "1.0.0",
                    "relaunch_on_boot": true
                }
            })
            .to_string(),
        )
        .expect("write rt gateway config");

        let rt_edge_dir = root.join("RT").join("RT.edge.buffer@motherbee");
        std::fs::create_dir_all(&rt_edge_dir).expect("create rt edge dir");
        std::fs::write(
            rt_edge_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "rt.edge.buffer",
                    "runtime_version": "1.2.3",
                    "relaunch_on_boot": true
                }
            })
            .to_string(),
        )
        .expect("write rt edge config");

        let broken_dir = root.join("WF").join("WF.invalid@motherbee");
        std::fs::create_dir_all(&broken_dir).expect("create wf dir");
        std::fs::write(
            broken_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "wf.invalid",
                    "relaunch_on_boot": true
                }
            })
            .to_string(),
        )
        .expect("write broken config");

        let nodes = persisted_custom_nodes_with_root(&root).expect("load persisted nodes");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].node_name, "AI.keep.test@motherbee");
        assert_eq!(nodes[0].runtime, "ai.keep.test");
        assert_eq!(nodes[0].runtime_version, "1.0.0-diag");
        assert_eq!(nodes[1].node_name, "RT.edge.buffer@motherbee");
        assert_eq!(nodes[1].runtime, "rt.edge.buffer");
        assert_eq!(nodes[1].runtime_version, "1.2.3");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn managed_spawn_disallowed_reason_blocks_sy_and_rt_gateway_only() {
        assert_eq!(
            managed_spawn_disallowed_reason("SY.admin@motherbee"),
            Some("managed spawn does not support SY.* nodes")
        );
        assert_eq!(
            managed_spawn_disallowed_reason("RT.gateway@motherbee"),
            Some("managed spawn does not support RT.gateway; it is core hive infrastructure")
        );
        assert_eq!(
            managed_spawn_disallowed_reason("RT.edge.buffer@motherbee"),
            None
        );
        assert_eq!(
            managed_spawn_disallowed_reason("AI.frontdesk.gov@motherbee"),
            None
        );
    }

    #[test]
    fn persisted_custom_nodes_with_root_requires_relaunch_on_boot_flag() {
        let root = std::env::temp_dir().join(format!(
            "fluxbee-persisted-custom-nodes-{}",
            Uuid::new_v4().simple()
        ));

        let disabled_dir = root.join("AI").join("AI.skip.test@motherbee");
        std::fs::create_dir_all(&disabled_dir).expect("create disabled dir");
        std::fs::write(
            disabled_dir.join("config.json"),
            serde_json::json!({
                "_system": {
                    "runtime": "ai.skip.test",
                    "runtime_version": "1.0.0-diag",
                    "relaunch_on_boot": false
                }
            })
            .to_string(),
        )
        .expect("write disabled config");

        let nodes = persisted_custom_nodes_with_root(&root).expect("load persisted nodes");
        assert!(nodes.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_managed_node_run_command_injects_fluxbee_node_name() {
        let cmd = build_managed_node_run_command(
            "fluxbee-node-AI.demo-motherbee",
            "AI.demo@motherbee",
            "/var/lib/fluxbee/dist/runtimes/ai.demo/1.0.0/bin/start.sh",
        );

        assert!(cmd.contains("--setenv=FLUXBEE_NODE_NAME='AI.demo@motherbee'"));
        assert!(cmd.contains("systemd-run --unit fluxbee-node-AI.demo-motherbee"));
        assert!(cmd.contains("'/var/lib/fluxbee/dist/runtimes/ai.demo/1.0.0/bin/start.sh'"));
    }

    #[test]
    fn runtime_start_script_preflight_missing_returns_runtime_not_present() {
        let runtime = format!("wf.preflight.{}", Uuid::new_v4().simple());
        let version = format!("diag-{}", Uuid::new_v4().simple());
        let start_script = format!(
            "/tmp/fluxbee-preflight-{}/bin/start.sh",
            Uuid::new_v4().simple()
        );
        let _ = std::fs::remove_file(&start_script);

        let out = runtime_start_script_preflight(
            &runtime,
            &version,
            &start_script,
            "worker-220",
            "WF.preflight.test@worker-220",
        )
        .expect_err("missing script must fail");

        assert_eq!(out["status"], "error");
        assert_eq!(out["error_code"], "RUNTIME_NOT_PRESENT");
        assert_eq!(out["runtime"], runtime);
        assert_eq!(out["version"], version);
        assert_eq!(out["expected_path"], start_script);
    }

    #[test]
    fn runtime_readiness_entry_keeps_fr8_fields() {
        let runtime = format!("wf.readiness.{}", Uuid::new_v4().simple());
        let version = format!("diag-{}", Uuid::new_v4().simple());
        let entry = RuntimeManifestEntry {
            available: vec![version.clone()],
            current: Some(version),
            package_type: None,
            runtime_base: None,
            extra: BTreeMap::new(),
        };
        let readiness = runtime_readiness_for_entry(&runtime, &entry, None);
        let item = readiness
            .as_object()
            .and_then(|map| map.values().next())
            .expect("readiness item");

        assert!(item.get("runtime_present").is_some());
        assert!(item.get("start_sh_executable").is_some());
        assert!(item.get("base_runtime_ready").is_some());
    }

    #[test]
    fn runtime_readiness_full_runtime_sets_base_runtime_ready_true() {
        let root = std::env::temp_dir().join(format!("fluxbee-readiness-{}", Uuid::new_v4()));
        let runtime = "ai.runtime.full";
        let version = "1.0.0";
        let start_script = root.join(runtime).join(version).join("bin/start.sh");
        fs::create_dir_all(
            start_script
                .parent()
                .expect("start script parent directory must exist"),
        )
        .expect("create bin dir");
        fs::write(&start_script, "#!/usr/bin/env bash\necho ok\n").expect("write start.sh");
        let mut perms = fs::metadata(&start_script)
            .expect("stat start.sh")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&start_script, perms).expect("chmod start.sh");

        let entry = RuntimeManifestEntry {
            available: vec![version.to_string()],
            current: Some(version.to_string()),
            package_type: Some("full_runtime".to_string()),
            runtime_base: None,
            extra: BTreeMap::new(),
        };
        let readiness = runtime_readiness_for_entry_with_root(runtime, &entry, None, &root);
        let item = readiness
            .as_object()
            .and_then(|map| map.get(version))
            .expect("readiness item");
        assert_eq!(item["runtime_present"], serde_json::json!(true));
        assert_eq!(item["start_sh_executable"], serde_json::json!(true));
        assert_eq!(item["base_runtime_ready"], serde_json::json!(true));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_readiness_config_only_sets_base_runtime_ready_from_base_runtime() {
        let root = std::env::temp_dir().join(format!("fluxbee-readiness-{}", Uuid::new_v4()));
        let runtime = "ai.runtime.config";
        let version = "1.0.0";
        let base_runtime = "ai.generic";
        let base_version = "3.0.0";
        let package_dir = root.join(runtime).join(version);
        let base_start = root
            .join(base_runtime)
            .join(base_version)
            .join("bin/start.sh");

        fs::create_dir_all(&package_dir).expect("create package dir");
        fs::create_dir_all(
            base_start
                .parent()
                .expect("base start script parent directory must exist"),
        )
        .expect("create base bin dir");
        fs::write(&base_start, "#!/usr/bin/env bash\necho ok\n").expect("write base start");
        let mut perms = fs::metadata(&base_start)
            .expect("stat base start")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&base_start, perms).expect("chmod base start");

        let entry = RuntimeManifestEntry {
            available: vec![version.to_string()],
            current: Some(version.to_string()),
            package_type: Some("config_only".to_string()),
            runtime_base: Some(base_runtime.to_string()),
            extra: BTreeMap::new(),
        };
        let runtime_entries = serde_json::json!({
            runtime: {
                "available": [version],
                "current": version,
                "type": "config_only",
                "runtime_base": base_runtime
            },
            base_runtime: {
                "available": [base_version],
                "current": base_version,
                "type": "full_runtime"
            }
        });
        let readiness = runtime_readiness_for_entry_with_root(
            runtime,
            &entry,
            runtime_entries.as_object(),
            &root,
        );
        let item = readiness
            .as_object()
            .and_then(|map| map.get(version))
            .expect("readiness item");
        assert_eq!(item["runtime_present"], serde_json::json!(true));
        assert_eq!(item["start_sh_executable"], serde_json::json!(true));
        assert_eq!(item["base_runtime_ready"], serde_json::json!(true));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_manifest_entry_from_value_preserves_extra_fields() {
        let raw = serde_json::json!({
            "available": ["1.0.0"],
            "current": "1.0.0",
            "type": "config_only",
            "runtime_base": "ai.generic",
            "custom_policy": {
                "foo": "bar"
            }
        });

        let entry =
            runtime_manifest_entry_from_value("ai.test.config", &raw).expect("typed entry parse");

        assert_eq!(entry.package_type.as_deref(), Some("config_only"));
        assert_eq!(entry.runtime_base.as_deref(), Some("ai.generic"));
        assert_eq!(
            entry.extra.get("custom_policy"),
            Some(&serde_json::json!({"foo":"bar"}))
        );
    }

    #[test]
    fn legacy_manifest_entry_without_type_uses_full_runtime_path_contract() {
        let runtime = "ai.legacy.sample";
        let current = "1.2.3";
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000001111,
            updated_at: Some("2026-03-16T00:10:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [current],
                    "current": current
                }
            }),
            hash: None,
        };

        let runtime_key = resolve_runtime_key(&manifest, runtime).expect("runtime key");
        let resolved_version =
            resolve_runtime_version(&manifest, &runtime_key, "current").expect("version");
        let start_script = runtime_start_script(&runtime_key, &resolved_version);

        assert_eq!(
            start_script,
            format!(
                "/var/lib/fluxbee/dist/runtimes/{}/{}/bin/start.sh",
                runtime, current
            )
        );
    }

    #[test]
    fn full_runtime_type_keeps_same_start_script_contract() {
        let runtime = "wf.legacy.full";
        let current = "0.0.7";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000002222,
            updated_at: Some("2026-03-16T00:20:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [current],
                    "current": current,
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let runtime_key = resolve_runtime_key(&manifest, runtime).expect("runtime key");
        let resolved_version =
            resolve_runtime_version(&manifest, &runtime_key, "current").expect("version");
        let start_script = runtime_start_script(&runtime_key, &resolved_version);

        assert_eq!(
            start_script,
            format!(
                "/var/lib/fluxbee/dist/runtimes/{}/{}/bin/start.sh",
                runtime, current
            )
        );
    }

    #[test]
    fn resolve_runtime_spawn_entrypoint_full_runtime_uses_own_start_script() {
        let runtime = "ai.agent.full";
        let version = "1.3.0";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000003333,
            updated_at: Some("2026-03-16T00:30:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [version],
                    "current": version,
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let entrypoint =
            resolve_runtime_spawn_entrypoint(&manifest, runtime, version).expect("entrypoint");
        assert_eq!(entrypoint.script_runtime, runtime);
        assert_eq!(entrypoint.script_version, version);
        assert_eq!(
            entrypoint.script_path,
            format!(
                "/var/lib/fluxbee/dist/runtimes/{}/{}/bin/start.sh",
                runtime, version
            )
        );
    }

    #[test]
    fn resolve_runtime_spawn_entrypoint_config_only_uses_base_runtime_start_script() {
        let runtime = "ai.agent.billing";
        let runtime_version = "2.1.0";
        let base_runtime = "ai.generic";
        let base_version = "5.0.0";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000004444,
            updated_at: Some("2026-03-16T00:40:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "config_only",
                    "runtime_base": base_runtime
                },
                base_runtime: {
                    "available": [base_version],
                    "current": base_version,
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let entrypoint = resolve_runtime_spawn_entrypoint(&manifest, runtime, runtime_version)
            .expect("entrypoint");
        assert_eq!(entrypoint.script_runtime, base_runtime);
        assert_eq!(entrypoint.script_version, base_version);
        assert_eq!(
            entrypoint.script_path,
            format!(
                "/var/lib/fluxbee/dist/runtimes/{}/{}/bin/start.sh",
                base_runtime, base_version
            )
        );
    }

    #[test]
    fn resolve_runtime_spawn_entrypoint_workflow_uses_base_runtime_start_script() {
        let runtime = "wf.onboarding.standard";
        let runtime_version = "1.0.0";
        let base_runtime = "wf.engine";
        let base_version = "9.9.1";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000005555,
            updated_at: Some("2026-03-16T00:50:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "workflow",
                    "runtime_base": base_runtime
                },
                base_runtime: {
                    "available": [base_version],
                    "current": base_version,
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let entrypoint = resolve_runtime_spawn_entrypoint(&manifest, runtime, runtime_version)
            .expect("entrypoint");
        assert_eq!(entrypoint.script_runtime, base_runtime);
        assert_eq!(entrypoint.script_version, base_version);
        assert_eq!(
            entrypoint.script_path,
            format!(
                "/var/lib/fluxbee/dist/runtimes/{}/{}/bin/start.sh",
                base_runtime, base_version
            )
        );
    }

    #[test]
    fn resolve_runtime_spawn_entrypoint_missing_runtime_base_returns_specific_error() {
        let runtime = "ai.agent.config";
        let runtime_version = "1.0.0";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000006666,
            updated_at: Some("2026-03-16T01:00:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "config_only"
                }
            }),
            hash: None,
        };

        let err = resolve_runtime_spawn_entrypoint(&manifest, runtime, runtime_version)
            .expect_err("missing runtime_base must fail");
        assert_eq!(err.error_code(), "MISSING_RUNTIME_BASE");
    }

    #[test]
    fn resolve_runtime_spawn_entrypoint_unknown_base_returns_specific_error() {
        let runtime = "ai.agent.config";
        let runtime_version = "1.0.0";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000007777,
            updated_at: Some("2026-03-16T01:10:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "config_only",
                    "runtime_base": "ai.missing"
                }
            }),
            hash: None,
        };

        let err = resolve_runtime_spawn_entrypoint(&manifest, runtime, runtime_version)
            .expect_err("unknown runtime_base must fail");
        assert_eq!(err.error_code(), "BASE_RUNTIME_NOT_AVAILABLE");
    }

    #[test]
    fn resolve_runtime_spawn_entrypoint_unknown_package_type_returns_specific_error() {
        let runtime = "ai.agent.weird";
        let runtime_version = "1.0.0";
        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000008888,
            updated_at: Some("2026-03-16T01:20:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "strange_mode"
                }
            }),
            hash: None,
        };

        let err = resolve_runtime_spawn_entrypoint(&manifest, runtime, runtime_version)
            .expect_err("unknown package type must fail");
        assert_eq!(err.error_code(), "UNKNOWN_PACKAGE_TYPE");
    }

    #[test]
    fn runtime_base_start_script_preflight_missing_returns_base_runtime_not_present() {
        let runtime = format!("wf.preflight.base.{}", Uuid::new_v4().simple());
        let version = format!("diag-{}", Uuid::new_v4().simple());
        let base_runtime = "wf.engine";
        let base_version = "9.0.0";
        let start_script = format!(
            "/tmp/fluxbee-preflight-base-{}/bin/start.sh",
            Uuid::new_v4().simple()
        );
        let _ = std::fs::remove_file(&start_script);

        let out = runtime_base_start_script_preflight(
            &runtime,
            &version,
            base_runtime,
            base_version,
            &start_script,
            "worker-220",
            "WF.preflight.base.test@worker-220",
        )
        .expect_err("missing base runtime script must fail");

        assert_eq!(out["status"], "error");
        assert_eq!(out["error_code"], "BASE_RUNTIME_NOT_PRESENT");
        assert_eq!(out["runtime"], runtime);
        assert_eq!(out["runtime_base"], base_runtime);
        assert_eq!(out["base_version"], base_version);
        assert_eq!(out["expected_path"], start_script);
    }

    #[test]
    fn verify_runtime_current_artifacts_with_root_config_only_requires_runtime_base_start_script() {
        let root = std::env::temp_dir().join(format!("fluxbee-verify-{}", Uuid::new_v4()));
        let runtime = "ai.agent.config";
        let runtime_version = "2.0.0";
        let base_runtime = "ai.generic";
        let base_version = "5.1.0";

        fs::create_dir_all(root.join(runtime).join(runtime_version)).expect("runtime package dir");

        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000011111,
            updated_at: Some("2026-03-16T01:40:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "config_only",
                    "runtime_base": base_runtime
                },
                base_runtime: {
                    "available": [base_version],
                    "current": base_version,
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let errors = verify_runtime_current_artifacts_with_root(&manifest, &root).expect("verify");
        assert!(
            errors
                .iter()
                .any(|item| item.contains("missing base start.sh")
                    && item.contains("runtime_base='ai.generic'")),
            "expected missing base start.sh error in {:?}",
            errors
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verify_runtime_current_artifacts_with_root_workflow_ok_when_base_runtime_ready() {
        let root = std::env::temp_dir().join(format!("fluxbee-verify-{}", Uuid::new_v4()));
        let runtime = "wf.onboarding.standard";
        let runtime_version = "1.0.0";
        let base_runtime = "wf.engine";
        let base_version = "9.2.0";
        let base_start = root
            .join(base_runtime)
            .join(base_version)
            .join("bin/start.sh");

        fs::create_dir_all(root.join(runtime).join(runtime_version)).expect("runtime package dir");
        fs::create_dir_all(
            base_start
                .parent()
                .expect("base start script parent directory must exist"),
        )
        .expect("base bin dir");
        fs::write(&base_start, "#!/usr/bin/env bash\necho ok\n").expect("write base start.sh");
        let mut perms = fs::metadata(&base_start)
            .expect("stat base start.sh")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&base_start, perms).expect("chmod base start.sh");

        let manifest = RuntimeManifest {
            schema_version: 2,
            version: 1710000012222,
            updated_at: Some("2026-03-16T01:50:00Z".to_string()),
            runtimes: serde_json::json!({
                runtime: {
                    "available": [runtime_version],
                    "current": runtime_version,
                    "type": "workflow",
                    "runtime_base": base_runtime
                },
                base_runtime: {
                    "available": [base_version],
                    "current": base_version,
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let errors = verify_runtime_current_artifacts_with_root(&manifest, &root).expect("verify");
        assert!(errors.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_runtime_config_template_with_root_uses_default_template_path() {
        let root = std::env::temp_dir().join(format!("fluxbee-template-{}", Uuid::new_v4()));
        let runtime = "ai.template.default";
        let version = "1.0.0";
        let template_path = root
            .join(runtime)
            .join(version)
            .join("config/default-config.json");
        fs::create_dir_all(
            template_path
                .parent()
                .expect("template parent directory must exist"),
        )
        .expect("create template dir");
        fs::write(
            &template_path,
            r#"{"model":"gpt-5","temperature":0.2,"tenant_id":"tnt:from-template"}"#,
        )
        .expect("write template");

        let template = load_runtime_config_template_with_root(runtime, version, &root)
            .expect("load template")
            .expect("template should exist");
        assert_eq!(template.get("model"), Some(&serde_json::json!("gpt-5")));
        assert_eq!(template.get("temperature"), Some(&serde_json::json!(0.2)));
        assert_eq!(
            template.get("tenant_id"),
            Some(&serde_json::json!("tnt:from-template"))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_runtime_config_template_with_root_uses_package_json_config_template_field() {
        let root = std::env::temp_dir().join(format!("fluxbee-template-{}", Uuid::new_v4()));
        let runtime = "ai.template.custom";
        let version = "2.0.0";
        let package_dir = root.join(runtime).join(version);
        fs::create_dir_all(package_dir.join("templates")).expect("create package dir");
        fs::write(
            package_dir.join("package.json"),
            r#"{"config_template":"templates/runtime.json"}"#,
        )
        .expect("write package.json");
        fs::write(
            package_dir.join("templates/runtime.json"),
            r#"{"model":"gpt-5-mini","region":"ar"}"#,
        )
        .expect("write template");

        let template = load_runtime_config_template_with_root(runtime, version, &root)
            .expect("load template")
            .expect("template should exist");
        assert_eq!(
            template.get("model"),
            Some(&serde_json::json!("gpt-5-mini"))
        );
        assert_eq!(template.get("region"), Some(&serde_json::json!("ar")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_runtime_config_template_with_root_rejects_parent_traversal() {
        let root = std::env::temp_dir().join(format!("fluxbee-template-{}", Uuid::new_v4()));
        let runtime = "ai.template.invalid";
        let version = "3.0.0";
        let package_dir = root.join(runtime).join(version);
        fs::create_dir_all(&package_dir).expect("create package dir");
        fs::write(
            package_dir.join("package.json"),
            r#"{"config_template":"../secret.json"}"#,
        )
        .expect("write package.json");

        let err = load_runtime_config_template_with_root(runtime, version, &root)
            .expect_err("parent traversal must fail");
        let msg = err.to_string();
        assert!(msg.contains("must not contain parent traversal"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn assemble_spawn_config_map_applies_template_then_request_then_forced_system() {
        let template = serde_json::json!({
            "model": "gpt-5",
            "temperature": 0.2,
            "tenant_id": "tnt:template",
            "_system": {"managed_by": "template"}
        })
        .as_object()
        .expect("template object")
        .clone();
        let request_patch = serde_json::json!({
            "temperature": 0.9,
            "max_tokens": 4096,
            "tenant_id": "tnt:request"
        })
        .as_object()
        .expect("request object")
        .clone();
        let forced_system = serde_json::json!({
            "managed_by": "SY.orchestrator",
            "config_version": 1
        });

        let out = assemble_spawn_config_map(
            Some(template),
            request_patch,
            Some("tnt:resolved".to_string()),
            Some(forced_system.clone()),
        );

        assert_eq!(out.get("model"), Some(&serde_json::json!("gpt-5")));
        assert_eq!(out.get("temperature"), Some(&serde_json::json!(0.9)));
        assert_eq!(out.get("max_tokens"), Some(&serde_json::json!(4096)));
        assert_eq!(
            out.get("tenant_id"),
            Some(&serde_json::json!("tnt:request"))
        );
        assert_eq!(out.get("_system"), Some(&forced_system));
    }

    #[test]
    fn assemble_spawn_config_map_uses_resolved_tenant_only_when_missing() {
        let request_patch = serde_json::json!({
            "model": "gpt-5-mini"
        })
        .as_object()
        .expect("request object")
        .clone();

        let out =
            assemble_spawn_config_map(None, request_patch, Some("tnt:resolved".to_string()), None);

        assert_eq!(out.get("model"), Some(&serde_json::json!("gpt-5-mini")));
        assert_eq!(
            out.get("tenant_id"),
            Some(&serde_json::json!("tnt:resolved"))
        );
        assert!(out.get("_system").is_none());
    }

    #[test]
    fn build_node_system_block_includes_runtime_base_and_package_path_when_provided() {
        let system = build_node_system_block(
            "AI.billing@worker-220",
            "worker-220",
            Some("ai.billing"),
            Some("2.1.0"),
            Some("ai.generic"),
            Some("/var/lib/fluxbee/dist/runtimes/ai.billing/2.1.0"),
            Some("ilk:11111111-1111-1111-1111-111111111111"),
            Some("tnt:22222222-2222-2222-2222-222222222222"),
            1,
        );
        assert_eq!(system["runtime_base"], serde_json::json!("ai.generic"));
        assert_eq!(
            system["package_path"],
            serde_json::json!("/var/lib/fluxbee/dist/runtimes/ai.billing/2.1.0")
        );
    }

    #[test]
    fn build_node_system_block_preserves_existing_identity_and_system_fields() {
        let system = build_node_system_block(
            "WF.demo.l1@worker-220",
            "worker-220",
            Some("wf.orch.diag"),
            Some("current"),
            None,
            None,
            Some("ilk:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
            Some("tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"),
            7,
        );
        let obj = system.as_object().expect("system block must be object");

        assert_eq!(
            obj.get("managed_by"),
            Some(&serde_json::json!("SY.orchestrator"))
        );
        assert_eq!(
            obj.get("node_name"),
            Some(&serde_json::json!("WF.demo.l1@worker-220"))
        );
        assert_eq!(obj.get("hive_id"), Some(&serde_json::json!("worker-220")));
        assert_eq!(obj.get("relaunch_on_boot"), Some(&serde_json::json!(true)));
        assert_eq!(obj.get("runtime"), Some(&serde_json::json!("wf.orch.diag")));
        assert_eq!(
            obj.get("runtime_version"),
            Some(&serde_json::json!("current"))
        );
        assert_eq!(
            obj.get("ilk_id"),
            Some(&serde_json::json!(
                "ilk:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
            ))
        );
        assert_eq!(
            obj.get("tenant_id"),
            Some(&serde_json::json!(
                "tnt:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
            ))
        );
        assert_eq!(obj.get("config_version"), Some(&serde_json::json!(7)));
        assert!(obj.get("created_at_ms").and_then(|v| v.as_u64()).is_some());
        assert!(obj.get("updated_at_ms").and_then(|v| v.as_u64()).is_some());
    }

    #[test]
    fn build_node_system_block_omits_runtime_base_and_package_path_when_absent() {
        let system = build_node_system_block(
            "AI.core@worker-220",
            "worker-220",
            Some("ai.core"),
            Some("1.0.0"),
            None,
            None,
            None,
            None,
            1,
        );
        let system_obj = system.as_object().expect("system must be object");
        assert!(!system_obj.contains_key("runtime_base"));
        assert!(!system_obj.contains_key("package_path"));
    }
}
