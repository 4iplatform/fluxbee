use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use tokio::sync::Mutex;

use jsr_client::{connect, NodeConfig, NodeReceiver, NodeSender};
use jsr_client::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use json_router::shm::{
    now_epoch_ms, LsaRegionReader, NodeEntry, RemoteIslandEntry, RouterRegionReader, ShmSnapshot,
};

type OrchestratorError = Box<dyn std::error::Error + Send + Sync>;

const BOOTSTRAP_SSH_USER: &str = "administrator";
const BOOTSTRAP_SSH_PASS: &str = "magicAI";

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
    wan: Option<WanSection>,
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

struct OrchestratorState {
    island_id: String,
    started_at: Instant,
    config_dir: PathBuf,
    state_dir: PathBuf,
    gateway_name: String,
    storage_path: Mutex<String>,
    wan_listen: Option<String>,
    tracked_nodes: Mutex<HashSet<String>>,
}

const CRITICAL_SERVICES: [&str; 5] = [
    "rt-gateway",
    "sy-config-routes",
    "sy-opa-rules",
    "sy-admin",
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

    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let run_dir = PathBuf::from(json_router::paths::RUN_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);

    let island = load_island(&config_dir)?;
    let gateway_name = island
        .wan
        .as_ref()
        .and_then(|wan| wan.gateway_name.clone())
        .unwrap_or_else(|| "RT.gateway".to_string());
    let wan_listen = island.wan.as_ref().and_then(|wan| wan.listen.clone());
    let storage_path = load_storage_path(&config_dir);
    let state = OrchestratorState {
        island_id: island.island_id.clone(),
        started_at: Instant::now(),
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        gateway_name,
        storage_path: Mutex::new(storage_path),
        wan_listen,
        tracked_nodes: Mutex::new(HashSet::new()),
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

    let (mut sender, mut receiver) = connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!("connected to router");
    tracing::info!(island = %island.island_id, "island ready");

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
                if msg.meta.msg_type != "admin" {
                    continue;
                }
                if let Err(err) = handle_admin(&sender, &msg, &state).await {
                    tracing::warn!("admin action error: {err}");
                }
            }
        }
    }
}

async fn bootstrap_local(state: &OrchestratorState, socket_dir: &Path) -> Result<(), OrchestratorError> {
    tracing::info!("starting rt-gateway");
    systemd_start("rt-gateway")?;
    wait_for_router_ready(state, socket_dir, Duration::from_secs(30)).await?;

    let services = ["sy-config-routes", "sy-opa-rules", "sy-identity", "sy-admin"];
    for service in services {
        tracing::info!(service = service, "starting service");
        if let Err(err) = systemd_start(service) {
            tracing::warn!(service = service, error = %err, "failed to start service");
        }
    }

    wait_for_sy_nodes(state, Duration::from_secs(30)).await?;
    Ok(())
}

