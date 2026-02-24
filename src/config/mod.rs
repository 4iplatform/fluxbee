use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use uuid::Uuid;

const DEFAULT_SHM_PREFIX: &str = "/jsr-";
const DEFAULT_HELLO_INTERVAL_MS: u64 = 10_000;
const DEFAULT_DEAD_INTERVAL_MS: u64 = 40_000;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
const DEFAULT_HEARTBEAT_STALE_MS: u64 = 30_000;
const DEFAULT_HIVE_ROLE: &str = "worker";
const DEFAULT_NATS_MODE: &str = "embedded";
const DEFAULT_NATS_PORT: u16 = 4222;
const DEFAULT_NATS_STORAGE_DIR: &str = "/var/lib/fluxbee/nats";
const DEFAULT_BLOB_ENABLED: bool = true;
const DEFAULT_BLOB_PATH: &str = "/var/lib/fluxbee/blob";
const DEFAULT_BLOB_SYNC_ENABLED: bool = false;
const DEFAULT_BLOB_SYNC_TOOL: &str = "syncthing";
const DEFAULT_BLOB_SYNC_API_PORT: u16 = 8384;
const DEFAULT_BLOB_SYNC_DATA_DIR: &str = "/var/lib/fluxbee/syncthing";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing hive.yaml")]
    MissingHive,
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
    pub hive_id: String,
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
    pub wan_authorized_hives: Vec<String>,
    pub hive_role: String,
    pub nats_mode: String,
    pub nats_port: u16,
    pub nats_url: String,
    pub nats_storage_dir: PathBuf,
    pub blob_enabled: bool,
    pub blob_path: PathBuf,
    pub blob_sync_enabled: bool,
    pub blob_sync_tool: String,
    pub blob_sync_api_port: u16,
    pub blob_sync_data_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    wan: Option<WanSection>,
    nats: Option<NatsSection>,
    blob: Option<BlobSection>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    listen: Option<String>,
    uplinks: Option<Vec<WanUplink>>,
    authorized_hives: Option<Vec<String>>,
    gateway_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanUplink {
    address: String,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    mode: Option<String>,
    port: Option<u16>,
    url: Option<String>,
    storage_dir: Option<String>,
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
        let config_dir = crate::paths::config_dir();
        let state_dir = crate::paths::state_dir();
        let node_socket_dir = crate::paths::router_socket_dir();
        let shm_prefix = DEFAULT_SHM_PREFIX.to_string();
        let hello_interval_ms = DEFAULT_HELLO_INTERVAL_MS;
        let dead_interval_ms = DEFAULT_DEAD_INTERVAL_MS;
        let heartbeat_interval_ms = DEFAULT_HEARTBEAT_INTERVAL_MS;
        let heartbeat_stale_ms = DEFAULT_HEARTBEAT_STALE_MS;
        let mut wan_listen = None;
        let mut wan_uplinks = Vec::new();
        let mut wan_authorized_hives = Vec::new();
        let mut hive_role = DEFAULT_HIVE_ROLE.to_string();
        let mut nats_mode = DEFAULT_NATS_MODE.to_string();
        let mut nats_port = DEFAULT_NATS_PORT;
        let mut nats_url = String::new();
        let mut nats_storage_dir = PathBuf::from(DEFAULT_NATS_STORAGE_DIR);
        let mut blob_enabled = DEFAULT_BLOB_ENABLED;
        let mut blob_path = PathBuf::from(DEFAULT_BLOB_PATH);
        let mut blob_sync_enabled = DEFAULT_BLOB_SYNC_ENABLED;
        let mut blob_sync_tool = DEFAULT_BLOB_SYNC_TOOL.to_string();
        let mut blob_sync_api_port = DEFAULT_BLOB_SYNC_API_PORT;
        let mut blob_sync_data_dir = PathBuf::from(DEFAULT_BLOB_SYNC_DATA_DIR);

        let hive_path = config_dir.join("hive.yaml");
        if !hive_path.exists() {
            return Err(ConfigError::MissingHive);
        }
        let data = std::fs::read_to_string(&hive_path)?;
        let hive: HiveFile = serde_yaml::from_str(&data)?;
        let hive_id = hive.hive_id;
        if let Some(role) = hive.role {
            let role = role.trim().to_ascii_lowercase();
            if !role.is_empty() {
                hive_role = role;
            }
        }
        let mut gateway_name = "RT.gateway".to_string();
        if let Some(wan) = hive.wan {
            wan_listen = wan.listen;
            if let Some(uplinks) = wan.uplinks {
                for uplink in uplinks {
                    wan_uplinks.push(uplink.address);
                }
            }
            if let Some(hives) = wan.authorized_hives {
                wan_authorized_hives = hives;
            }
            if let Some(name) = wan.gateway_name {
                gateway_name = name;
            }
        }
        if let Some(nats) = hive.nats {
            if let Some(mode) = nats.mode {
                let mode = mode.trim().to_ascii_lowercase();
                if !mode.is_empty() {
                    nats_mode = mode;
                }
            }
            if let Some(port) = nats.port {
                nats_port = port;
            }
            if let Some(url) = nats.url {
                let url = url.trim().to_string();
                if !url.is_empty() {
                    nats_url = url;
                }
            }
            if let Some(storage_dir) = nats.storage_dir {
                let storage_dir = storage_dir.trim();
                if !storage_dir.is_empty() {
                    nats_storage_dir = PathBuf::from(storage_dir);
                }
            }
        }
        if let Some(blob) = hive.blob {
            if let Some(enabled) = blob.enabled {
                blob_enabled = enabled;
            }
            if let Some(path) = blob.path {
                let path = path.trim();
                if !path.is_empty() {
                    blob_path = PathBuf::from(path);
                }
            }
            if let Some(sync) = blob.sync {
                if let Some(enabled) = sync.enabled {
                    blob_sync_enabled = enabled;
                }
                if let Some(tool) = sync.tool {
                    let tool = tool.trim().to_ascii_lowercase();
                    if !tool.is_empty() {
                        blob_sync_tool = tool;
                    }
                }
                if let Some(api_port) = sync.api_port {
                    blob_sync_api_port = api_port;
                }
                if let Some(data_dir) = sync.data_dir {
                    let data_dir = data_dir.trim();
                    if !data_dir.is_empty() {
                        blob_sync_data_dir = PathBuf::from(data_dir);
                    }
                }
            }
        }
        if nats_url.is_empty() {
            nats_url = format!("nats://127.0.0.1:{nats_port}");
        }

        let router_name = std::env::var("JSR_ROUTER_NAME").unwrap_or_else(|_| gateway_name.clone());
        let is_gateway = is_gateway_name(&router_name, &gateway_name);
        let router_l2_name = ensure_l2_name(&router_name, &hive_id);
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
            hive_id,
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
            wan_authorized_hives,
            hive_role,
            nats_mode,
            nats_port,
            nats_url,
            nats_storage_dir,
            blob_enabled,
            blob_path,
            blob_sync_enabled,
            blob_sync_tool,
            blob_sync_api_port,
            blob_sync_data_dir,
        })
    }
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

fn identity_path(state_dir: &Path, router_l2_name: &str) -> PathBuf {
    state_dir.join(router_l2_name).join("identity.yaml")
}

fn is_gateway_name(router_name: &str, gateway_name: &str) -> bool {
    let base = router_name.split('@').next().unwrap_or(router_name);
    let gateway_base = gateway_name.split('@').next().unwrap_or(gateway_name);
    base == gateway_base
}

fn build_identity_yaml(
    router_l2_name: &str,
    uuid: Uuid,
    shm_name: &str,
) -> Result<String, ConfigError> {
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
