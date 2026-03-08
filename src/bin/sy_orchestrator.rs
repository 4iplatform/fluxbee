use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Mutex;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::nats::{request_local, NatsRequestEnvelope, NatsResponseEnvelope};
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};
use json_router::shm::{
    now_epoch_ms, LsaRegionReader, LsaSnapshot, NodeEntry, RemoteHiveEntry, RemoteNodeEntry,
    RouterRegionReader, ShmSnapshot, FLAG_DELETED, FLAG_STALE, HEARTBEAT_STALE_MS,
};

type OrchestratorError = Box<dyn std::error::Error + Send + Sync>;

const BOOTSTRAP_SSH_USER: &str = "administrator";
const BOOTSTRAP_SSH_PASS: &str = "magicAI";
const MOTHERBEE_SSH_KEY_PATH: &str = "/var/lib/fluxbee/ssh/motherbee.key";
const ORCH_SUDOERS_PATH: &str = "/etc/sudoers.d/fluxbee-orchestrator";
const ORCH_SSH_GATE_PATH: &str = "/usr/local/bin/fluxbee-ssh-gate.sh";
const RUNTIME_VERIFY_INTERVAL_SECS: u64 = 300;
const RUNTIME_MANIFEST_FILE: &str = "runtime-manifest.json";
const RUNTIME_MANIFEST_SCHEMA_VERSION: u64 = 1;
const NATS_BOOTSTRAP_TIMEOUT_SECS: u64 = 20;
const STORAGE_BOOTSTRAP_TIMEOUT_SECS: u64 = 30;
const STORAGE_DB_READINESS_TIMEOUT_SECS: u64 = 30;
const STORAGE_DB_READINESS_REQUEST_TIMEOUT_SECS: u64 = 3;
const SUBJECT_STORAGE_METRICS_GET: &str = "storage.metrics.get";
const SY_NODES_BOOTSTRAP_TIMEOUT_SECS: u64 = 60;
const ADD_HIVE_FINALIZE_SOCKET_TIMEOUT_SECS: u64 = 120;
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
const SYNCTHING_INSTALL_PATH: &str = "/var/lib/fluxbee/vendor/bin/syncthing";
const VENDOR_ROOT_DIR: &str = "/var/lib/fluxbee/vendor";
const DIST_ROOT_DIR: &str = "/var/lib/fluxbee/dist";
const DIST_RUNTIME_ROOT_DIR: &str = "/var/lib/fluxbee/dist/runtimes";
const DIST_RUNTIME_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/runtimes/manifest.json";
const DIST_CORE_BIN_SOURCE_DIR: &str = "/var/lib/fluxbee/dist/core/bin";
const DIST_CORE_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/core/manifest.json";
const DIST_VENDOR_ROOT_DIR: &str = "/var/lib/fluxbee/dist/vendor";
const DIST_VENDOR_MANIFEST_PATH: &str = "/var/lib/fluxbee/dist/vendor/manifest.json";
const DIST_SYNCTHING_VENDOR_SOURCE_PATH: &str = "/var/lib/fluxbee/dist/vendor/syncthing/syncthing";
const CORE_SERVICE_HEALTH_TIMEOUT_SECS: u64 = 30;
const DEPLOYMENT_HISTORY_MAX_LIMIT: usize = 500;
const DRIFT_ALERT_MAX_LIMIT: usize = 500;
const CORE_SYNC_RESTART_ORDER: &[&str] = &[
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-identity",
    "sy-admin",
    "sy-storage",
    "sy-orchestrator",
];
const WORKER_MIN_CORE_COMPONENTS: [&str; 4] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-orchestrator",
];
const WORKER_BOOTSTRAP_CORE_COMPONENTS: [&str; 2] = ["rt-gateway", "sy-orchestrator"];
const DEFAULT_BLOB_ENABLED: bool = true;
const DEFAULT_BLOB_PATH: &str = "/var/lib/fluxbee/blob";
const DEFAULT_BLOB_SYNC_ENABLED: bool = false;
const DEFAULT_BLOB_SYNC_TOOL: &str = "syncthing";
const DEFAULT_BLOB_SYNC_API_PORT: u16 = 8384;
const DEFAULT_BLOB_SYNC_DATA_DIR: &str = "/var/lib/fluxbee/syncthing";
const DEFAULT_DIST_PATH: &str = DIST_ROOT_DIR;
const DEFAULT_DIST_SYNC_ENABLED: bool = true;
const DEFAULT_DIST_SYNC_TOOL: &str = "syncthing";

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    nats: Option<NatsSection>,
    storage: Option<StorageSection>,
    blob: Option<BlobSection>,
    dist: Option<DistSection>,
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
struct BlobSection {
    enabled: Option<bool>,
    path: Option<String>,
    sync: Option<BlobSyncSection>,
}

