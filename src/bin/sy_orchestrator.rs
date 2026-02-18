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

use json_router::shm::{
    now_epoch_ms, LsaRegionReader, NodeEntry, RemoteHiveEntry, RouterRegionReader, ShmSnapshot,
};
use jsr_client::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use jsr_client::{connect, NodeConfig, NodeReceiver, NodeSender};

type OrchestratorError = Box<dyn std::error::Error + Send + Sync>;

const BOOTSTRAP_SSH_USER: &str = "administrator";
const BOOTSTRAP_SSH_PASS: &str = "magicAI";
const RUNTIME_VERIFY_INTERVAL_SECS: u64 = 300;
const RUNTIME_MANIFEST_FILE: &str = "runtime-manifest.json";
const NATS_BOOTSTRAP_TIMEOUT_SECS: u64 = 20;
const STORAGE_BOOTSTRAP_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    nats: Option<NatsSection>,
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
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    mode: Option<String>,
    port: Option<u16>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrchestratorFile {
    #[serde(default)]
    storage: Option<StorageSection>,
    #[serde(default)]
    storage_path: Option<String>,
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

struct OrchestratorState {
    hive_id: String,
    started_at: Instant,
    config_dir: PathBuf,
    state_dir: PathBuf,
    gateway_name: String,
    storage_path: Mutex<String>,
    wan_listen: Option<String>,
    tracked_nodes: Mutex<HashSet<String>>,
    runtime_manifest: Mutex<Option<RuntimeManifest>>,
    last_runtime_verify: Mutex<Instant>,
    nats_endpoint: String,
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
    let nats_endpoint = nats_endpoint_from_hive(&hive);
    let storage_path = load_storage_path(&config_dir);
    let runtime_manifest = load_runtime_manifest();
    let state = OrchestratorState {
        hive_id: hive.hive_id.clone(),
        started_at: Instant::now(),
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        gateway_name,
        storage_path: Mutex::new(storage_path),
        wan_listen,
        tracked_nodes: Mutex::new(HashSet::new()),
        runtime_manifest: Mutex::new(runtime_manifest),
        last_runtime_verify: Mutex::new(Instant::now()),
        nats_endpoint,
    };
    ensure_dirs(&config_dir, &state_dir, &run_dir)?;
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
    tracing::info!("starting rt-gateway");
    systemd_start("rt-gateway")?;
    wait_for_router_ready(state, socket_dir, Duration::from_secs(30)).await?;
    wait_for_nats_ready(
        &state.nats_endpoint,
        Duration::from_secs(NATS_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;

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

    wait_for_sy_nodes(state, Duration::from_secs(30)).await?;
    wait_for_service_active(
        "sy-storage",
        Duration::from_secs(STORAGE_BOOTSTRAP_TIMEOUT_SECS),
    )
    .await?;
    Ok(())
}

async fn wait_for_router_ready(
    state: &OrchestratorState,
    socket_dir: &Path,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    const ROUTER_SHM_MAX_STALE_MS: u64 = 30_000;

    let start = Instant::now();
    let socket_age_limit = timeout + Duration::from_secs(10);
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
            socket_ready =
                expected_socket.exists() && socket_file_recent(&expected_socket, socket_age_limit);

            heartbeat_age_ms = now_epoch_ms().saturating_sub(snapshot.header.heartbeat);
            shm_ready = heartbeat_age_ms <= ROUTER_SHM_MAX_STALE_MS;
        }

        if rt_gateway_active && socket_ready && shm_ready {
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

fn socket_file_recent(path: &Path, max_age: Duration) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|age| age <= max_age)
        .unwrap_or(false)
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
}

async fn handle_admin(
    sender: &NodeSender,
    msg: &Message,
    state: &OrchestratorState,
) -> Result<(), OrchestratorError> {
    let action = msg.meta.action.as_deref().unwrap_or("");
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
                match persist_storage_path(&state.config_dir, &path) {
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
        "list_nodes" => match load_router_snapshot(state) {
            Ok(snapshot) => serde_json::json!({
                "status": "ok",
                "nodes": nodes_from_snapshot(&snapshot),
            }),
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "SHM_NOT_FOUND",
                "message": err.to_string(),
            }),
        },
        "list_routers" => match load_router_snapshot(state) {
            Ok(snapshot) => serde_json::json!({
                "status": "ok",
                "routers": routers_from_snapshot(&snapshot),
            }),
            Err(err) => serde_json::json!({
                "status": "error",
                "error_code": "SHM_NOT_FOUND",
                "message": err.to_string(),
            }),
        },
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
                match remove_hive(&state.state_dir, &hive_id) {
                    Ok(()) => serde_json::json!({
                        "status": "ok",
                        "hive_id": hive_id,
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

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), jsr_client::NodeError> {
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
    fs::create_dir_all(runtimes_root())?;
    fs::create_dir_all(orchestrator_runtime_dir())?;
    fs::create_dir_all(run_dir)?;
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

fn remove_hive(_state_dir: &Path, hive_id: &str) -> Result<(), OrchestratorError> {
    let root = hives_root();
    let dir = root.join(hive_id);
    if !dir.exists() {
        return Err("hive not found".into());
    }
    fs::remove_dir_all(dir)?;
    Ok(())
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

    let Some(manifest) = manifest else {
        return Ok(());
    };

    {
        let mut guard = state.runtime_manifest.lock().await;
        *guard = Some(manifest.clone());
    }

    runtime_sync_workers(state, &manifest).await
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

fn sync_runtime_to_worker(
    hive_id: &str,
    address: &str,
    key_path: &Path,
) -> Result<(), OrchestratorError> {
    let local_root = runtimes_root();
    if !local_root.exists() {
        return Ok(());
    }

    ssh_with_key(
        address,
        key_path,
        &sudo_wrap("mkdir -p /var/lib/fluxbee/runtimes"),
        BOOTSTRAP_SSH_USER,
    )?;

    let mut cmd = Command::new("rsync");
    cmd.arg("-az")
        .arg("--delete")
        .arg("-e")
        .arg(format!(
            "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10",
            key_path.display()
        ))
        .arg(format!("{}/", local_root.display()))
        .arg(format!(
            "{}@{}:/var/lib/fluxbee/runtimes/",
            BOOTSTRAP_SSH_USER, address
        ));
    run_cmd(cmd, &format!("rsync runtime ({hive_id})"))
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

fn add_hive_flow(
    state: &OrchestratorState,
    hive_id: &str,
    address: &str,
    harden_ssh: bool,
) -> serde_json::Value {
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

    let required_binaries = [
        "/usr/bin/rt-gateway",
        "/usr/bin/sy-config-routes",
        "/usr/bin/sy-opa-rules",
    ];
    for bin in required_binaries {
        if !Path::new(bin).exists() {
            return serde_json::json!({
                "status": "error",
                "error_code": "COPY_FAILED",
                "message": format!("missing binary {bin}"),
            });
        }
    }

    let mut binaries = vec![
        "/usr/bin/rt-gateway",
        "/usr/bin/sy-config-routes",
        "/usr/bin/sy-opa-rules",
    ];
    if identity_available() {
        binaries.push("/usr/bin/sy-identity");
    }
    if let Err(err) = scp_with_key(address, &key_path, &binaries, "/tmp/", BOOTSTRAP_SSH_USER) {
        return serde_json::json!({
            "status": "error",
            "error_code": "COPY_FAILED",
            "message": err.to_string(),
        });
    }

    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("mv /tmp/sy-* /usr/bin/ && mv /tmp/rt-* /usr/bin/ && chmod +x /usr/bin/sy-* /usr/bin/rt-*"),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "COPY_FAILED",
            "message": err.to_string(),
        });
    }

    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("mkdir -p /etc/fluxbee /var/lib/fluxbee/state/nodes /var/lib/fluxbee/opa/current /var/lib/fluxbee/opa/staged /var/lib/fluxbee/opa/backup /var/lib/fluxbee/nats /var/lib/fluxbee/runtimes /var/run/fluxbee/routers"),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_FAILED",
            "message": err.to_string(),
        });
    }

    let wan_listen = state.wan_listen.clone().unwrap_or_default();
    let hive_yaml = format!(
        "hive_id: {}\nrole: worker\nwan:\n  gateway_name: RT.gateway\n  uplinks:\n    - address: \"{}\"\nnats:\n  mode: embedded\n  port: 4222\n",
        hive_id, wan_listen
    );
    if let Err(err) = write_remote_file(address, &key_path, "/etc/fluxbee/hive.yaml", &hive_yaml) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_FAILED",
            "message": err.to_string(),
        });
    }

    let config_routes_yaml = "version: 1\nroutes: []\nvpns: []\n";
    if let Err(err) = write_remote_file(
        address,
        &key_path,
        "/etc/fluxbee/sy-config-routes.yaml",
        config_routes_yaml,
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
    if identity_available() {
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
            &sudo_wrap(&format!("systemctl start {name}")),
            BOOTSTRAP_SSH_USER,
        ) {
            return serde_json::json!({
                "status": "error",
                "error_code": "SERVICE_FAILED",
                "message": format!("start {name}: {err}"),
            });
        }
    }

    let mut wan_connected = true;
    if wait_for_wan(&state.hive_id, hive_id, Duration::from_secs(60)).is_err() {
        wan_connected = false;
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
        return serde_json::json!({
            "status": "error",
            "error_code": "WAN_TIMEOUT",
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

fn orchestrator_file_path() -> PathBuf {
    json_router::paths::storage_root_dir().join("orchestrator.yaml")
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
    while Instant::now() < deadline {
        if let Some(snapshot) = reader.read_snapshot() {
            if snapshot
                .hives
                .iter()
                .any(|entry| remote_hive_match(entry, remote_id))
            {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("wan timeout".into())
}

fn remote_hive_match(entry: &RemoteHiveEntry, target: &str) -> bool {
    if entry.hive_id_len == 0 {
        return false;
    }
    let len = entry.hive_id_len as usize;
    let name = String::from_utf8_lossy(&entry.hive_id[..len]);
    name == target
}

fn sudo_wrap(cmd: &str) -> String {
    let pass = BOOTSTRAP_SSH_PASS.replace('\'', "'\"'\"'");
    format!("echo '{}' | sudo -S -p '' {}", pass, cmd)
}

fn load_storage_path(config_dir: &Path) -> String {
    let _ = config_dir;
    let default_root = json_router::paths::storage_root_dir()
        .to_string_lossy()
        .to_string();
    let path = orchestrator_file_path();
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(_) => return default_root,
    };
    let parsed: OrchestratorFile = match serde_yaml::from_str(&data) {
        Ok(parsed) => parsed,
        Err(_) => return default_root,
    };
    if let Some(path) = parsed
        .storage
        .and_then(|storage| storage.path)
        .or(parsed.storage_path)
    {
        if !path.trim().is_empty() {
            return path;
        }
    }
    default_root
}

fn persist_storage_path(config_dir: &Path, path: &str) -> Result<(), OrchestratorError> {
    let _ = config_dir;
    let file_path = orchestrator_file_path();
    let data = serde_yaml::to_string(&serde_json::json!({
        "storage": { "path": path }
    }))?;
    fs::write(file_path, data)?;
    Ok(())
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee" || r == "mother")
}
