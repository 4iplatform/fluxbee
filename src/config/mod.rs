use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use uuid::Uuid;

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router/state";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router";

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
    pub node_socket_path: PathBuf,
    pub shm_name: String,
    pub shm_prefix: String,
    pub is_gateway: bool,
}

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
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
        let config_dir = PathBuf::from(DEFAULT_CONFIG_DIR);
        let state_dir = PathBuf::from(DEFAULT_STATE_DIR);
        let socket_dir = PathBuf::from(DEFAULT_SOCKET_DIR);
        let node_socket_path = socket_dir.join("router.sock");
        let shm_prefix = "/jsr-".to_string();
        let island_path = config_dir.join("island.yaml");
        if !island_path.exists() {
            return Err(ConfigError::MissingIsland);
        }
        let data = std::fs::read_to_string(&island_path)?;
        let island: IslandFile = serde_yaml::from_str(&data)?;
        let router_l2_name = ensure_l2_name(&router_name, &island.island_id);
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
        Ok(Self {
            router_name,
            router_l2_name,
            router_uuid,
            island_id: island.island_id,
            config_dir,
            state_dir,
            node_socket_path,
            shm_name,
            shm_prefix,
            is_gateway: false,
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
