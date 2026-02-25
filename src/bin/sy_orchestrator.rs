use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

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
const RUNTIME_VERIFY_INTERVAL_SECS: u64 = 300;
const RUNTIME_MANIFEST_FILE: &str = "runtime-manifest.json";
const NATS_BOOTSTRAP_TIMEOUT_SECS: u64 = 20;
const STORAGE_BOOTSTRAP_TIMEOUT_SECS: u64 = 30;
const STORAGE_DB_READINESS_TIMEOUT_SECS: u64 = 30;
const STORAGE_DB_READINESS_REQUEST_TIMEOUT_SECS: u64 = 3;
const SUBJECT_STORAGE_METRICS_GET: &str = "storage.metrics.get";
const SY_NODES_BOOTSTRAP_TIMEOUT_SECS: u64 = 60;
const SYNCTHING_SERVICE_NAME: &str = "fluxbee-syncthing";
const SYNCTHING_BOOTSTRAP_TIMEOUT_SECS: u64 = 30;
const SYNCTHING_HEALTH_TIMEOUT_SECS: u64 = 2;
const SYNCTHING_INSTALL_USER: &str = "fluxbee";
const SYNCTHING_SYNC_PORT_TCP: u16 = 22000;
const SYNCTHING_SYNC_PORT_UDP: u16 = 22000;
const SYNCTHING_DISCOVERY_PORT_UDP: u16 = 21027;
const SYNCTHING_INSTALL_PATH: &str = "/usr/bin/syncthing";
const SYNCTHING_VENDOR_SOURCE_PATH: &str = "/var/lib/fluxbee/vendor/syncthing/syncthing";
const CORE_BIN_SOURCE_DIR: &str = "/var/lib/fluxbee/core/bin";
const CORE_MANIFEST_PATH: &str = "/var/lib/fluxbee/core/manifest.json";
const DEFAULT_BLOB_ENABLED: bool = true;
const DEFAULT_BLOB_PATH: &str = "/var/lib/fluxbee/blob";
const DEFAULT_BLOB_SYNC_ENABLED: bool = false;
const DEFAULT_BLOB_SYNC_TOOL: &str = "syncthing";
const DEFAULT_BLOB_SYNC_API_PORT: u16 = 8384;
const DEFAULT_BLOB_SYNC_DATA_DIR: &str = "/var/lib/fluxbee/syncthing";

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    nats: Option<NatsSection>,
    storage: Option<StorageSection>,
    blob: Option<BlobSection>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobRuntimeConfig {
    enabled: bool,
    path: PathBuf,
    sync_enabled: bool,
    sync_tool: String,
    sync_api_port: u16,
    sync_data_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct StorageSection {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HiveInfoFile {
    #[serde(default)]
    address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RuntimeManifest {
    #[serde(default)]
    version: u64,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    runtimes: serde_json::Value,
    #[serde(default)]
    hash: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct CoreManifest {
    schema_version: u64,
    components: std::collections::BTreeMap<String, CoreManifestComponent>,
}

#[derive(Debug, Deserialize, Clone)]
struct CoreManifestComponent {
    service: String,
    version: String,
    build_id: String,
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
}

struct OrchestratorState {
    hive_id: String,
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
    blob_sync_last_desired: Mutex<BlobRuntimeConfig>,
}

const CRITICAL_SERVICES: [&str; 5] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-admin",
    "sy-storage",
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
    if !is_mother_role(hive.role.as_deref()) {
        tracing::warn!("SY.orchestrator solo corre en motherbee; role != motherbee");
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
    let storage_path = storage_path_from_hive(&hive);
    let runtime_manifest = load_runtime_manifest();
    let system_allowed_origins = load_system_allowed_origins(&hive.hive_id);
    tracing::info!(allowed = ?system_allowed_origins, "system message origin allowlist loaded");
    let state = OrchestratorState {
        hive_id: hive.hive_id.clone(),
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
        blob_sync_last_desired: Mutex::new(blob_runtime),
    };
    tracing::info!(
        blob_enabled = state.blob.enabled,
        blob_path = %state.blob.path.display(),
        blob_sync_enabled = state.blob.sync_enabled,
        blob_sync_tool = %state.blob.sync_tool,
        blob_sync_api_port = state.blob.sync_api_port,
        blob_sync_data_dir = %state.blob.sync_data_dir.display(),
        "blob runtime config loaded"
    );
    ensure_dirs(&config_dir, &state_dir, &run_dir, &state.blob)?;
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
    let core_bins: Vec<String> = core_manifest
        .components
        .keys()
        .map(|name| format!("{CORE_BIN_SOURCE_DIR}/{name}"))
        .collect();
    validate_core_manifest_for_bins(&core_bins)?;

    tracing::info!("starting rt-gateway");
    systemd_start("rt-gateway")?;
    wait_for_router_ready(state, socket_dir, Duration::from_secs(30)).await?;
    wait_for_nats_ready(
        &state.nats_endpoint,
        Duration::from_secs(NATS_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;
    if state.blob.sync_enabled {
        ensure_blob_sync_runtime(&state.blob).await?;
    } else {
        disable_blob_sync_runtime_local()?;
        disable_remote_blob_sync_all_hives(state);
    }

    let mut services = vec!["sy-config-routes", "sy-opa-rules", "sy-admin", "sy-storage"];
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
    let mut required = vec!["SY.config.routes", "SY.opa.rules", "SY.admin"];
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
    for service in CRITICAL_SERVICES {
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
        "run_node" => run_node_flow(state, &msg.payload),
        "kill_node" => kill_node_flow(state, &msg.payload),
        "run_router" => run_router_flow(state, &msg.payload),
        "kill_router" => kill_router_flow(state, &msg.payload),
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
            let hive = msg
                .payload
                .get("hive_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(hive_id) = hive {
                remove_hive_flow(state, &hive_id)
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing hive_id",
                })
            }
        }
        "add_hive" => {
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
                add_hive_flow(state, &hive_id, &address, harden_ssh)
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
    if matches!(action, "RUNTIME_UPDATE" | "SPAWN_NODE" | "KILL_NODE") {
        let source_name = resolve_system_source_name_with_retry(state, &msg.routing.src).await;
        let is_allowed = source_name
            .as_deref()
            .is_some_and(|name| state.system_allowed_origins.contains(name));
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
                "SPAWN_NODE" => {
                    let _ =
                        send_system_action_response(sender, msg, "SPAWN_NODE_RESPONSE", payload)
                            .await;
                }
                "KILL_NODE" => {
                    let _ = send_system_action_response(sender, msg, "KILL_NODE_RESPONSE", payload)
                        .await;
                }
                _ => {}
            }
            return Ok(());
        }
    }

    match msg.meta.msg.as_deref() {
        Some("RUNTIME_UPDATE") => {
            let manifest = parse_runtime_manifest(&msg.payload)?;
            persist_runtime_manifest(&manifest)?;
            {
                let mut guard = state.runtime_manifest.lock().await;
                *guard = Some(manifest.clone());
            }
            tracing::info!(version = manifest.version, "runtime manifest updated");
            runtime_sync_workers(state, &manifest).await?;
        }
        Some("SPAWN_NODE") => {
            let result = run_node_flow(state, &msg.payload);
            tracing::info!(result = %result, "SPAWN_NODE processed");
            let _ = send_system_action_response(sender, msg, "SPAWN_NODE_RESPONSE", result).await;
        }
        Some("KILL_NODE") => {
            let result = kill_node_flow(state, &msg.payload);
            tracing::info!(result = %result, "KILL_NODE processed");
            let _ = send_system_action_response(sender, msg, "KILL_NODE_RESPONSE", result).await;
        }
        _ => {}
    }
    Ok(())
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
        if start.elapsed() >= Duration::from_millis(500) {
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
    fs::create_dir_all(runtimes_root())?;
    fs::create_dir_all(orchestrator_runtime_dir())?;
    fs::create_dir_all(run_dir)?;
    Ok(())
}

fn blob_sync_tool_is_syncthing(blob: &BlobRuntimeConfig) -> bool {
    blob.sync_tool.trim().eq_ignore_ascii_case("syncthing")
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
    if syncthing_binary_available() {
        return Ok(());
    }
    if !Path::new(SYNCTHING_VENDOR_SOURCE_PATH).exists() {
        return Err(format!(
            "syncthing vendor binary missing at '{}' (seed local vendor repo first)",
            SYNCTHING_VENDOR_SOURCE_PATH
        )
        .into());
    }
    tracing::info!(
        source = SYNCTHING_VENDOR_SOURCE_PATH,
        target = SYNCTHING_INSTALL_PATH,
        "installing syncthing from vendor source"
    );
    let mut install = Command::new("install");
    install
        .arg("-m")
        .arg("0755")
        .arg(SYNCTHING_VENDOR_SOURCE_PATH)
        .arg(SYNCTHING_INSTALL_PATH);
    run_cmd(install, "install syncthing from vendor source")?;
    if !syncthing_binary_available() {
        return Err("syncthing install finished but installed binary is still missing".into());
    }
    Ok(())
}

fn syncthing_unit_contents(blob: &BlobRuntimeConfig, service_user: &str) -> String {
    let service_group = if linux_user_exists(service_user) {
        service_user
    } else {
        "root"
    };
    format!(
        "[Unit]\nDescription=Fluxbee Syncthing (blob sync)\nAfter=network.target\n\n[Service]\nType=simple\nUser={}\nGroup={}\nWorkingDirectory={}\nEnvironment=HOME={}\nExecStart={} -no-browser -no-restart -home={} -gui-address=127.0.0.1:{}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
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

async fn ensure_blob_sync_runtime(blob: &BlobRuntimeConfig) -> Result<(), OrchestratorError> {
    if !blob.sync_enabled {
        return Ok(());
    }
    if !blob_sync_tool_is_syncthing(blob) {
        return Err(format!(
            "unsupported blob.sync.tool '{}' (expected syncthing)",
            blob.sync_tool
        )
        .into());
    }

    ensure_syncthing_installed()?;
    ensure_syncthing_unit(blob)?;
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
    wait_for_syncthing_health(blob).await?;
    tracing::info!(
        service = SYNCTHING_SERVICE_NAME,
        api_port = blob.sync_api_port,
        "blob sync service healthy"
    );
    ensure_remote_blob_sync_all_hives(blob);
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
    let changed = {
        let mut last = state.blob_sync_last_desired.lock().await;
        if *last != desired_blob {
            *last = desired_blob.clone();
            true
        } else {
            false
        }
    };

    if !desired_blob.sync_enabled {
        if changed {
            tracing::info!("blob sync disabled in hive.yaml; reverting syncthing runtime");
            disable_blob_sync_runtime_local()?;
            disable_remote_blob_sync_all_hives(state);
        }
        return Ok(());
    }
    if !blob_sync_tool_is_syncthing(&desired_blob) {
        return Err(format!(
            "unsupported blob.sync.tool '{}' (expected syncthing)",
            desired_blob.sync_tool
        )
        .into());
    }

    if changed {
        tracing::info!("blob sync config changed in hive.yaml; reconciling syncthing runtime");
        ensure_blob_sync_runtime(&desired_blob).await?;
        return Ok(());
    }

    let service_active = systemd_is_active(SYNCTHING_SERVICE_NAME);
    let api_healthy = syncthing_api_healthy(desired_blob.sync_api_port).await;
    if service_active && api_healthy {
        return Ok(());
    }

    tracing::warn!(
        service = SYNCTHING_SERVICE_NAME,
        service_active = service_active,
        api_healthy = api_healthy,
        "syncthing unhealthy; restarting"
    );
    ensure_blob_sync_runtime(&desired_blob).await?;
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

fn nodes_from_snapshot(snapshot: &ShmSnapshot) -> Vec<serde_json::Value> {
    snapshot
        .nodes
        .iter()
        .filter_map(|node| node_entry_to_json(node))
        .collect()
}

fn list_nodes_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    if target_hive == state.hive_id {
        return match load_router_snapshot(state) {
            Ok(snapshot) => serde_json::json!({
                "status": "ok",
                "nodes": nodes_from_snapshot(&snapshot),
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

fn node_entry_to_json(entry: &NodeEntry) -> Option<serde_json::Value> {
    if entry.name_len == 0 {
        return None;
    }
    let name = node_name(entry);
    let uuid = Uuid::from_slice(&entry.uuid).ok()?;
    Some(serde_json::json!({
        "uuid": uuid.to_string(),
        "name": name,
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
        let Some(node_json) = remote_node_to_json(node, hive_entry.last_updated, node.flags) else {
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
) -> Option<serde_json::Value> {
    if entry.name_len == 0 {
        return None;
    }
    let len = entry.name_len as usize;
    let name = String::from_utf8_lossy(&entry.name[..len]).into_owned();
    let uuid = Uuid::from_slice(&entry.uuid).ok()?;
    Some(serde_json::json!({
        "uuid": uuid.to_string(),
        "name": name,
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

fn remove_hive_flow(state: &OrchestratorState, hive_id: &str) -> serde_json::Value {
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

    let (address, key_path) = match hive_access(hive_id) {
        Ok(parts) => parts,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "REMOVE_FAILED",
                "message": format!("cannot resolve hive access for remote cleanup: {err}"),
            })
        }
    };

    let cleanup_cmd = "systemctl disable --now rt-gateway >/dev/null 2>&1 || true; \
systemctl stop rt-gateway >/dev/null 2>&1 || true; \
systemctl kill -s KILL rt-gateway >/dev/null 2>&1 || true; \
systemctl reset-failed rt-gateway >/dev/null 2>&1 || true; \
systemctl disable --now sy-config-routes >/dev/null 2>&1 || true; \
systemctl stop sy-config-routes >/dev/null 2>&1 || true; \
systemctl kill -s KILL sy-config-routes >/dev/null 2>&1 || true; \
systemctl reset-failed sy-config-routes >/dev/null 2>&1 || true; \
systemctl disable --now sy-opa-rules >/dev/null 2>&1 || true; \
systemctl stop sy-opa-rules >/dev/null 2>&1 || true; \
systemctl kill -s KILL sy-opa-rules >/dev/null 2>&1 || true; \
systemctl reset-failed sy-opa-rules >/dev/null 2>&1 || true; \
systemctl disable --now sy-identity >/dev/null 2>&1 || true; \
systemctl stop sy-identity >/dev/null 2>&1 || true; \
systemctl kill -s KILL sy-identity >/dev/null 2>&1 || true; \
systemctl reset-failed sy-identity >/dev/null 2>&1 || true";
    if let Err(err) = ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap(cleanup_cmd),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "REMOVE_FAILED",
            "message": format!("remote cleanup failed: {err}"),
            "hive_id": hive_id,
            "address": address,
        });
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
        "remote_cleanup": "stopped",
    })
}

fn read_hive_info(root: &Path, hive_id: &str) -> Result<serde_json::Value, OrchestratorError> {
    let path = root.join(hive_id).join("info.yaml");
    let data = fs::read_to_string(&path)?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let json = serde_json::to_value(yaml)?;
    Ok(json)
}

fn parse_runtime_manifest(
    payload: &serde_json::Value,
) -> Result<RuntimeManifest, OrchestratorError> {
    if !payload.is_object() {
        return Err("runtime manifest payload must be an object".into());
    }
    let manifest: RuntimeManifest = serde_json::from_value(payload.clone())?;
    if manifest.version == 0 {
        return Err("runtime manifest missing version".into());
    }
    Ok(manifest)
}

fn runtimes_root() -> PathBuf {
    json_router::paths::storage_root_dir().join("runtimes")
}

fn orchestrator_runtime_dir() -> PathBuf {
    json_router::paths::storage_root_dir().join("orchestrator")
}

fn orchestrator_runtime_manifest_path() -> PathBuf {
    orchestrator_runtime_dir().join(RUNTIME_MANIFEST_FILE)
}

fn load_runtime_manifest() -> Option<RuntimeManifest> {
    let primary = orchestrator_runtime_manifest_path();
    let fallback = runtimes_root().join("manifest.json");
    for path in [primary, fallback] {
        let data = match fs::read_to_string(&path) {
            Ok(data) => data,
            Err(_) => continue,
        };
        if let Ok(manifest) = serde_json::from_str::<RuntimeManifest>(&data) {
            return Some(manifest);
        }
    }
    None
}

fn persist_runtime_manifest(manifest: &RuntimeManifest) -> Result<(), OrchestratorError> {
    fs::create_dir_all(orchestrator_runtime_dir())?;
    fs::create_dir_all(runtimes_root())?;
    let data = serde_json::to_string_pretty(manifest)?;
    fs::write(orchestrator_runtime_manifest_path(), &data)?;
    fs::write(runtimes_root().join("manifest.json"), data)?;
    Ok(())
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
        {
            let mut guard = state.runtime_manifest.lock().await;
            *guard = Some(manifest.clone());
        }
        runtime_sync_workers(state, &manifest).await?;
    }

    core_sync_workers().await
}

async fn runtime_sync_workers(
    _state: &OrchestratorState,
    manifest: &RuntimeManifest,
) -> Result<(), OrchestratorError> {
    if manifest.version == 0 {
        return Ok(());
    }
    let local_hash = local_runtime_manifest_hash()?;
    let workers = list_worker_access();
    for (hive_id, address, key_path) in workers {
        let remote_hash = remote_runtime_manifest_hash(&address, &key_path)
            .ok()
            .flatten();
        if local_hash.is_some() && remote_hash == local_hash {
            continue;
        }
        if let Err(err) = sync_runtime_to_worker(&hive_id, &address, &key_path) {
            tracing::warn!(hive = %hive_id, error = %err, "runtime sync failed");
        }
    }
    Ok(())
}

async fn core_sync_workers() -> Result<(), OrchestratorError> {
    let local_hash = local_core_manifest_hash()?;
    let workers = list_worker_access();
    for (hive_id, address, key_path) in workers {
        let remote_hash = remote_core_manifest_hash(&address, &key_path)
            .ok()
            .flatten();
        if local_hash.is_some() && remote_hash == local_hash {
            continue;
        }
        if let Err(err) = sync_core_to_worker(&hive_id, &address, &key_path) {
            tracing::warn!(hive = %hive_id, error = %err, "core sync failed");
        }
    }
    Ok(())
}

fn list_worker_access() -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let root = hives_root();
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        let hive_id = entry.file_name().to_string_lossy().to_string();
        let hive_dir = root.join(&hive_id);
        let key_path = hive_dir.join("ssh.key");
        if !key_path.exists() {
            continue;
        }
        let info_path = hive_dir.join("info.yaml");
        let data = match fs::read_to_string(&info_path) {
            Ok(data) => data,
            Err(_) => continue,
        };
        let info: HiveInfoFile = match serde_yaml::from_str(&data) {
            Ok(info) => info,
            Err(_) => continue,
        };
        let Some(address) = info.address else {
            continue;
        };
        if address.trim().is_empty() {
            continue;
        }
        out.push((hive_id, address, key_path));
    }

    out
}

fn local_runtime_manifest_hash() -> Result<Option<String>, OrchestratorError> {
    let manifest_path = runtimes_root().join("manifest.json");
    if !manifest_path.exists() {
        return Ok(None);
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
    if hash.is_empty() {
        return Ok(None);
    }
    Ok(Some(hash))
}

fn remote_runtime_manifest_hash(
    address: &str,
    key_path: &Path,
) -> Result<Option<String>, OrchestratorError> {
    let cmd = r#"bash -lc "if [ -f /var/lib/fluxbee/runtimes/manifest.json ]; then sha256sum /var/lib/fluxbee/runtimes/manifest.json | awk '{print $1}'; fi""#;
    let out = ssh_with_key_output(address, key_path, &sudo_wrap(cmd), BOOTSTRAP_SSH_USER)?;
    let hash = out.trim().to_string();
    if hash.is_empty() {
        return Ok(None);
    }
    Ok(Some(hash))
}

fn local_core_manifest_hash() -> Result<Option<String>, OrchestratorError> {
    let manifest_path = Path::new(CORE_MANIFEST_PATH);
    if !manifest_path.exists() {
        return Ok(None);
    }
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
    let cmd = format!(
        "bash -lc \"if [ -f '{}' ]; then sha256sum '{}' | awk '{{print $1}}'; fi\"",
        CORE_MANIFEST_PATH, CORE_MANIFEST_PATH
    );
    let out = ssh_with_key_output(address, key_path, &sudo_wrap(&cmd), BOOTSTRAP_SSH_USER)?;
    let hash = out.trim().to_string();
    if hash.is_empty() {
        return Ok(None);
    }
    Ok(Some(hash))
}

fn sync_runtime_to_worker(
    hive_id: &str,
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let local_root = runtimes_root();
    if !local_root.exists() {
        return Ok(());
    }

    let remote_stage = format!("/tmp/fluxbee-runtimes-sync-{hive_id}");
    let prepare_stage = format!(
        "rm -rf '{stage}' && mkdir -p '{stage}'",
        stage = remote_stage
    );
    ssh_with_key(address, key_path, &prepare_stage, BOOTSTRAP_SSH_USER)?;

    let mut cmd = Command::new("rsync");
    cmd.arg("-rz")
        .arg("--delete")
        .arg("--omit-dir-times")
        .arg("--no-perms")
        .arg("--no-owner")
        .arg("--no-group")
        .arg("-e")
        .arg(format!(
            "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10",
            key_path.display()
        ))
        .arg(format!("{}/", local_root.display()))
        .arg(format!(
            "{}@{}:{}/",
            BOOTSTRAP_SSH_USER, address, remote_stage
        ));
    run_cmd(cmd, &format!("rsync runtime stage ({hive_id})"))?;

    let promote_cmd = format!(
        "mkdir -p /var/lib/fluxbee/runtimes && rsync -r --delete --omit-dir-times --no-perms --no-owner --no-group '{stage}/' /var/lib/fluxbee/runtimes/ && rm -rf '{stage}'",
        stage = remote_stage
    );
    let promote_cmd_escaped = promote_cmd.replace('"', "\\\"");
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&format!("bash -lc \"{}\"", promote_cmd_escaped)),
        BOOTSTRAP_SSH_USER,
    )
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

fn runtime_start_script(runtime: &str, version: &str) -> String {
    format!("/var/lib/fluxbee/runtimes/{runtime}/{version}/bin/start.sh")
}

fn hive_has_runtime_script(
    state: &OrchestratorState,
    target_hive: &str,
    script_path: &str,
) -> Result<bool, OrchestratorError> {
    if target_hive == state.hive_id {
        return Ok(Path::new(script_path).exists());
    }
    let (address, key_path) = hive_access(target_hive)?;
    let cmd = format!(
        "bash -lc \"if [ -x '{}' ]; then echo 1; else echo 0; fi\"",
        script_path
    );
    let out = ssh_with_key_output(&address, &key_path, &sudo_wrap(&cmd), BOOTSTRAP_SSH_USER)?;
    Ok(out.trim() == "1")
}

fn sync_runtime_to_hive(
    state: &OrchestratorState,
    target_hive: &str,
) -> Result<(), OrchestratorError> {
    if target_hive == state.hive_id {
        return Ok(());
    }
    let (address, key_path) = hive_access(target_hive)?;
    sync_runtime_to_worker(target_hive, &address, &key_path)
}

fn run_node_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let runtime = payload
        .get("runtime")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
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

    let requested_version = payload
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("current")
        .trim();
    if !valid_token(requested_version) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "invalid version",
        });
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
    let version = match resolve_runtime_version(&manifest, runtime, requested_version) {
        Ok(version) => version,
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_NOT_AVAILABLE",
                "message": err.to_string(),
            });
        }
    };

    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    let unit = payload
        .get("unit")
        .and_then(|v| v.as_str())
        .map(|v| sanitize_unit_suffix(v.trim()))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            let base = payload
                .get("config")
                .and_then(|v| v.get("instance_id"))
                .and_then(|v| v.as_str())
                .unwrap_or(runtime);
            format!(
                "fluxbee-node-{}-{}",
                sanitize_unit_suffix(base),
                now_epoch_ms()
            )
        });

    let start_script = runtime_start_script(runtime, &version);
    match hive_has_runtime_script(state, &target_hive, &start_script) {
        Ok(true) => {}
        Ok(false) => {
            if let Err(err) = sync_runtime_to_hive(state, &target_hive) {
                return serde_json::json!({
                    "status": "error",
                    "error_code": "RUNTIME_SYNC_FAILED",
                    "message": err.to_string(),
                    "target": target_hive,
                });
            }
            match hive_has_runtime_script(state, &target_hive, &start_script) {
                Ok(true) => {}
                Ok(false) => {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "RUNTIME_NOT_PRESENT",
                        "message": format!("runtime script missing after sync: {start_script}"),
                        "target": target_hive,
                    });
                }
                Err(err) => {
                    return serde_json::json!({
                        "status": "error",
                        "error_code": "RUNTIME_CHECK_FAILED",
                        "message": err.to_string(),
                        "target": target_hive,
                    });
                }
            }
        }
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "RUNTIME_CHECK_FAILED",
                "message": err.to_string(),
                "target": target_hive,
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
            "runtime": runtime,
            "version": version,
            "requested_version": requested_version,
            "target": target_hive,
            "unit": unit,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SPAWN_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "unit": unit,
        }),
    }
}

