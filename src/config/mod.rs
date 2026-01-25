use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use uuid::Uuid;

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router/state";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router/routers";
const DEFAULT_SHM_PREFIX: &str = "/jsr-";
const DEFAULT_HELLO_INTERVAL_MS: u64 = 10_000;
const DEFAULT_DEAD_INTERVAL_MS: u64 = 40_000;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_HEARTBEAT_STALE_MS: u64 = 30_000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing island.yaml")]
    MissingIsland,
    #[error("missing router name (JSR_ROUTER_NAME)")]
    MissingRouterName,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
}

#[derive(Clone, Debug)]
pub struct RouterConfig {
    pub router_name: String,
    pub router_l2_name: String,
    pub router_uuid: Uuid,
    pub island_id: String,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub node_socket_dir: PathBuf,
    pub node_socket_path: PathBuf,
    pub shm_name: String,
    pub shm_prefix: String,
    pub is_gateway: bool,
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub heartbeat_stale_ms: u64,
    pub wan_listen: Option<String>,
    pub wan_uplinks: Vec<String>,
    pub wan_authorized_islands: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
    wan: Option<WanSection>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    listen: Option<String>,
    uplinks: Option<Vec<WanUplink>>,
    authorized_islands: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct WanUplink {
    address: String,
}

#[derive(Debug, Deserialize)]
struct IdentityFile {
    layer1: IdentityLayer1,
    layer2: IdentityLayer2,
    shm: IdentityShm,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct IdentityLayer1 {
    uuid: String,
}

#[derive(Debug, Deserialize)]
struct IdentityLayer2 {
    name: String,
}

#[derive(Debug, Deserialize)]
struct IdentityShm {
    name: String,
}

impl RouterConfig {
    pub fn load_from_env() -> Result<Self, ConfigError> {
        let router_name = std::env::var("JSR_ROUTER_NAME")
            .map_err(|_| ConfigError::MissingRouterName)?;
        let config_dir = PathBuf::from(
            std::env::var("JSR_CONFIG_DIR").unwrap_or_else(|_| DEFAULT_CONFIG_DIR.to_string()),
        );

        let mut state_dir = PathBuf::from(DEFAULT_STATE_DIR);
        let mut node_socket_dir = PathBuf::from(DEFAULT_SOCKET_DIR);
        let mut shm_prefix = DEFAULT_SHM_PREFIX.to_string();
        let mut hello_interval_ms = DEFAULT_HELLO_INTERVAL_MS;
        let mut dead_interval_ms = DEFAULT_DEAD_INTERVAL_MS;
        let mut heartbeat_interval_ms = DEFAULT_HEARTBEAT_INTERVAL_MS;
        let mut heartbeat_stale_ms = DEFAULT_HEARTBEAT_STALE_MS;
        let mut wan_listen = None;
        let mut wan_uplinks = Vec::new();
        let mut wan_authorized_islands = Vec::new();

        let island_path = config_dir.join("island.yaml");
        if !island_path.exists() {
            return Err(ConfigError::MissingIsland);
        }
        let data = std::fs::read_to_string(&island_path)?;
        let island: IslandFile = serde_yaml::from_str(&data)?;
        let island_id = island.island_id;
        if let Some(wan) = island.wan {
            wan_listen = wan.listen;
            if let Some(uplinks) = wan.uplinks {
                for uplink in uplinks {
                    wan_uplinks.push(uplink.address);
                }
            }
            if let Some(islands) = wan.authorized_islands {
                wan_authorized_islands = islands;
            }
        }

        if let Ok(value) = std::env::var("JSR_STATE_DIR") {
            state_dir = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("JSR_SOCKET_DIR") {
            node_socket_dir = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("JSR_SHM_PREFIX") {
            shm_prefix = value;
        }

        let is_gateway = is_gateway_name(&router_name);
        let router_l2_name = ensure_l2_name(&router_name, &island_id);
        let identity_path = identity_path(&state_dir, &router_l2_name);
        let (router_uuid, shm_name) = if identity_path.exists() {
            let identity_raw = fs::read_to_string(&identity_path)?;
            let identity: IdentityFile = serde_yaml::from_str(&identity_raw)?;
            let uuid = Uuid::parse_str(&identity.layer1.uuid)?;
            (uuid, identity.shm.name)
        } else {
            fs::create_dir_all(identity_path.parent().unwrap_or(Path::new(".")))?;
            let uuid = Uuid::new_v4();
            let shm_name = crate::shm::build_shm_name(&shm_prefix, uuid);
            let identity = build_identity_yaml(&router_l2_name, uuid, &shm_name)?;
            fs::write(&identity_path, identity)?;
            (uuid, shm_name)
        };
        let node_socket_path = node_socket_dir.join(format!("{}.sock", router_uuid.simple()));
        Ok(Self {
            router_name,
            router_l2_name,
            router_uuid,
            island_id,
            config_dir,
            state_dir,
            node_socket_dir,
            node_socket_path,
            shm_name,
            shm_prefix,
            is_gateway,
            hello_interval_ms,
            dead_interval_ms,
            heartbeat_interval_ms,
            heartbeat_stale_ms,
            wan_listen,
            wan_uplinks,
            wan_authorized_islands,
        })
    }
}

fn ensure_l2_name(name: &str, island_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, island_id)
    }
}

fn identity_path(state_dir: &Path, router_l2_name: &str) -> PathBuf {
    state_dir.join(router_l2_name).join("identity.yaml")
}

fn is_gateway_name(router_name: &str) -> bool {
    let base = router_name.split('@').next().unwrap_or(router_name);
    base == "RT.gateway"
}

fn build_identity_yaml(router_l2_name: &str, uuid: Uuid, shm_name: &str) -> Result<String, ConfigError> {
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let value = serde_yaml::to_string(&serde_yaml::Value::Mapping(
        [
            (
                serde_yaml::Value::String("layer1".to_string()),
                serde_yaml::Value::Mapping(
                    [(
                        serde_yaml::Value::String("uuid".to_string()),
                        serde_yaml::Value::String(uuid.to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                serde_yaml::Value::String("layer2".to_string()),
                serde_yaml::Value::Mapping(
                    [(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(router_l2_name.to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                serde_yaml::Value::String("shm".to_string()),
                serde_yaml::Value::Mapping(
                    [(
                        serde_yaml::Value::String("name".to_string()),
                        serde_yaml::Value::String(shm_name.to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            ),
            (
                serde_yaml::Value::String("created_at".to_string()),
                serde_yaml::Value::String(created_at.to_string()),
            ),
        ]
        .into_iter()
        .collect(),
    ))?;
    Ok(value)
}