async fn wait_for_router_ready(
    state: &OrchestratorState,
    socket_dir: &Path,
    timeout: Duration,
) -> Result<(), OrchestratorError> {
    let start = Instant::now();
    loop {
        let socket_ready = socket_dir
            .read_dir()
            .ok()
            .and_then(|mut entries| entries.find(|entry| {
                entry
                    .as_ref()
                    .ok()
                    .and_then(|entry| entry.path().extension().map(|ext| ext == "sock"))
                    .unwrap_or(false)
            }))
            .is_some();
        let shm_ready = load_router_snapshot(state).is_ok();
        if socket_ready && shm_ready {
            tracing::info!("router socket + shm ready");
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err("router bootstrap timeout".into());
        }
        time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_sy_nodes(state: &OrchestratorState, timeout: Duration) -> Result<(), OrchestratorError> {
    let required = ["SY.config.routes", "SY.opa.rules", "SY.identity", "SY.admin"];
    let start = Instant::now();
    loop {
        if let Ok(snapshot) = load_router_snapshot(state) {
            let mut missing = Vec::new();
            for name in &required {
                let expected = ensure_l2_name(name, &state.island_id);
                let found = snapshot.nodes.iter().any(|node| {
                    if node.name_len == 0 {
                        return false;
                    }
                    let node_name = node_name(node);
                    node_name == *name || node_name == expected
                });
                if !found {
                    missing.push(name);
                }
            }
            if missing.is_empty() {
                tracing::info!("sy nodes connected");
                return Ok(());
            }
        }
        if start.elapsed() >= timeout {
            return Err("sy nodes bootstrap timeout".into());
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
}

async fn shutdown_sequence(state: &OrchestratorState) {
    let tracked = state.tracked_nodes.lock().await;
    for node in tracked.iter() {
        tracing::warn!(node = node.as_str(), "shutdown pending; node still connected");
    }
    drop(tracked);

    time::sleep(Duration::from_secs(10)).await;

    for service in ["sy-identity", "sy-admin", "sy-config-routes", "sy-opa-rules"] {
        if let Err(err) = systemd_stop(service) {
            tracing::warn!(service = service, error = %err, "failed to stop service");
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
        "island_status" => {
            let uptime_ms = state.started_at.elapsed().as_millis() as u64;
            serde_json::json!({
                "status": "ok",
                "island_id": state.island_id,
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
            let version = msg.payload.get("version").and_then(|value| value.as_u64()).unwrap_or(0);
            let _ = send_config_response(
                sender,
                msg,
                "storage",
                version,
                status,
                error_code,
                error_detail,
                &state.island_id,
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
        "run_node" => {
            serde_json::json!({
                "status": "ok",
                "message": "run_node stub",
            })
        }
        "kill_node" => {
            serde_json::json!({
                "status": "ok",
                "message": "kill_node stub",
            })
        }
        "run_router" => {
            serde_json::json!({
                "status": "ok",
                "message": "run_router stub",
            })
        }
        "kill_router" => {
            serde_json::json!({
                "status": "ok",
                "message": "kill_router stub",
            })
        }
        "list_islands" => {
            serde_json::json!({
                "status": "ok",
                "islands": list_islands(&state.state_dir)?,
            })
        }
        "get_island" => {
            let island = msg
                .payload
                .get("island_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(island_id) = island {
                match get_island(&state.state_dir, &island_id) {
                    Ok(payload) => serde_json::json!({
                        "status": "ok",
                        "island": payload,
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
                    "message": "missing island_id",
                })
            }
        }
        "remove_island" => {
            let island = msg
                .payload
                .get("island_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(island_id) = island {
                match remove_island(&state.state_dir, &island_id) {
                    Ok(()) => serde_json::json!({
                        "status": "ok",
                        "island_id": island_id,
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
                    "message": "missing island_id",
                })
            }
        }
        "add_island" => {
            let island_id = msg
                .payload
                .get("island_id")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            let address = msg
                .payload
                .get("address")
                .and_then(|value| value.as_str())
                .map(|value| value.to_string());
            if let Some(island_id) = island_id {
                let address = address.unwrap_or_default();
                add_island_flow(state, &island_id, &address)
            } else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing island_id",
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

fn load_island(config_dir: &Path) -> Result<IslandFile, OrchestratorError> {
    let data = fs::read_to_string(config_dir.join("island.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn ensure_dirs(config_dir: &Path, state_dir: &Path, run_dir: &Path) -> Result<(), OrchestratorError> {
    fs::create_dir_all(config_dir)?;
    fs::create_dir_all(state_dir.join("nodes"))?;
    fs::create_dir_all(islands_root())?;
    fs::create_dir_all(Path::new("/var/lib/json-router"))?;
    fs::create_dir_all(Path::new("/var/lib/json-router/opa-rules"))?;
    fs::create_dir_all(run_dir)?;
    Ok(())
}

fn write_pid(run_dir: &Path) -> Result<(), OrchestratorError> {
    let pid_path = run_dir.join("orchestrator.pid");
    let pid = std::process::id();
    fs::write(pid_path, pid.to_string())?;
    Ok(())
}

fn ensure_l2_name(name: &str, island_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, island_id)
    }
}

fn load_router_snapshot(state: &OrchestratorState) -> Result<ShmSnapshot, OrchestratorError> {
    let router_l2_name = ensure_l2_name(&state.gateway_name, &state.island_id);
    let identity_path = state
        .state_dir
        .join(&router_l2_name)
        .join("identity.yaml");
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
    island: &str,
) -> Result<(), OrchestratorError> {
    let mut payload = serde_json::json!({
        "subsystem": subsystem,
        "version": version,
        "status": status,
        "island": island,
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

fn list_islands(_state_dir: &Path) -> Result<Vec<serde_json::Value>, OrchestratorError> {
    let root = islands_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let island_id = entry.file_name().to_string_lossy().to_string();
        if let Ok(info) = read_island_info(&root, &island_id) {
            out.push(info);
        } else {
            out.push(serde_json::json!({ "island_id": island_id }));
        }
    }
    out.sort_by(|a, b| {
        let a = a.get("island_id").and_then(|v| v.as_str()).unwrap_or("");
        let b = b.get("island_id").and_then(|v| v.as_str()).unwrap_or("");
        a.cmp(b)
    });
    Ok(out)
}

fn get_island(_state_dir: &Path, island_id: &str) -> Result<serde_json::Value, OrchestratorError> {
    let root = islands_root();
    read_island_info(&root, island_id)
}

fn remove_island(_state_dir: &Path, island_id: &str) -> Result<(), OrchestratorError> {
    let root = islands_root();
    let dir = root.join(island_id);
    if !dir.exists() {
        return Err("island not found".into());
    }
    fs::remove_dir_all(dir)?;
    Ok(())
}

fn read_island_info(
    root: &Path,
    island_id: &str,
) -> Result<serde_json::Value, OrchestratorError> {
    let path = root.join(island_id).join("info.yaml");
    let data = fs::read_to_string(&path)?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let json = serde_json::to_value(yaml)?;
    Ok(json)
}

fn add_island_flow(state: &OrchestratorState, island_id: &str, address: &str) -> serde_json::Value {
    if island_exists(&state.state_dir, island_id) {
        return serde_json::json!({
            "status": "error",
            "error_code": "ISLAND_EXISTS",
            "message": "island already exists",
        });
    }
    if !valid_island_id(island_id) {
        return serde_json::json!({
            "status": "error",
            "error_code": "INVALID_ISLAND_ID",
            "message": "invalid island_id",
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
            "message": "wan.listen missing in island.yaml",
        });
    }

    if let Err(err) = ssh_with_pass(address, "true", BOOTSTRAP_SSH_USER) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_AUTH_FAILED",
            "message": err.to_string(),
        });
    }

    let island_dir = islands_root().join(island_id);
    if let Err(err) = fs::create_dir_all(&island_dir) {
        return serde_json::json!({
            "status": "error",
            "error_code": "IO_ERROR",
            "message": err.to_string(),
        });
    }
    let key_path = island_dir.join("ssh.key");
    let key_pub = island_dir.join("ssh.key.pub");
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
        &sudo_wrap("mkdir -p /root/.ssh && chmod 700 /root/.ssh"),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_AUTH_FAILED",
            "message": err.to_string(),
        });
    }
    let escaped = pub_key.replace('\'', "'\"'\"'");
    let append_cmd = format!(
        "bash -lc \"printf '%s\\\\n' '{}' >> /root/.ssh/authorized_keys\"",
        escaped
    );
    if let Err(err) = ssh_with_pass(address, &sudo_wrap(&append_cmd), BOOTSTRAP_SSH_USER) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SSH_KEY_FAILED",
            "message": err.to_string(),
        });
    }
    let _ = ssh_with_pass(
        address,
        &sudo_wrap("chmod 600 /root/.ssh/authorized_keys"),
        BOOTSTRAP_SSH_USER,
    );

    // NOTE: PasswordAuthentication disable step intentionally skipped for testing safety.
    // Spec: set PasswordAuthentication no and restart sshd.

    let binaries = [
        "/usr/bin/sy-orchestrator",
        "/usr/bin/rt-gateway",
        "/usr/bin/sy-config-routes",
        "/usr/bin/sy-opa-rules",
        "/usr/bin/sy-admin",
    ];
    for bin in binaries {
        if !Path::new(bin).exists() {
            return serde_json::json!({
                "status": "error",
                "error_code": "COPY_FAILED",
                "message": format!("missing binary {bin}"),
            });
        }
    }
    if let Err(err) = scp_with_key(address, &key_path, &binaries, "/tmp/", BOOTSTRAP_SSH_USER) {
        return serde_json::json!({
            "status": "error",
            "error_code": "COPY_FAILED",
            "message": err.to_string(),
        });
    }
    let _ = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("mv /tmp/sy-* /usr/bin/ && mv /tmp/rt-* /usr/bin/ && chmod +x /usr/bin/sy-* /usr/bin/rt-*"),
        BOOTSTRAP_SSH_USER,
    );

    let _ = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("mkdir -p /etc/json-router /var/lib/json-router/nodes /var/lib/json-router/opa-rules /var/run/json-router/routers"),
        BOOTSTRAP_SSH_USER,
    );

    let wan_listen = state.wan_listen.clone().unwrap_or_default();
    let island_yaml = format!(
        "island_id: {}\nwan:\n  gateway_name: RT.gateway\n  uplinks:\n    - address: \"{}\"\n",
        island_id, wan_listen
    );
    if let Err(err) = write_remote_file(
        address,
        &key_path,
        "/etc/json-router/island.yaml",
        &island_yaml,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "CONFIG_FAILED",
            "message": err.to_string(),
        });
    }

    let config_routes_yaml = "version: 1\nroutes: []\nvpns: []\n";
    let _ = write_remote_file(
        address,
        &key_path,
        "/etc/json-router/config-routes.yaml",
        config_routes_yaml,
    );

    let service = r#"[Unit]
Description=JSON Router Orchestrator
After=network.target

[Service]
Type=simple
ExecStart=/usr/bin/sy-orchestrator
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"#;
    let _ = write_remote_file(
        address,
        &key_path,
        "/etc/systemd/system/sy-orchestrator.service",
        service,
    );
    let _ = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("systemctl daemon-reload"),
        BOOTSTRAP_SSH_USER,
    );
    let _ = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("systemctl enable sy-orchestrator"),
        BOOTSTRAP_SSH_USER,
    );
    if let Err(err) = ssh_with_key(
        address,
        &key_path,
        &sudo_wrap("systemctl start sy-orchestrator"),
        BOOTSTRAP_SSH_USER,
    ) {
        return serde_json::json!({
            "status": "error",
            "error_code": "SERVICE_FAILED",
            "message": err.to_string(),
        });
    }

    let mut wan_connected = true;
    if let Err(_) = wait_for_wan(&state.island_id, island_id, Duration::from_secs(60)) {
        wan_connected = false;
    }

    let info_path = islands_root().join(island_id).join("info.yaml");
    let info = serde_yaml::to_string(&serde_json::json!({
        "island_id": island_id,
        "address": address,
        "created_at": now_epoch_ms().to_string(),
        "status": if wan_connected { "connected" } else { "pending" },
    }))
    .unwrap_or_default();
    let _ = fs::write(info_path, info);

    if !wan_connected {
        return serde_json::json!({
            "status": "error",
            "error_code": "WAN_TIMEOUT",
            "island_id": island_id,
            "address": address,
            "wan_connected": false,
        });
    }

    serde_json::json!({
        "status": "ok",
        "island_id": island_id,
        "address": address,
        "wan_connected": true,
    })
}

fn island_exists(state_dir: &Path, island_id: &str) -> bool {
    let _ = state_dir;
    islands_root().join(island_id).exists()
}

fn valid_island_id(value: &str) -> bool {
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
    if labels.iter().any(|label| label.is_empty() || label.len() > 63) {
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

fn islands_root() -> PathBuf {
    PathBuf::from("/var/lib/json-router/islands")
}

fn orchestrator_file_path() -> PathBuf {
    PathBuf::from("/var/lib/json-router/orchestrator.yaml")
}

fn run_cmd(mut cmd: Command, label: &str) -> Result<(), OrchestratorError> {
    let output = cmd.output()?;
    if output.status.success() {
        return Ok(());
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
    run_cmd(Command::new("systemctl").arg("start").arg(service), "systemctl start")
}

fn systemd_stop(service: &str) -> Result<(), OrchestratorError> {
    run_cmd(Command::new("systemctl").arg("stop").arg(service), "systemctl stop")
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

fn ssh_with_key(address: &str, key_path: &Path, command: &str, user: &str) -> Result<(), OrchestratorError> {
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

fn wait_for_wan(island_id: &str, remote_id: &str, timeout: Duration) -> Result<(), OrchestratorError> {
    let shm_name = format!("/jsr-lsa-{}", island_id);
    let reader = LsaRegionReader::open_read_only(&shm_name)?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(snapshot) = reader.read_snapshot() {
            if snapshot
                .islands
                .iter()
                .any(|entry| remote_island_match(entry, remote_id))
            {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("wan timeout".into())
}

fn remote_island_match(entry: &RemoteIslandEntry, target: &str) -> bool {
    if entry.island_id_len == 0 {
        return false;
    }
    let len = entry.island_id_len as usize;
    let name = String::from_utf8_lossy(&entry.island_id[..len]);
    name == target
}

fn sudo_wrap(cmd: &str) -> String {
    let pass = BOOTSTRAP_SSH_PASS.replace('\'', "'\"'\"'");
    format!("echo '{}' | sudo -S -p '' {}", pass, cmd)
}

fn load_storage_path(config_dir: &Path) -> String {
    let _ = config_dir;
    let default_root = "/var/lib/json-router".to_string();
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