fn kill_node_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
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

    let unit = payload
        .get("unit")
        .and_then(|v| v.as_str())
        .map(|v| sanitize_unit_suffix(v.trim()))
        .filter(|v| !v.is_empty())
        .or_else(|| {
            if node_name.is_empty() {
                None
            } else {
                Some(format!(
                    "fluxbee-node-{}",
                    sanitize_unit_suffix(node_name.split('@').next().unwrap_or(&node_name))
                ))
            }
        });

    let Some(unit) = unit else {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "missing unit or node_name",
        });
    };

    let signal = payload
        .get("signal")
        .and_then(|v| v.as_str())
        .unwrap_or("SIGTERM")
        .to_ascii_uppercase();

    let cmd = if signal == "SIGKILL" {
        format!(
            "systemctl kill -s KILL {unit} || true; systemctl stop {unit} || true; systemctl reset-failed {unit} || true"
        )
    } else {
        format!("systemctl stop {unit} || true; systemctl reset-failed {unit} || true")
    };

    match execute_on_hive(state, &target_hive, &cmd, "kill_node") {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "unit": unit,
            "signal": signal,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "KILL_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "unit": unit,
        }),
    }
}

fn run_router_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    let service = payload
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("rt-gateway")
        .trim();

    if !valid_token(service) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "invalid service",
        });
    }

    let cmd = format!("systemctl start {service}");
    match execute_on_hive(state, &target_hive, &cmd, "run_router") {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "service": service,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SERVICE_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "service": service,
        }),
    }
}

