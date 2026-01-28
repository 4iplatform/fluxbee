use std::fs;
use std::path::{Path, PathBuf};

use jsr_client::{NodeClient, NodeConfig};
use jsr_client::protocol::{Destination, Message, Meta, OpaReloadPayload, Routing, MSG_OPA_RELOAD, SYSTEM_KIND};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::{self, Duration};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const POLICY_PATH: &str = "/var/lib/json-router/policy.wasm";
const STATE_PATH: &str = "/var/lib/json-router/opa-rules/state.yaml";

#[derive(Debug, Serialize, Deserialize)]
struct OpaState {
    version: u64,
    last_mtime_ms: Option<u64>,
}

impl Default for OpaState {
    fn default() -> Self {
        Self {
            version: 0,
            last_mtime_ms: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_opa_rules supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);

    ensure_dir(&state_dir)?;
    ensure_dir(&PathBuf::from("/var/lib/json-router/opa-rules"))?;

    let island = load_island(&config_dir)?;
    let router_socket = match std::env::var("JSR_ROUTER_NAME") {
        Ok(router_name) => {
            let router_l2_name = ensure_l2_name(&router_name, &island.island_id);
            match load_router_uuid(&state_dir, &router_l2_name) {
                Ok(router_uuid) => socket_dir.join(format!("{}.sock", router_uuid.simple())),
                Err(_) => socket_dir.clone(),
            }
        }
        Err(_) => socket_dir.clone(),
    };

    let node_uuid = load_or_create_uuid(&state_dir.join("nodes"), "SY.opa.rules")?;
    let node_config = NodeConfig {
        name: "SY.opa.rules".to_string(),
        router_socket: router_socket.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let mut client = NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!("connected to router");

    let mut state = load_state()?;
    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Some(mtime_ms) = policy_mtime_ms() {
                    if state.last_mtime_ms != Some(mtime_ms) {
                        state.version = state.version.saturating_add(1);
                        state.last_mtime_ms = Some(mtime_ms);
                        save_state(&state)?;
                        send_reload(&mut client, state.version).await?;
                        tracing::info!(version = state.version, "opa reload broadcast sent");
                    }
                } else if state.last_mtime_ms.is_none() {
                    tracing::warn!("opa policy.wasm not found; waiting");
                }
            }
            msg = client.recv() => {
                match msg {
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!("recv error: {err} (reconnecting)");
                        client = NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await?;
                        tracing::info!("reconnected to router");
                    }
                }
            }
        }
    }
}

async fn send_reload(client: &mut NodeClient, version: u64) -> Result<(), Box<dyn std::error::Error>> {
    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_OPA_RELOAD.to_string()),
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload: json!(OpaReloadPayload {
            version,
            hash: None,
        }),
    };
    client.send(&msg).await?;
    Ok(())
}

fn policy_mtime_ms() -> Option<u64> {
    let meta = fs::metadata(POLICY_PATH).ok()?;
    let mtime = meta.modified().ok()?;
    Some(
        mtime
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    )
}

fn load_state() -> Result<OpaState, Box<dyn std::error::Error>> {
    let path = PathBuf::from(STATE_PATH);
    if !path.exists() {
        return Ok(OpaState::default());
    }
    let data = fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&data)?)
}

fn save_state(state: &OpaState) -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(STATE_PATH);
    let data = serde_yaml::to_string(state)?;
    fs::write(path, data)?;
    Ok(())
}

fn load_island(config_dir: &Path) -> Result<IslandFile, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(config_dir.join("island.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn ensure_l2_name(name: &str, island_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, island_id)
    }
}

fn load_router_uuid(state_dir: &Path, router_l2_name: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    let identity_path = state_dir.join(router_l2_name).join("identity.yaml");
    let data = fs::read_to_string(identity_path)?;
    let value: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let uuid = value
        .get("layer1")
        .and_then(|v| v.get("uuid"))
        .and_then(|v| v.as_str())
        .ok_or("missing layer1.uuid")?;
    Ok(Uuid::parse_str(uuid)?)
}

fn load_or_create_uuid(
    nodes_dir: &Path,
    name: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    ensure_dir(nodes_dir)?;
    let path = nodes_dir.join(format!("{name}.uuid"));
    if path.exists() {
        let data = fs::read_to_string(&path)?;
        return Ok(Uuid::parse_str(data.trim())?);
    }
    let uuid = Uuid::new_v4();
    fs::write(&path, uuid.to_string())?;
    Ok(uuid)
}

fn ensure_dir(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(path)?;
    Ok(())
}
