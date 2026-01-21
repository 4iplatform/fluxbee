use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use json_router::node_client::{NodeClient, NodeConfig};
use json_router::shm::{
    copy_bytes_with_len, now_epoch_ms, ConfigRegionWriter, StaticRouteEntry, VpnAssignment,
    ACTION_DROP, ACTION_FORWARD, FLAG_ACTIVE, MATCH_EXACT, MATCH_GLOB, MATCH_PREFIX,
};

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router";

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
}

#[derive(Debug, Deserialize)]
struct SyConfigFile {
    version: u64,
    updated_at: String,
    #[serde(default)]
    routes: Vec<RouteConfig>,
    #[serde(default)]
    vpns: Vec<VpnConfig>,
}

#[derive(Debug, Deserialize)]
struct RouteConfig {
    prefix: String,
    #[serde(default = "default_match_kind")]
    match_kind: String,
    action: String,
    #[serde(default)]
    next_hop_island: Option<String>,
    #[serde(default)]
    metric: Option<u32>,
    #[serde(default)]
    priority: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct VpnConfig {
    pattern: String,
    #[serde(default = "default_match_kind")]
    match_kind: String,
    vpn_id: u32,
    #[serde(default)]
    priority: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = std::env::var("JSR_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_DIR));
    let state_dir = std::env::var("JSR_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_STATE_DIR));
    let socket_dir = std::env::var("JSR_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SOCKET_DIR));

    ensure_dir(&state_dir)?;

    let island = load_island(&config_dir)?;
    let sy_config = load_config(&config_dir)?;

    let shm_name = format!("/jsr-config-{}", island.island_id);
    let node_uuid = load_or_create_uuid(&state_dir.join("nodes"), "SY.config.routes")?;
    let mut writer = ConfigRegionWriter::open_or_create(&shm_name, node_uuid, &island.island_id)?;

    let routes = build_routes(&sy_config.routes)?;
    let vpns = build_vpns(&sy_config.vpns)?;

    writer.write_static_routes(&routes, true)?;
    writer.write_vpn_assignments(&vpns, false)?;

    tracing::info!(
        routes = routes.len(),
        vpns = vpns.len(),
        "config region written"
    );

    let _client = NodeClient::connect(NodeConfig {
        name: "SY.config.routes".to_string(),
        router_socket: socket_dir.join("router.sock"),
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    })
    .await?;

    tracing::info!("connected to router");

    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;
        writer.update_heartbeat();
    }
}

fn load_island(config_dir: &Path) -> Result<IslandFile, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(config_dir.join("island.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn load_config(config_dir: &Path) -> Result<SyConfigFile, Box<dyn std::error::Error>> {
    let path = config_dir.join("sy-config-routes.yaml");
    if !path.exists() {
        let empty = SyConfigFile {
            version: 1,
            updated_at: "".to_string(),
            routes: Vec::new(),
            vpns: Vec::new(),
        };
        let data = serde_yaml::to_string(&empty)?;
        fs::write(&path, data)?;
        return Ok(empty);
    }
    let data = fs::read_to_string(&path)?;
    Ok(serde_yaml::from_str(&data)?)
}

fn build_routes(routes: &[RouteConfig]) -> Result<Vec<StaticRouteEntry>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for route in routes {
        let mut entry = empty_static_route();
        entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, &route.prefix) as u16;
        entry.match_kind = match_kind(&route.match_kind)?;
        entry.action = action_kind(&route.action)?;
        if let Some(next) = &route.next_hop_island {
            entry.next_hop_island_len =
                copy_bytes_with_len(&mut entry.next_hop_island, next) as u8;
        }
        entry.metric = route.metric.unwrap_or(0);
        entry.priority = route.priority.unwrap_or(100);
        entry.flags = FLAG_ACTIVE;
        entry.installed_at = now_epoch_ms();
        out.push(entry);
    }
    Ok(out)
}

fn build_vpns(vpns: &[VpnConfig]) -> Result<Vec<VpnAssignment>, Box<dyn std::error::Error>> {
    let mut out = Vec::new();
    for vpn in vpns {
        let mut entry = empty_vpn_assignment();
        entry.pattern_len = copy_bytes_with_len(&mut entry.pattern, &vpn.pattern) as u16;
        entry.match_kind = match_kind(&vpn.match_kind)?;
        entry.vpn_id = vpn.vpn_id;
        entry.priority = vpn.priority.unwrap_or(100);
        entry.flags = FLAG_ACTIVE;
        out.push(entry);
    }
    Ok(out)
}

fn match_kind(value: &str) -> Result<u8, Box<dyn std::error::Error>> {
    match value {
        "EXACT" => Ok(MATCH_EXACT),
        "PREFIX" => Ok(MATCH_PREFIX),
        "GLOB" => Ok(MATCH_GLOB),
        _ => Err("invalid match_kind".into()),
    }
}

fn action_kind(value: &str) -> Result<u8, Box<dyn std::error::Error>> {
    match value {
        "FORWARD" => Ok(ACTION_FORWARD),
        "DROP" => Ok(ACTION_DROP),
        _ => Err("invalid action".into()),
    }
}

fn default_match_kind() -> String {
    "PREFIX".to_string()
}

fn ensure_dir(path: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)
}

fn load_or_create_uuid(dir: &Path, base_name: &str) -> Result<Uuid, Box<dyn std::error::Error>> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.uuid", base_name));
    if path.exists() {
        let data = fs::read_to_string(&path)?;
        return Ok(Uuid::parse_str(data.trim())?);
    }
    let uuid = Uuid::new_v4();
    fs::write(&path, uuid.to_string())?;
    Ok(uuid)
}

fn empty_static_route() -> StaticRouteEntry {
    StaticRouteEntry {
        prefix: [0u8; 256],
        prefix_len: 0,
        match_kind: 0,
        action: 0,
        next_hop_island: [0u8; 32],
        next_hop_island_len: 0,
        _pad: [0u8; 3],
        metric: 0,
        priority: 0,
        flags: 0,
        installed_at: 0,
        _reserved: [0u8; 8],
    }
}

fn empty_vpn_assignment() -> VpnAssignment {
    VpnAssignment {
        pattern: [0u8; 256],
        pattern_len: 0,
        match_kind: 0,
        _pad0: 0,
        vpn_id: 0,
        priority: 0,
        flags: 0,
        _reserved: [0u8; 20],
    }
}