fn kill_router_flow(state: &OrchestratorState, payload: &serde_json::Value) -> serde_json::Value {
    let target_hive = target_hive_from_payload(payload, &state.hive_id);
    let service = payload
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("rt-gateway")
        .trim();

    if !valid_token(service) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_REQUEST",
            "message": "invalid service",
        });
    }

    let cmd = format!("systemctl stop {service}");
    match execute_on_hive(state, &target_hive, &cmd, "kill_router") {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "target": target_hive,
            "service": service,
        }),
        Err(err) => serde_json::json!({
            "status": "error",
            "error_code": "SERVICE_FAILED",
            "message": err.to_string(),
            "target": target_hive,
            "service": service,
        }),
    }
}

fn execute_on_hive(
    state: &OrchestratorState,
    target_hive: &str,
    command: &str,
    label: &str,
) -> Result<(), OrchestratorError> {
    if target_hive == state.hive_id {
        let mut cmd = Command::new("bash");
        cmd.arg("-lc").arg(command);
        return run_cmd(cmd, label);
    }

    let (address, key_path) = hive_access(target_hive)?;
    ssh_with_key(&address, &key_path, &sudo_wrap(command), BOOTSTRAP_SSH_USER)
}

fn hive_access(hive_id: &str) -> Result<(String, PathBuf), OrchestratorError> {
    let root = hives_root();
    let hive_dir = root.join(hive_id);
    let key_path = hive_dir.join("ssh.key");
    if !key_path.exists() {
        return Err(format!("missing ssh key for hive '{hive_id}'").into());
    }

    let info_path = hive_dir.join("info.yaml");
    let data = fs::read_to_string(&info_path)?;
    let info: HiveInfoFile = serde_yaml::from_str(&data)?;
    let address = info
        .address
        .ok_or_else(|| format!("missing address for hive '{hive_id}'"))?;
    Ok((address, key_path))
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
    let manifest_path = Path::new(CORE_MANIFEST_PATH);
    if !manifest_path.exists() {
        return Err(format!(
            "core manifest missing at '{}' (run scripts/install.sh)",
            manifest_path.display()
        )
        .into());
    }
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
            format!("core manifest missing component '{component_name}' for '{}'", path.display())
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

fn sync_core_to_worker(
    hive_id: &str,
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let manifest = load_core_manifest()?;
    let component_names: Vec<String> = manifest.components.keys().cloned().collect();
    if component_names.is_empty() {
        return Err("core manifest has no components to sync".into());
    }

    let mut local_paths = Vec::new();
    for name in &component_names {
        if !valid_token(name) {
            return Err(format!("core manifest has invalid component name '{}'", name).into());
        }
        let path = Path::new(CORE_BIN_SOURCE_DIR).join(name);
        if !path.exists() {
            return Err(format!("missing core source binary '{}'", path.display()).into());
        }
        local_paths.push(path.display().to_string());
    }
    validate_core_manifest_for_bins(&local_paths)?;

    let mut upload_paths = local_paths.clone();
    upload_paths.push(CORE_MANIFEST_PATH.to_string());
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
        "mkdir -p /var/lib/fluxbee/core".to_string(),
        "rm -rf /var/lib/fluxbee/core/bin.next".to_string(),
        "mkdir -p /var/lib/fluxbee/core/bin.next".to_string(),
    ];

    for name in &component_names {
        commands.push(format!(
            "install -m 0755 '{stage}/{name}' '/var/lib/fluxbee/core/bin.next/{name}'",
            stage = shell_single_quote(&remote_stage),
            name = shell_single_quote(name),
        ));
    }
    commands.push(format!(
        "install -m 0644 '{stage}/manifest.json' '/var/lib/fluxbee/core/manifest.next.json'",
        stage = shell_single_quote(&remote_stage)
    ));

    for name in &component_names {
        let expected = manifest
            .components
            .get(name)
            .ok_or_else(|| format!("core manifest missing component '{}'", name))?;
        commands.push(format!(
            "test \"$(sha256sum '/var/lib/fluxbee/core/bin.next/{name}' | awk '{{print $1}}')\" = '{sha}'",
            name = shell_single_quote(name),
            sha = shell_single_quote(expected.sha256.trim()),
        ));
        if let Some(size) = expected.size {
            commands.push(format!(
                "test \"$(stat -c %s '/var/lib/fluxbee/core/bin.next/{name}')\" = '{size}'",
                name = shell_single_quote(name),
                size = size
            ));
        }
    }

    commands.push("rm -rf /var/lib/fluxbee/core/bin.prev".to_string());
    commands.push(
        "if [ -d /var/lib/fluxbee/core/bin ]; then mv /var/lib/fluxbee/core/bin /var/lib/fluxbee/core/bin.prev; fi"
            .to_string(),
    );
    commands.push("mv /var/lib/fluxbee/core/bin.next /var/lib/fluxbee/core/bin".to_string());
    commands.push(
        "mv /var/lib/fluxbee/core/manifest.next.json /var/lib/fluxbee/core/manifest.json"
            .to_string(),
    );
    for name in &component_names {
        commands.push(format!(
            "install -m 0755 '/var/lib/fluxbee/core/bin/{name}' '/usr/bin/{name}'",
            name = shell_single_quote(name),
        ));
    }
    commands.push(format!(
        "rm -rf '{stage}'",
        stage = shell_single_quote(&remote_stage)
    ));

    let promote_cmd = commands.join(" && ");
    let promote_cmd_escaped = promote_cmd.replace('"', "\\\"");
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&format!("bash -lc \"{}\"", promote_cmd_escaped)),
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

    Ok(())
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

