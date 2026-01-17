use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::shm::{build_shm_name, SHM_NAME_PREFIX};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("router name required (--name or JSR_ROUTER_NAME)")]
    MissingRouterName,
    #[error("config file not found and island.yaml missing")]
    MissingConfig,
    #[error("router not authorized: {0}")]
    RouterNotAuthorized(String),
    #[error("config name mismatch: expected {expected}, got {actual}")]
    ConfigNameMismatch { expected: String, actual: String },
    #[error("identity name mismatch")]
    IdentityNameMismatch,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub router_name: String,
    pub router_uuid: Uuid,
    pub island_id: String,
    pub node_socket_dir: PathBuf,
    pub shm_name: String,
    pub scan_interval_ms: u64,
    pub fib_refresh_ms: u64,
    pub uplink_listen: Option<String>,
    pub uplink_peers: Vec<UplinkPeerConfig>,
    pub static_routes: Vec<StaticRouteConfig>,
    pub peer_shm_names: Vec<String>,
    pub log_level: String,
}

#[derive(Clone, Debug)]
pub struct UplinkPeerConfig {
    pub link_id: u32,
    pub addr: String,
}

#[derive(Clone, Debug)]
pub struct StaticRouteConfig {
    pub pattern: String,
    pub match_kind: String,
    pub next_hop_router: String,
    pub link_id: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct IslandConfig {
    island: IslandSection,
    defaults: DefaultsConfig,
    routers: Vec<RouterEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DefaultsConfig {
    #[serde(default)]
    paths: PathsConfig,
    #[serde(default)]
    timers: TimersConfig,
}

#[derive(Debug, Deserialize, Serialize)]
struct RouterConfig {
    router: RouterSection,
    #[serde(default)]
    paths: PathsConfig,
    #[serde(default)]
    timers: TimersConfig,
    #[serde(default)]
    wan: WanConfig,
    #[serde(default)]
    local_peers: LocalPeersConfig,
    #[serde(default)]
    routing: RoutingConfig,
}

#[derive(Debug, Deserialize, Serialize)]
struct RouterEntry {
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct IslandSection {
    id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct RouterSection {
    name: String,
    island_id: String,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct PathsConfig {
    #[serde(default = "default_state_dir")]
    state_dir: PathBuf,
    #[serde(default = "default_node_socket_dir")]
    node_socket_dir: PathBuf,
    #[serde(default = "default_shm_prefix")]
    shm_prefix: String,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct TimersConfig {
    #[serde(default = "default_hello_interval_ms")]
    hello_interval_ms: u64,
    #[serde(default = "default_dead_interval_ms")]
    dead_interval_ms: u64,
    #[serde(default = "default_heartbeat_interval_ms")]
    heartbeat_interval_ms: u64,
    #[serde(default = "default_heartbeat_stale_ms")]
    heartbeat_stale_ms: u64,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct WanConfig {
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    uplinks: Vec<WanUplink>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct WanUplink {
    address: String,
    #[serde(default)]
    island: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct LocalPeersConfig {
    #[serde(default)]
    shm: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct RoutingConfig {
    #[serde(default)]
    static_routes: Vec<StaticRouteConfigFile>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct StaticRouteConfigFile {
    pattern: String,
    #[serde(default = "default_match_kind")]
    match_kind: String,
    next_hop_router: String,
    #[serde(default = "default_link_id")]
    link_id: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct IdentityFile {
    layer1: IdentityLayer1,
    layer2: IdentityLayer2,
    shm: IdentityShm,
    created_at: String,
    created_at_ms: u64,
    system: IdentitySystem,
}

#[derive(Debug, Deserialize, Serialize)]
struct IdentityLayer1 {
    uuid: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct IdentityLayer2 {
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct IdentityShm {
    name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct IdentitySystem {
    hostname: String,
    pid_at_creation: u32,
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let args = Args::parse()?;
        let config_dir = args.config_dir.unwrap_or_else(|| PathBuf::from("/etc/json-router"));

        let router_config = load_or_create_router_config(&config_dir, &args.router_name)?;
        if router_config.router.name != args.router_name {
            return Err(ConfigError::ConfigNameMismatch {
                expected: args.router_name.clone(),
                actual: router_config.router.name,
            });
        }

        let identity =
            load_or_create_identity(&router_config.paths, &router_config.router.name)?;
        if identity.layer2.name != router_config.router.name {
            return Err(ConfigError::IdentityNameMismatch);
        }

        let router_uuid = Uuid::parse_str(&identity.layer1.uuid)?;
        let uplink_peers = router_config
            .wan
            .uplinks
            .iter()
            .enumerate()
            .map(|(idx, uplink)| UplinkPeerConfig {
                link_id: (idx + 1) as u32,
                addr: uplink.address.clone(),
            })
            .collect();
        let static_routes = router_config
            .routing
            .static_routes
            .into_iter()
            .map(|route| StaticRouteConfig {
                pattern: route.pattern,
                match_kind: route.match_kind,
                next_hop_router: route.next_hop_router,
                link_id: route.link_id,
            })
            .collect();

        Ok(Self {
            router_name: router_config.router.name,
            router_uuid,
            island_id: router_config.router.island_id,
            node_socket_dir: router_config.paths.node_socket_dir,
            shm_name: identity.shm.name,
            scan_interval_ms: 250,
            fib_refresh_ms: 1_000,
            uplink_listen: router_config.wan.listen,
            uplink_peers,
            static_routes,
            peer_shm_names: router_config.local_peers.shm,
            log_level: args.log_level,
        })
    }
}

struct Args {
    router_name: String,
    config_dir: Option<PathBuf>,
    log_level: String,
}

impl Args {
    fn parse() -> Result<Self, ConfigError> {
        let mut router_name = env::var("JSR_ROUTER_NAME").ok();
        let mut config_dir = env::var("JSR_CONFIG_DIR").ok().map(PathBuf::from);
        let mut log_level = env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--name" => {
                    router_name = args.next();
                }
                "--config" => {
                    config_dir = args.next().map(PathBuf::from);
                }
                "--log-level" => {
                    log_level = args.next().unwrap_or_else(|| log_level.clone());
                }
                _ => {}
            }
        }

        let router_name = router_name.ok_or(ConfigError::MissingRouterName)?;
        Ok(Self {
            router_name,
            config_dir,
            log_level,
        })
    }
}

fn load_or_create_router_config(
    config_dir: &Path,
    router_name: &str,
) -> Result<RouterConfig, ConfigError> {
    let router_dir = config_dir.join("routers").join(router_name);
    let router_config_path = router_dir.join("config.yaml");

    if router_config_path.exists() {
        let data = fs::read_to_string(&router_config_path)?;
        let config: RouterConfig = serde_yaml::from_str(&data)?;
        return Ok(config);
    }

    let island_path = config_dir.join("island.yaml");
    if !island_path.exists() {
        return Err(ConfigError::MissingConfig);
    }

    let island_data = fs::read_to_string(&island_path)?;
    let island: IslandConfig = serde_yaml::from_str(&island_data)?;
    if !island.routers.iter().any(|r| r.name == router_name) {
        return Err(ConfigError::RouterNotAuthorized(router_name.to_string()));
    }

    let router_config = RouterConfig {
        router: RouterSection {
            name: router_name.to_string(),
            island_id: island.island.id,
        },
        paths: island.defaults.paths,
        timers: island.defaults.timers,
        wan: WanConfig::default(),
        local_peers: LocalPeersConfig::default(),
        routing: RoutingConfig::default(),
    };

    fs::create_dir_all(&router_dir)?;
    let serialized = serde_yaml::to_string(&router_config)?;
    fs::write(&router_config_path, serialized)?;
    Ok(router_config)
}

fn load_or_create_identity(
    paths: &PathsConfig,
    router_name: &str,
) -> Result<IdentityFile, ConfigError> {
    let identity_dir = paths.state_dir.join(router_name);
    let identity_path = identity_dir.join("identity.yaml");

    if identity_path.exists() {
        let data = fs::read_to_string(&identity_path)?;
        let identity: IdentityFile = serde_yaml::from_str(&data)?;
        return Ok(identity);
    }

    fs::create_dir_all(&identity_dir)?;
    let router_uuid = Uuid::new_v4();
    let shm_name = build_shm_name(&paths.shm_prefix, router_uuid);
    let now_ms = now_epoch_ms();
    let identity = IdentityFile {
        layer1: IdentityLayer1 {
            uuid: router_uuid.to_string(),
        },
        layer2: IdentityLayer2 {
            name: router_name.to_string(),
        },
        shm: IdentityShm { name: shm_name },
        created_at: chrono::Utc::now().to_rfc3339(),
        created_at_ms: now_ms,
        system: IdentitySystem {
            hostname: env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
            pid_at_creation: std::process::id(),
        },
    };

    let serialized = serde_yaml::to_string(&identity)?;
    fs::write(&identity_path, serialized)?;
    Ok(identity)
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("./state")
}

fn default_node_socket_dir() -> PathBuf {
    PathBuf::from("/var/run/mesh/nodes")
}

fn default_shm_prefix() -> String {
    SHM_NAME_PREFIX.to_string()
}

fn default_hello_interval_ms() -> u64 {
    10_000
}

fn default_dead_interval_ms() -> u64 {
    40_000
}

fn default_heartbeat_interval_ms() -> u64 {
    5_000
}

fn default_heartbeat_stale_ms() -> u64 {
    30_000
}

fn default_match_kind() -> String {
    "exact".to_string()
}

fn default_link_id() -> u32 {
    1
}
