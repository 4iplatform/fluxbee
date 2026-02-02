use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use jsr_client::{connect, NodeConfig, NodeReceiver, NodeSender};
use jsr_client::protocol::{Destination, Message, Meta, Routing};
use json_router::shm::{NodeEntry, RouterRegionReader, ShmSnapshot};

type OrchestratorError = Box<dyn std::error::Error + Send + Sync>;

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
}

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
    let state = OrchestratorState {
        island_id: island.island_id.clone(),
        started_at: Instant::now(),
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        gateway_name,
    };
    ensure_dirs(&config_dir, &state_dir, &run_dir)?;
    write_pid(&run_dir)?;

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

    let mut watchdog = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = watchdog.tick() => {
                // Placeholder for watchdog checks (RT.gateway, SY.*)
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
            let path = load_storage_path(&state.config_dir);
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
            let Some(path) = path else {
                serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_REQUEST",
                    "message": "missing path",
                })
            } else {
                if let Err(err) = persist_storage_path(&state.config_dir, &path) {
                    serde_json::json!({
                        "status": "error",
                        "error_code": "PERSIST_FAILED",
                        "message": err.to_string(),
                    })
                } else {
                    serde_json::json!({
                        "status": "ok",
                        "path": path,
                    })
                }
            }
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
    fs::create_dir_all(state_dir.join("islands"))?;
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

fn load_storage_path(config_dir: &Path) -> String {
    let default_root = "/var/lib/json-router".to_string();
    let path = config_dir.join("orchestrator.yaml");
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
    let file_path = config_dir.join("orchestrator.yaml");
    let data = serde_yaml::to_string(&serde_json::json!({
        "storage": { "path": path }
    }))?;
    fs::write(file_path, data)?;
    Ok(())
}