fn ensure_remote_syncthing_firewall(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let firewall_cmd = format!(
        "bash -lc \"if command -v ufw >/dev/null 2>&1; then ufw allow {}/tcp || true; ufw allow {}/udp || true; ufw allow {}/udp || true; fi; if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then firewall-cmd --add-port={}/tcp || true; firewall-cmd --add-port={}/udp || true; firewall-cmd --add-port={}/udp || true; firewall-cmd --permanent --add-port={}/tcp || true; firewall-cmd --permanent --add-port={}/udp || true; firewall-cmd --permanent --add-port={}/udp || true; fi\"",
        SYNCTHING_SYNC_PORT_TCP,
        SYNCTHING_SYNC_PORT_UDP,
        SYNCTHING_DISCOVERY_PORT_UDP,
        SYNCTHING_SYNC_PORT_TCP,
        SYNCTHING_SYNC_PORT_UDP,
        SYNCTHING_DISCOVERY_PORT_UDP,
        SYNCTHING_SYNC_PORT_TCP,
        SYNCTHING_SYNC_PORT_UDP,
        SYNCTHING_DISCOVERY_PORT_UDP,
    );
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&firewall_cmd),
        BOOTSTRAP_SSH_USER,
    )
}

fn disable_remote_syncthing_firewall(
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let firewall_cmd = format!(
        "bash -lc \"if command -v ufw >/dev/null 2>&1; then ufw --force delete allow {}/tcp || true; ufw --force delete allow {}/udp || true; ufw --force delete allow {}/udp || true; fi; if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then firewall-cmd --remove-port={}/tcp || true; firewall-cmd --remove-port={}/udp || true; firewall-cmd --remove-port={}/udp || true; firewall-cmd --permanent --remove-port={}/tcp || true; firewall-cmd --permanent --remove-port={}/udp || true; firewall-cmd --permanent --remove-port={}/udp || true; fi\"",
        SYNCTHING_SYNC_PORT_TCP,
        SYNCTHING_SYNC_PORT_UDP,
        SYNCTHING_DISCOVERY_PORT_UDP,
        SYNCTHING_SYNC_PORT_TCP,
        SYNCTHING_SYNC_PORT_UDP,
        SYNCTHING_DISCOVERY_PORT_UDP,
        SYNCTHING_SYNC_PORT_TCP,
        SYNCTHING_SYNC_PORT_UDP,
        SYNCTHING_DISCOVERY_PORT_UDP,
    );
    ssh_with_key(
        address,
        key_path,
        &sudo_wrap(&firewall_cmd),
        BOOTSTRAP_SSH_USER,
    )
}