#[derive(Debug, Deserialize)]
struct BlobSyncSection {
    enabled: Option<bool>,
    tool: Option<String>,
    api_port: Option<u16>,
    data_dir: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
struct RuntimeManifest {
    #[serde(default = "default_runtime_manifest_schema_version")]
    schema_version: u64,
    #[serde(default)]
    version: u64,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    runtimes: serde_json::Value,
    #[serde(default)]
    hash: Option<String>,
}

fn default_runtime_manifest_schema_version() -> u64 {
    RUNTIME_MANIFEST_SCHEMA_VERSION
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
    last_runtime_verify: Mutex<Instant>,
    nats_endpoint: String,
    blob: BlobRuntimeConfig,
    dist: DistRuntimeConfig,
    blob_sync_last_desired: Mutex<BlobRuntimeConfig>,
}

const MOTHERBEE_CRITICAL_SERVICES: [&str; 5] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-admin",
    "sy-storage",
];
const WORKER_CRITICAL_SERVICES: [&str; 3] = ["rt-gateway", "sy-config-routes", "sy-opa-rules"];

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
    if !is_motherbee && !is_worker_role(hive.role.as_deref()) {
        tracing::warn!(
            role = ?hive.role,
            "SY.orchestrator supports only role=motherbee|worker; exiting"
        );
        return Ok(());
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
        last_runtime_verify: Mutex::new(Instant::now()),
        nats_endpoint,
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
        dist_path = %state.dist.path.display(),
        dist_sync_enabled = state.dist.sync_enabled,
        dist_sync_tool = %state.dist.sync_tool,
        "blob/dist runtime config loaded"
    );
    ensure_dirs(&config_dir, &state_dir, &run_dir, &state.blob, &state.dist)?;
    write_pid(&run_dir)?;

    bootstrap_local(&state, &socket_dir).await?;

    let node_config = NodeConfig {
        name: "SY.orchestrator".to_string(),
        router_socket: socket_dir.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
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
        vec!["sy-config-routes", "sy-opa-rules", "sy-admin", "sy-storage"]
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
        if let Err(err) = runtime_verify_and_sync(state).await {
            tracing::warn!(error = %err, "runtime verify/sync failed");
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

    for service in ["sy-storage", "sy-admin", "sy-config-routes", "sy-opa-rules"] {
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
        "list_nodes" => list_nodes_flow(state, &msg.payload),
        "list_routers" => list_routers_flow(state, &msg.payload),
        "list_versions" => list_versions_flow(state).await,
        "get_versions" => get_versions_flow(state, &msg.payload).await,
        "list_deployments" => list_deployments_flow(&msg.payload),
        "get_deployments" => get_deployments_flow(state, &msg.payload),
        "list_drift_alerts" => list_drift_alerts_flow(&msg.payload),
        "get_drift_alerts" => get_drift_alerts_flow(state, &msg.payload),
        "run_node" => run_node_flow(state, &msg.payload).await,
        "kill_node" => kill_node_flow(state, &msg.payload).await,
        "list_hives" => {
            serde_json::json!({
                "status": "ok",
                "hives": list_hives(&state.state_dir)?,
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
            | "SPAWN_NODE"
            | "KILL_NODE"
            | "GET_VERSIONS"
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
                "SPAWN_NODE" => {
                    let _ =
                        send_system_action_response(sender, msg, "SPAWN_NODE_RESPONSE", payload)
                            .await;
                }
                "KILL_NODE" => {
                    let _ = send_system_action_response(sender, msg, "KILL_NODE_RESPONSE", payload)
                        .await;
                }
                "GET_VERSIONS" => {
                    let _ =
                        send_system_action_response(sender, msg, "GET_VERSIONS_RESPONSE", payload)
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
        Some("SYSTEM_UPDATE") => {
            let result = handle_system_update_message(state, msg).await;
            let _ =
                send_system_action_response(sender, msg, "SYSTEM_UPDATE_RESPONSE", result).await;
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
        Some("GET_VERSIONS") => {
            let result = get_versions_flow(state, &msg.payload).await;
            let _ = send_system_action_response(sender, msg, "GET_VERSIONS_RESPONSE", result).await;
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
            version: load_runtime_manifest().map(|manifest| manifest.version),
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
    match category {
        "runtime" => {
            let manifest = load_runtime_manifest().ok_or("runtime manifest missing locally")?;
            apply_runtime_retention(&manifest)?;
            {
                let mut guard = state.runtime_manifest.lock().await;
                *guard = Some(manifest);
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
            let backup_dir = PathBuf::from("/var/lib/fluxbee/core/bin.prev.local")
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
        }
    }

    BlobRuntimeConfig {
        enabled,
        path,
        sync_enabled,
        sync_tool,
        sync_api_port,
        sync_data_dir,
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
        fs::create_dir_all(&blob.path)?;
    }
    if blob.sync_enabled {
        fs::create_dir_all(&blob.sync_data_dir)?;
    }
    fs::create_dir_all(&dist.path)?;
    fs::create_dir_all(dist.path.join("runtimes"))?;
    fs::create_dir_all(dist.path.join("core").join("bin"))?;
    fs::create_dir_all(dist.path.join("vendor"))?;
    fs::create_dir_all(runtimes_root())?;
    fs::create_dir_all(Path::new(DIST_CORE_BIN_SOURCE_DIR))?;
    fs::create_dir_all(Path::new(DIST_VENDOR_ROOT_DIR))?;
    fs::create_dir_all(Path::new(VENDOR_ROOT_DIR))?;
    fs::create_dir_all(orchestrator_runtime_dir())?;
    fs::create_dir_all(run_dir)?;
    Ok(())
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

fn syncthing_binary_available() -> bool {
    Path::new(SYNCTHING_INSTALL_PATH).exists()
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

fn ensure_syncthing_firewall_local() {
    let mut applied = false;
    if command_exists("ufw") {
        applied = true;
        for rule in [
            format!("{SYNCTHING_SYNC_PORT_TCP}/tcp"),
            format!("{SYNCTHING_SYNC_PORT_UDP}/udp"),
            format!("{SYNCTHING_DISCOVERY_PORT_UDP}/udp"),
        ] {
            let mut cmd = Command::new("ufw");
            cmd.arg("allow").arg(&rule);
            if let Err(err) = run_cmd(cmd, "ufw allow syncthing") {
                tracing::warn!(rule = %rule, error = %err, "ufw allow failed");
            }
        }
    }

    if command_exists("firewall-cmd") {
        let mut state_cmd = Command::new("firewall-cmd");
        state_cmd.arg("--state");
        if run_cmd(state_cmd, "firewall-cmd --state").is_ok() {
            applied = true;
            for rule in [
                format!("{SYNCTHING_SYNC_PORT_TCP}/tcp"),
                format!("{SYNCTHING_SYNC_PORT_UDP}/udp"),
                format!("{SYNCTHING_DISCOVERY_PORT_UDP}/udp"),
            ] {
                let mut cmd_now = Command::new("firewall-cmd");
                cmd_now.arg("--add-port").arg(&rule);
                if let Err(err) = run_cmd(cmd_now, "firewall-cmd --add-port") {
                    tracing::warn!(rule = %rule, error = %err, "firewalld runtime port rule failed");
                }

                let mut cmd_persistent = Command::new("firewall-cmd");
                cmd_persistent
                    .arg("--permanent")
                    .arg("--add-port")
                    .arg(&rule);
                if let Err(err) = run_cmd(cmd_persistent, "firewall-cmd --permanent --add-port") {
                    tracing::warn!(rule = %rule, error = %err, "firewalld permanent port rule failed");
                }
            }
        }
    }

    if !applied {
        tracing::warn!(
            "no ufw/firewalld detected; syncthing ports must be opened by host firewall policy"
        );
    }
}

fn disable_syncthing_firewall_local() {
    let mut applied = false;
    if command_exists("ufw") {
        applied = true;
        for rule in [
            format!("{SYNCTHING_SYNC_PORT_TCP}/tcp"),
            format!("{SYNCTHING_SYNC_PORT_UDP}/udp"),
            format!("{SYNCTHING_DISCOVERY_PORT_UDP}/udp"),
        ] {
            let mut cmd = Command::new("ufw");
            cmd.arg("--force").arg("delete").arg("allow").arg(&rule);
            if let Err(err) = run_cmd(cmd, "ufw delete allow syncthing") {
                tracing::warn!(rule = %rule, error = %err, "ufw delete allow failed");
            }
        }
    }

    if command_exists("firewall-cmd") {
        let mut state_cmd = Command::new("firewall-cmd");
        state_cmd.arg("--state");
        if run_cmd(state_cmd, "firewall-cmd --state").is_ok() {
            applied = true;
            for rule in [
                format!("{SYNCTHING_SYNC_PORT_TCP}/tcp"),
                format!("{SYNCTHING_SYNC_PORT_UDP}/udp"),
                format!("{SYNCTHING_DISCOVERY_PORT_UDP}/udp"),
            ] {
                let mut cmd_now = Command::new("firewall-cmd");
                cmd_now.arg("--remove-port").arg(&rule);
                if let Err(err) = run_cmd(cmd_now, "firewall-cmd --remove-port") {
                    tracing::warn!(
                        rule = %rule,
                        error = %err,
                        "firewalld runtime port remove failed"
                    );
                }

                let mut cmd_persistent = Command::new("firewall-cmd");
                cmd_persistent
                    .arg("--permanent")
                    .arg("--remove-port")
                    .arg(&rule);
                if let Err(err) = run_cmd(cmd_persistent, "firewall-cmd --permanent --remove-port")
                {
                    tracing::warn!(
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
            "no ufw/firewalld detected; syncthing ports must be removed by host firewall policy"
        );
    }
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
    let service_group = if linux_user_exists(service_user) {
        service_user
    } else {
        "root"
    };
    format!(
        "[Unit]\nDescription=Fluxbee Syncthing (blob sync)\nAfter=network.target\n\n[Service]\nType=simple\nUser={}\nGroup={}\nWorkingDirectory={}\nEnvironment=HOME={}\nExecStart={} --no-browser --no-restart --home={} --gui-address=127.0.0.1:{}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
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
    let service_user = if linux_user_exists(SYNCTHING_INSTALL_USER) {
        SYNCTHING_INSTALL_USER.to_string()
    } else {
        tracing::warn!(
            user = SYNCTHING_INSTALL_USER,
            "linux user not found; running syncthing as root"
        );
        "root".to_string()
    };
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
    let mut cmd = Command::new(SYNCTHING_INSTALL_PATH);
    cmd.arg("--home")
        .arg(&sync.sync_data_dir)
        .arg("--device-id");
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
        let (next, folder_changed) = ensure_syncthing_folder_in_config_xml(
            &updated,
            SYNCTHING_FOLDER_BLOB_ID,
            &blob.path.display().to_string(),
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

fn reconcile_syncthing_folders_xml(
    config_xml: &str,
    blob: &BlobRuntimeConfig,
    dist: &DistRuntimeConfig,
) -> Result<(String, Vec<String>), OrchestratorError> {
    let mut updated = config_xml.to_string();
    let mut changed_folders = Vec::new();

    if blob.sync_enabled && blob_sync_tool_is_syncthing(blob) {
        let (next, changed) = ensure_syncthing_folder_in_config_xml(
            &updated,
            SYNCTHING_FOLDER_BLOB_ID,
            &blob.path.display().to_string(),
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
    if !changed_folders.is_empty() {
        tracing::info!(
            service = SYNCTHING_SERVICE_NAME,
            folders = ?changed_folders,
            "syncthing folder config reconciled locally; restarting service"
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
    if service_active && api_healthy {
        return Ok(());
    }

    tracing::warn!(
        service = SYNCTHING_SERVICE_NAME,
        service_active = service_active,
        api_healthy = api_healthy,
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

fn routers_from_snapshot(snapshot: &ShmSnapshot) -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "uuid": snapshot.header.router_uuid.to_string(),
        "name": snapshot.header.router_name,
        "is_gateway": snapshot.header.is_gateway,
        "nodes_count": snapshot.header.node_count,
        "heartbeat": snapshot.header.heartbeat,
        "status": "alive",
    })]
}

fn nodes_from_snapshot(snapshot: &ShmSnapshot, local_hive: &str) -> Vec<serde_json::Value> {
    snapshot
        .nodes
        .iter()
        .filter_map(|node| node_entry_to_json(node, local_hive))
        .collect()
}

fn list_nodes_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        return match load_router_snapshot(state) {
            Ok(snapshot) => serde_json::json!({
                "status": "ok",
                "nodes": nodes_from_snapshot(&snapshot, &state.hive_id),
            }),
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "SHM_NOT_FOUND",
                "message": err.to_string(),
            }),
        };
    }

    match load_lsa_snapshot(state) {
        Ok(snapshot) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "nodes": remote_nodes_for_hive(&snapshot, &target_hive),
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SHM_NOT_FOUND",
            "message": err.to_string(),
        }),
    }
}

fn list_routers_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        return match load_router_snapshot(state) {
            Ok(snapshot) => serde_json::json!({
                "status": "ok",
                "routers": routers_from_snapshot(&snapshot),
            }),
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "SHM_NOT_FOUND",
                "message": err.to_string(),
            }),
        };
    }

    match load_lsa_snapshot(state) {
        Ok(snapshot) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "routers": remote_routers_for_hive(state, &snapshot, &target_hive),
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SHM_NOT_FOUND",
            "message": err.to_string(),
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

    let runtimes = match load_runtime_manifest() {
        Some(manifest) => {
            let manifest_hash = local_runtime_manifest_hash().ok().flatten();
            serde_json::json!({
                "status": "ok",
                "manifest_version": manifest.version,
                "manifest_hash": manifest_hash,
                "runtimes": manifest.runtimes,
            })
        }
        None => serde_json::json!({
            "status": "missing",
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

fn list_deployments_flow(payload: &serde_json::Value) -> serde_json::Value {
    let limit = deployment_limit_from_payload(payload);
    let category = payload
        .get("category")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    match read_deployment_history(limit, None, category) {
        Ok(entries) => serde_json::json!({
            "status": "ok",
            "entries": entries,
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
            "hive_id": target_hive,
            "entries": entries,
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

fn drift_filter_str<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn list_drift_alerts_flow(payload: &serde_json::Value) -> serde_json::Value {
    let limit = drift_alert_limit_from_payload(payload);
    let category = drift_filter_str(payload, "category");
    let severity = drift_filter_str(payload, "severity");
    let kind = drift_filter_str(payload, "kind");
    match read_drift_alerts(limit, None, category, severity, kind) {
        Ok(entries) => serde_json::json!({
            "status": "ok",
            "entries": entries,
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
            "hive_id": target_hive,
            "entries": entries,
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

fn remote_routers_for_hive(
    state: &OrchestratorState,
    snapshot: &LsaSnapshot,
    target_hive: &str,
) -> Vec<serde_json::Value> {
    let now = now_epoch_ms();
    let gateway_base = state
        .gateway_name
        .split('@')
        .next()
        .unwrap_or(&state.gateway_name)
        .to_string();
    for hive in &snapshot.hives {
        let Some(hive_id) = remote_hive_name(hive) else {
            continue;
        };
        if hive_id != target_hive {
            continue;
        }
        let status = remote_hive_status(hive, now);
        let router_uuid = Uuid::from_bytes(hive.router_uuid);
        let router_name =
            remote_router_name(hive).unwrap_or_else(|| format!("{gateway_base}@{hive_id}"));
        return vec![serde_json::json!({
            "uuid": router_uuid.to_string(),
            "name": router_name,
            "is_gateway": true,
            "nodes_count": hive.node_count,
            "status": status,
            "heartbeat": hive.last_updated,
        })];
    }
    Vec::new()
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

fn list_hives(_state_dir: &Path) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    let root = hives_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
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
    "for s in rt-gateway sy-config-routes sy-opa-rules sy-identity sy-orchestrator sy-admin sy-storage fluxbee-syncthing; do \
systemctl stop --no-block \"$s\" >/dev/null 2>&1 || true; \
systemctl disable \"$s\" >/dev/null 2>&1 || true; \
systemctl kill -s KILL \"$s\" >/dev/null 2>&1 || true; \
systemctl reset-failed \"$s\" >/dev/null 2>&1 || true; \
done"
}

fn remove_hive_cleanup_local_flow() -> serde_json::Value {
    let deferred_script = format!("sleep 1; {}", remove_hive_cleanup_script());
    match Command::new("bash").arg("-lc").arg(deferred_script).spawn() {
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

    let remote_cleanup: &str;
    let remote_cleanup_via: &str;
    let address = read_hive_info(&root, hive_id)
        .ok()
        .and_then(|info| {
            info.get("address")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
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
        remote_cleanup = "socket_ok";
        remote_cleanup_via = "socket";
    } else {
        remote_cleanup = if socket_cleanup_timed_out {
            "socket_timeout"
        } else {
            "local_only"
        };
        remote_cleanup_via = "local_only";
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

fn runtime_keep_versions(
    manifest: &RuntimeManifest,
) -> Result<HashMap<String, HashSet<String>>, OrchestratorError> {
    let runtimes = manifest
        .runtimes
        .as_object()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let mut keep_map: HashMap<String, HashSet<String>> = HashMap::new();
    for (runtime, entry) in runtimes {
        if !valid_token(runtime) {
            return Err(format!("runtime manifest invalid runtime name '{}'", runtime).into());
        }
        let mut keep: HashSet<String> = HashSet::new();
        if let Some(current) = entry.get("current").and_then(|v| v.as_str()) {
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
        if let Some(available) = entry.get("available").and_then(|v| v.as_array()) {
            for value in available {
                let version = value
                    .as_str()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "runtime manifest invalid available version for runtime '{}'",
                            runtime
                        )
                    })?;
                if !valid_token(version) {
                    return Err(format!(
                        "runtime manifest invalid available version '{}'",
                        version
                    )
                    .into());
                }
                keep.insert(version.to_string());
            }
        }
        if !keep.is_empty() {
            keep_map.insert(runtime.clone(), keep);
        }
    }
    Ok(keep_map)
}

fn apply_runtime_retention(
    manifest: &RuntimeManifest,
) -> Result<RuntimeRetentionStats, OrchestratorError> {
    let keep_map = runtime_keep_versions(manifest)?;
    let root = runtimes_root();
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

fn orchestrator_runtime_manifest_path() -> PathBuf {
    orchestrator_runtime_dir().join(RUNTIME_MANIFEST_FILE)
}

fn load_runtime_manifest() -> Option<RuntimeManifest> {
    let primary = orchestrator_runtime_manifest_path();
    let mut paths = vec![primary];
    paths.extend(local_runtime_manifest_paths());
    for path in paths {
        let data = match fs::read_to_string(&path) {
            Ok(data) => data,
            Err(_) => continue,
        };
        if let Ok(manifest) = serde_json::from_str::<RuntimeManifest>(&data) {
            if manifest.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION || manifest.version == 0 {
                tracing::warn!(
                    path = %path.display(),
                    schema_version = manifest.schema_version,
                    version = manifest.version,
                    "ignoring invalid local runtime manifest"
                );
                continue;
            }
            return Some(manifest);
        }
    }
    None
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

async fn runtime_verify_and_sync(state: &OrchestratorState) -> Result<(), OrchestratorError> {
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
        tracing::info!(
            "watchdog verify: remote runtime/core/vendor SSH sync disabled in v2 (use SYSTEM_UPDATE per hive)"
        );
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
    let runtimes = manifest
        .runtimes
        .as_object()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let entry = runtimes
        .get(runtime)
        .ok_or_else(|| format!("runtime '{runtime}' not found in manifest"))?;

    let current = entry
        .get("current")
        .and_then(|v| v.as_str())
        .map(|v| v.trim())
        .unwrap_or("");
    let available: Vec<&str> = entry
        .get("available")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

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
    let runtimes = manifest
        .runtimes
        .as_object()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    if runtimes.contains_key(runtime) {
        return Ok(runtime.to_string());
    }
    Err(format!("runtime '{runtime}' not found in manifest").into())
}

fn runtime_start_script(runtime: &str, version: &str) -> String {
    format!("/var/lib/fluxbee/dist/runtimes/{runtime}/{version}/bin/start.sh")
}

fn local_runtime_script_exists(script_path: &str) -> bool {
    Path::new(script_path).is_file()
}

fn system_forward_timeout() -> Duration {
    let secs = std::env::var("JSR_ORCH_SYSTEM_FORWARD_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(45);
    Duration::from_secs(secs)
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

    let socket_dir = json_router::paths::router_socket_dir();
    let relay_name = format!("SY.orchestrator.relay.{}", now_epoch_ms());
    let relay_config = NodeConfig {
        name: relay_name,
        router_socket: socket_dir,
        uuid_persistence_dir: state.state_dir.join("nodes"),
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

    let trace_id = Uuid::new_v4().to_string();
    let request = Message {
        routing: Routing {
            src: relay_sender.uuid().to_string(),
            dst: Destination::Unicast(format!("SY.orchestrator@{target_hive}")),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(request_msg.to_string()),
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
            target_hive,
            forward_timeout.as_secs()
        )
        .into()),
    }
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

    let manifest = match load_runtime_manifest() {
        Some(manifest) => manifest,
        None => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_MANIFEST_MISSING",
                "message": "runtime manifest not found",
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

    let start_script = runtime_start_script(&runtime_key, &version);
    if !local_runtime_script_exists(&start_script) {
        return serde_json::json!({
            "status": "error",
            "error_code": "RUNTIME_NOT_PRESENT",
            "message": format!("runtime script missing in dist path: {}", start_script),
            "target": target_hive,
            "node_name": node_name,
        });
    }

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
            });
        }
    }

    let cmd = format!(
        "systemd-run --unit {} --collect --property Restart=always --property RestartSec=5 {}",
        unit, start_script
    );

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
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SPAWN_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": node_name,
            "unit": unit,
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

    let normalized_node_name = if node_name.is_empty() {
        None
    } else {
        match normalize_node_name_for_target(&node_name, &target_hive) {
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
            normalized_node_name
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
            if let Some(name) = normalized_node_name.as_ref() {
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
                "node_name": normalized_node_name,
                "unit": unit,
            }),
        };
    }

    match systemd_unit_is_active(&unit) {
        Ok(false) => {
            return serde_json::json!({
                "status": "ok",
                "state": "not_found",
                "hive": target_hive,
                "target": target_hive,
                "node_name": normalized_node_name,
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
                "node_name": normalized_node_name,
                "unit": unit,
            });
        }
    }

    match execute_on_hive(state, &target_hive, &cmd, "kill_node") {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "hive": target_hive,
            "target": target_hive,
            "node_name": normalized_node_name,
            "unit": unit,
            "signal": signal,
            "force": force,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "KILL_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "node_name": normalized_node_name,
            "unit": unit,
        }),
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

fn shell_single_quote(value: &str) -> String {
    value.replace('\'', "'\"'\"'")
}

fn sha256_file(path: &Path) -> Result<String, OrchestratorError> {
    let mut cmd = Command::new("sha256sum");
    cmd.arg(path);
    let out = run_cmd_output(cmd, "sha256sum core binary")?;
    Ok(out
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
        .unwrap_or_default())
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
    if manifest.components.contains_key("sy-identity") {
        out.push("sy-identity".to_string());
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
        let upload_refs: Vec<&str> = local_paths.iter().map(String::as_str).collect();
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
            "mkdir -p /var/lib/fluxbee/dist/core/bin".to_string(),
        ];
        for name in &component_names {
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

    let mut stable = 0_u8;
    let mut last_err: Option<String> = None;
    for _ in 0..timeout_secs {
        match ssh_with_key(
            address,
            key_path,
            &sudo_wrap(&is_active_cmd),
            BOOTSTRAP_SSH_USER,
        ) {
            Ok(()) => {
                let substate = ssh_with_key_output(
                    address,
                    key_path,
                    &sudo_wrap(&substate_cmd),
                    BOOTSTRAP_SSH_USER,
                )
                .unwrap_or_default();
                if substate.trim() == "running" {
                    stable = stable.saturating_add(1);
                    if stable >= 3 {
                        return Ok(());
                    }
                } else {
                    stable = 0;
                }
            }
            Err(err) => {
                stable = 0;
                last_err = Some(err.to_string());
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    let detail = last_err.unwrap_or_else(|| "timed out waiting for active/running".to_string());
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
        "systemctl --no-pager -l status '{service}' 2>/dev/null | tail -n {lines} || true",
        service = service_q,
        lines = lines
    );
    match ssh_with_key_output(address, key_path, &sudo_wrap(&cmd), BOOTSTRAP_SSH_USER) {
        Ok(out) => {
            let trimmed = out.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(truncate_for_error(trimmed, 800))
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

async fn add_hive_finalize_via_socket(
    state: &OrchestratorState,
    hive_id: &str,
    address: &str,
    require_dist_sync: bool,
    dist_sync_probe_timeout_secs: u64,
    syncthing_peer_device_id: Option<&str>,
    syncthing_peer_name: Option<&str>,
) -> Result<serde_json::Value, OrchestratorError> {
    let timeout_secs = ADD_HIVE_FINALIZE_SOCKET_TIMEOUT_SECS.max(dist_sync_probe_timeout_secs);
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
                    payload
                }
                Err(err) => {
                    append_add_hive_finalize_history(
                        state,
                        hive_id,
                        "error",
                        Some(err.to_string()),
                    );
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "FINALIZE_FAILED",
                        "message": format!("worker socket-only finalize failed: {}", err),
                        "hive_id": hive_id,
                        "address": address,
                        "bootstrap_mode": "socket_only_existing_orchestrator",
                        "harden_ssh": harden_ssh,
                        "restrict_ssh_requested": restrict_ssh,
                        "require_dist_sync": require_dist_sync,
                        "dist_sync_probe_timeout_secs": dist_sync_probe_timeout_secs,
                    });
                }
            };
            let mut syncthing_peer_linked = false;
            if syncthing_expected {
                let Some(worker_syncthing_device_id) =
                    syncthing_device_id_from_finalize_payload(&finalize)
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
                    &worker_syncthing_device_id,
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
            let info_path = hives_root().join(hive_id).join("info.yaml");
            let info = serde_yaml::to_string(&serde_json::json!({
                "hive_id": hive_id,
                "address": address,
                "created_at": now_epoch_ms().to_string(),
                "status": "connected",
            }))
            .unwrap_or_default();
            if let Err(err) = fs::write(info_path, info) {
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
                "finalize": finalize,
            });
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

    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap(&format!(
            "mkdir -p /etc/fluxbee /var/lib/fluxbee/state/nodes /var/lib/fluxbee/opa/current /var/lib/fluxbee/opa/staged /var/lib/fluxbee/opa/backup /var/lib/fluxbee/nats /var/lib/fluxbee/dist /var/lib/fluxbee/dist/runtimes /var/lib/fluxbee/dist/core/bin /var/lib/fluxbee/dist/vendor /var/run/fluxbee/routers '{}' '{}'",
            state.blob.path.display(),
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
    let hive_yaml = format!(
        "hive_id: {}\nrole: worker\nwan:\n  gateway_name: RT.gateway\n  uplinks:\n    - address: \"{}\"\nnats:\n  mode: embedded\n  port: 4222\nstorage:\n  path: \"{}\"\nblob:\n  enabled: {}\n  path: \"{}\"\n  sync:\n    enabled: {}\n    tool: \"{}\"\n    api_port: {}\n    data_dir: \"{}\"\ndist:\n  path: \"{}\"\n  sync:\n    enabled: {}\n    tool: \"{}\"\n",
        hive_id,
        worker_uplink,
        storage_path,
        desired_blob.enabled,
        desired_blob.path.display(),
        desired_blob.sync_enabled,
        desired_blob.sync_tool,
        desired_blob.sync_api_port,
        desired_blob.sync_data_dir.display(),
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
        let journal_tail = remote_service_journal_tail(address, &key_path, "sy-orchestrator", 40);
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

    let info_path = hives_root().join(hive_id).join("info.yaml");
    let info = serde_yaml::to_string(&serde_json::json!({
        "hive_id": hive_id,
        "address": address,
        "created_at": now_epoch_ms().to_string(),
        "status": if wan_connected && orchestrator_connected { "connected" } else { "pending" },
    }))
    .unwrap_or_default();
    if let Err(err) = fs::write(info_path, info) {
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
        let journal_tail = remote_service_journal_tail(address, &key_path, "sy-orchestrator", 40);
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
    if syncthing_expected {
        let Some(worker_syncthing_device_id) = syncthing_device_id_from_finalize_payload(&finalize)
        else {
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
            &worker_syncthing_device_id,
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
    env_flag_enabled("FLUXBEE_ADD_HIVE_HARDEN_SSH") || env_flag_enabled("JSR_ADD_HIVE_HARDEN_SSH")
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
    if let Ok(raw) = std::env::var("FLUXBEE_ADD_HIVE_RESTRICT_SSH") {
        if let Some(value) = parse_bool_str(&raw) {
            return value;
        }
    }
    if let Ok(raw) = std::env::var("JSR_ADD_HIVE_RESTRICT_SSH") {
        if let Some(value) = parse_bool_str(&raw) {
            return value;
        }
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

fn env_flag_enabled(name: &str) -> bool {
    let raw = match std::env::var(name) {
        Ok(value) => value,
        Err(_) => return false,
    };
    parse_bool_str(&raw).unwrap_or(false)
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

fn remote_router_name(entry: &RemoteHiveEntry) -> Option<String> {
    if entry.router_name_len == 0 {
        return None;
    }
    let len = entry.router_name_len as usize;
    Some(String::from_utf8_lossy(&entry.router_name[..len]).into_owned())
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
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee" || r == "mother")
}

fn is_worker_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "worker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use json_router::shm::LsaHeaderSnapshot;

    fn write_name(buf: &mut [u8], value: &str) -> u16 {
        let bytes = value.as_bytes();
        let len = bytes.len().min(buf.len());
        buf[..len].copy_from_slice(&bytes[..len]);
        len as u16
    }

    fn test_state() -> OrchestratorState {
        OrchestratorState {
            hive_id: "sandbox".to_string(),
            is_motherbee: true,
            started_at: Instant::now(),
            config_dir: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp"),
            gateway_name: "RT.gateway@sandbox".to_string(),
            storage_path: Mutex::new("/var/lib/fluxbee".to_string()),
            wan_listen: None,
            wan_authorized_hives: Vec::new(),
            tracked_nodes: Mutex::new(HashSet::new()),
            system_allowed_origins: HashSet::new(),
            runtime_manifest: Mutex::new(None),
            last_runtime_verify: Mutex::new(Instant::now()),
            nats_endpoint: "nats://127.0.0.1:4222".to_string(),
            blob: BlobRuntimeConfig {
                enabled: true,
                path: PathBuf::from("/var/lib/fluxbee/blob"),
                sync_enabled: false,
                sync_tool: "syncthing".to_string(),
                sync_api_port: 8384,
                sync_data_dir: PathBuf::from("/var/lib/fluxbee/syncthing"),
            },
            dist: DistRuntimeConfig {
                path: PathBuf::from("/var/lib/fluxbee/dist"),
                sync_enabled: true,
                sync_tool: "syncthing".to_string(),
            },
            blob_sync_last_desired: Mutex::new(BlobRuntimeConfig {
                enabled: true,
                path: PathBuf::from("/var/lib/fluxbee/blob"),
                sync_enabled: false,
                sync_tool: "syncthing".to_string(),
                sync_api_port: 8384,
                sync_data_dir: PathBuf::from("/var/lib/fluxbee/syncthing"),
            }),
        }
    }

    #[test]
    fn remote_routers_projection_uses_real_uuid_and_name() {
        let router_uuid = Uuid::new_v4();
        let mut hive = RemoteHiveEntry {
            hive_id: [0u8; 64],
            hive_id_len: 0,
            router_uuid: *router_uuid.as_bytes(),
            router_name: [0u8; 64],
            router_name_len: 0,
            last_lsa_seq: 10,
            last_updated: now_epoch_ms(),
            flags: 0,
            node_count: 2,
            route_count: 0,
            vpn_count: 0,
        };
        hive.hive_id_len = write_name(&mut hive.hive_id, "worker-220");
        hive.router_name_len = write_name(&mut hive.router_name, "RT.gateway@worker-220");

        let snapshot = LsaSnapshot {
            header: LsaHeaderSnapshot {
                hive_count: 1,
                total_node_count: 0,
                total_route_count: 0,
                total_vpn_count: 0,
                heartbeat: now_epoch_ms(),
            },
            hives: vec![hive],
            nodes: Vec::new(),
            routes: Vec::new(),
            vpns: Vec::new(),
        };
        let state = test_state();
        let routers = remote_routers_for_hive(&state, &snapshot, "worker-220");

        assert_eq!(routers.len(), 1);
        assert_eq!(routers[0]["uuid"], router_uuid.to_string());
        assert_eq!(routers[0]["name"], "RT.gateway@worker-220");
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
}