fn ensure_remote_syncthing_runtime(
    hive_id: &str,
    blob: &BlobRuntimeConfig,
) -> Result<(), OrchestratorError> {
    let (address, key_path) = hive_access(hive_id)?;
    if !Path::new(SYNCTHING_VENDOR_SOURCE_PATH).exists() {
        return Err(format!(
            "syncthing vendor binary missing at '{}' (seed local vendor repo first)",
            SYNCTHING_VENDOR_SOURCE_PATH
        )
        .into());
    }

    let remote_tmp_path = format!("/tmp/fluxbee-syncthing-{}.bin", sanitize_unit_suffix(hive_id));
    scp_with_key(
        &address,
        &key_path,
        &[SYNCTHING_VENDOR_SOURCE_PATH],
        &remote_tmp_path,
        BOOTSTRAP_SSH_USER,
    )?;
    let install_cmd = format!(
        "install -m 0755 '{}' '{}' && rm -f '{}'",
        remote_tmp_path, SYNCTHING_INSTALL_PATH, remote_tmp_path
    );
    ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap(&install_cmd),
        BOOTSTRAP_SSH_USER,
    )?;

    let blob_path_q = shell_single_quote(&blob.path.display().to_string());
    let sync_data_dir_q = shell_single_quote(&blob.sync_data_dir.display().to_string());
    let mkdir_cmd = format!("mkdir -p '{blob_path_q}' '{sync_data_dir_q}'");
    ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap(&mkdir_cmd),
        BOOTSTRAP_SSH_USER,
    )?;

    let remote_unit = syncthing_unit_contents(blob, "root");
    write_remote_file(
        &address,
        &key_path,
        &format!("/etc/systemd/system/{SYNCTHING_SERVICE_NAME}.service"),
        &remote_unit,
    )?;
    ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap("systemctl daemon-reload"),
        BOOTSTRAP_SSH_USER,
    )?;
    ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap(&format!("systemctl enable {SYNCTHING_SERVICE_NAME}")),
        BOOTSTRAP_SSH_USER,
    )?;
    ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap(&format!("systemctl restart {SYNCTHING_SERVICE_NAME}")),
        BOOTSTRAP_SSH_USER,
    )?;
    ensure_remote_syncthing_firewall(&address, &key_path)?;
    Ok(())
}

fn ensure_remote_blob_sync_all_hives(blob: &BlobRuntimeConfig) {
    for hive_id in list_managed_hive_ids() {
        if let Err(err) = ensure_remote_syncthing_runtime(&hive_id, blob) {
            tracing::warn!(
                hive_id = hive_id,
                error = %err,
                "failed to ensure syncthing on managed hive"
            );
        }
    }
}

fn disable_remote_syncthing_runtime(hive_id: &str) {
    let (address, key_path) = match hive_access(hive_id) {
        Ok(access) => access,
        Err(err) => {
            tracing::warn!(
                hive_id = hive_id,
                error = %err,
                "failed to resolve hive access for syncthing teardown"
            );
            return;
        }
    };

    let stop_disable_cmd = format!(
        "bash -lc \"systemctl stop {0} || true; systemctl disable {0} || true; rm -f /etc/systemd/system/{0}.service; systemctl daemon-reload || true\"",
        SYNCTHING_SERVICE_NAME
    );
    if let Err(err) = ssh_with_key(
        &address,
        &key_path,
        &sudo_wrap(&stop_disable_cmd),
        BOOTSTRAP_SSH_USER,
    ) {
        tracing::warn!(
            hive_id = hive_id,
            error = %err,
            "failed to stop/disable remote syncthing"
        );
    }

    if let Err(err) = disable_remote_syncthing_firewall(&address, &key_path) {
        tracing::warn!(
            hive_id = hive_id,
            error = %err,
            "failed to remove remote syncthing firewall rules"
        );
    }
}

fn disable_remote_blob_sync_all_hives(state: &OrchestratorState) {
    let _ = state;
    for hive_id in list_managed_hive_ids() {
        disable_remote_syncthing_runtime(&hive_id);
    }
}

fn add_hive_flow(
    state: &OrchestratorState,
    hive_id: &str,
    address: &str,
    harden_ssh: bool,
) -> serde_json::Value {
    let desired_blob = current_blob_runtime_config(state);
    let root = hives_root();
    let hive_dir = root.join(hive_id);
    if hive_exists(&state.state_dir, hive_id) {
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

    if let Err(err) = ssh_with_pass(address, "true", BOOTSTRAP_SSH_USER) {
        return ssh_bootstrap_error_payload(&err.to_string());
    }

    if let Err(err) = fs::create_dir_all(&hive_dir) {
        return serde_json::json!({
            "status": "error",
            "error_code": "IO_ERROR",
            "message": err.to_string(),
        });
    }

    let key_path = hive_dir.join("ssh.key");
    let key_pub = hive_dir.join("ssh.key.pub");
    let mut keygen = Command::new("ssh-keygen");
    keygen
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-f")
        .arg(&key_path);
    if let Err(err) = run_cmd(keygen, "ssh-keygen") {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": err.to_string(),
        });
    }

    let mut chmod = Command::new("chmod");
    chmod.arg("600").arg(&key_path);
    if let Err(err) = run_cmd(chmod, "chmod") {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": err.to_string(),
        });
    }

    let pub_key = match fs::read_to_string(&key_pub) {
        Ok(data) => data.trim().to_string(),
        Err(err) => {
            return serde_json::json!({
                "status": "error",
                "error_code": "SSH_KEY_FAILED",
                "message": err.to_string(),
            })
        }
    };

    if let Err(err) = ssh_with_pass(
        address,
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh",
        BOOTSTRAP_SSH_USER,
    ) {
        return ssh_bootstrap_error_payload(&err.to_string());
    }

    let escaped = pub_key.replace('\'', "'\"'\"'");
    let append_cmd = format!(
        "bash -lc \"printf '%s\\n' '{}' >> ~/.ssh/authorized_keys\"",
        escaped
    );
    if let Err(err) = ssh_with_pass(address, &append_cmd, BOOTSTRAP_SSH_USER) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": err.to_string(),
        });
    }
    if let Err(err) = ssh_with_pass(
        address,
        "chmod 600 ~/.ssh/authorized_keys",
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": err.to_string(),
        });
    }

    if harden_ssh {
        if let Err(err) = disable_remote_password_auth(address) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SSH_HARDEN_FAILED",
                "message": err.to_string(),
            });
        }
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

    if let Err(err) = sync_core_to_worker(hive_id, address, &key_path) {
        return serde_json::json!({
            "status": "error",
            "error_code": "COPY_FAILED",
            "message": err.to_string(),
        });
    }

    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap(&format!(
            "mkdir -p /etc/fluxbee /var/lib/fluxbee/state/nodes /var/lib/fluxbee/opa/current /var/lib/fluxbee/opa/staged /var/lib/fluxbee/opa/backup /var/lib/fluxbee/nats /var/lib/fluxbee/runtimes /var/run/fluxbee/routers '{}' '{}'",
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
        "hive_id: {}\nrole: worker\nwan:\n  gateway_name: RT.gateway\n  uplinks:\n    - address: \"{}\"\nnats:\n  mode: embedded\n  port: 4222\nstorage:\n  path: \"{}\"\nblob:\n  enabled: {}\n  path: \"{}\"\n  sync:\n    enabled: {}\n    tool: \"{}\"\n    api_port: {}\n    data_dir: \"{}\"\n",
        hive_id,
        worker_uplink,
        storage_path,
        desired_blob.enabled,
        desired_blob.path.display(),
        desired_blob.sync_enabled,
        desired_blob.sync_tool,
        desired_blob.sync_api_port,
        desired_blob.sync_data_dir.display()
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
    ];
    if has_identity_source {
        worker_units.push(("sy-identity", "/usr/bin/sy-identity"));
    }

    for (name, exec_path) in &worker_units {
        let unit = format!(
            "[Unit]\nDescription=Fluxbee {name}\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={exec_path}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
        );
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

    for (name, _) in &worker_units {
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
        if let Err(err) = ssh_with_key(
            address,
            &key_path,
            &sudo_wrap(&format!("systemctl restart {name}")),
            BOOTSTRAP_SSH_USER,
        ) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("restart {name}: {err}"),
            });
        }
    }

    if desired_blob.sync_enabled {
        if let Err(err) = ensure_remote_syncthing_runtime(hive_id, &desired_blob) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SYNC_SETUP_FAILED",
                "message": format!("syncthing setup failed: {err}"),
            });
        }
    }

    let mut wan_connected = true;
    let mut wan_wait_error = None;
    if let Err(err) = wait_for_wan(&state.hive_id, hive_id, Duration::from_secs(60)) {
        tracing::warn!(hive_id = hive_id, error = %err, "worker WAN did not become ready in time");
        wan_connected = false;
        wan_wait_error = Some(err.to_string());
    }

    let info_path = hives_root().join(hive_id).join("info.yaml");
    let info = serde_yaml::to_string(&serde_json::json!({
        "hive_id": hive_id,
        "address": address,
        "created_at": now_epoch_ms().to_string(),
        "status": if wan_connected { "connected" } else { "pending" },
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
            "wan_connected": false,
        });
    }

    serde_json::json!({
        "status": "ok",
        "hive_id": hive_id,
        "address": address,
        "harden_ssh": harden_ssh,
        "wan_connected": true,
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{label} failed: {stderr}").into())
}

fn run_cmd_output(mut cmd: Command, label: &str) -> Result<String, OrchestratorError> {
    let output = cmd.output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{label} failed: {stderr}").into())
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

fn disable_remote_password_auth(address: &str) -> Result<(), OrchestratorError> {
    let set_password_auth_cmd = r#"bash -lc "if grep -Eq '^[[:space:]]*#?[[:space:]]*PasswordAuthentication[[:space:]]+' /etc/ssh/sshd_config; then sed -i.bak -E 's/^[[:space:]]*#?[[:space:]]*PasswordAuthentication[[:space:]]+.*/PasswordAuthentication no/' /etc/ssh/sshd_config; else printf '\nPasswordAuthentication no\n' >> /etc/ssh/sshd_config; fi""#;
    ssh_with_pass(
        address,
        &sudo_wrap(set_password_auth_cmd),
        BOOTSTRAP_SSH_USER,
    )?;

    let restart_ssh_cmd = r#"bash -lc "systemctl restart sshd || systemctl restart ssh || service sshd restart || service ssh restart""#;
    ssh_with_pass(address, &sudo_wrap(restart_ssh_cmd), BOOTSTRAP_SSH_USER)?;
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

        let active = remote_node_to_json(&node, 1000, 0).expect("active node");
        assert_eq!(active["status"], "active");

        let stale = remote_node_to_json(&node, 1000, FLAG_STALE).expect("stale node");
        assert_eq!(stale["status"], "stale");

        let deleted = remote_node_to_json(&node, 1000, FLAG_DELETED).expect("deleted node");
        assert_eq!(deleted["status"], "deleted");
    }
}
