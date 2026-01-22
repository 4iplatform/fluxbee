use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use json_router::node_client::{NodeClient, NodeConfig};
use json_router::protocol::{ConfigChangedPayload, Destination, Message, Meta, Routing, SYSTEM_KIND};
use json_router::shm::{
    copy_bytes_with_len, now_epoch_ms, ConfigRegionWriter, StaticRouteEntry, VpnAssignment,
    ACTION_DROP, ACTION_FORWARD, FLAG_ACTIVE, MATCH_EXACT, MATCH_GLOB, MATCH_PREFIX,
};

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router/state";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router/routers";

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct SyConfigFile {
    version: u64,
    updated_at: String,
    #[serde(default)]
    routes: Vec<RouteConfig>,
    #[serde(default)]
    vpns: Vec<VpnConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
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

    let config_dir = PathBuf::from(DEFAULT_CONFIG_DIR);
    let state_dir = PathBuf::from(DEFAULT_STATE_DIR);
    let socket_dir = PathBuf::from(DEFAULT_SOCKET_DIR);

    ensure_dir(&state_dir)?;

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
    let mut sy_config = load_config(&config_dir)?;
    let mut last_modified = config_mtime(&config_dir)?;

    let shm_name = format!("/jsr-config-{}", island.island_id);
    let node_uuid = load_or_create_uuid(&state_dir.join("nodes"), "SY.config.routes")?;
    let mut writer = ConfigRegionWriter::open_or_create(&shm_name, node_uuid, &island.island_id)?;

    apply_config(&mut writer, &sy_config)?;

    let mut client = NodeClient::connect(NodeConfig {
        name: "SY.config.routes".to_string(),
        router_socket: router_socket.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    })
    .await?;

    tracing::info!("connected to router");
    send_config_changed(&mut client, &mut writer, &sy_config).await?;

    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;
        writer.update_heartbeat();
        let current = config_mtime(&config_dir)?;
        if current > last_modified {
            sy_config = load_config(&config_dir)?;
            apply_config(&mut writer, &sy_config)?;
            send_config_changed(&mut client, &mut writer, &sy_config).await?;
            last_modified = current;
        }
    }
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

fn apply_config(
    writer: &mut ConfigRegionWriter,
    sy_config: &SyConfigFile,
) -> Result<(), Box<dyn std::error::Error>> {
    let routes = build_routes(&sy_config.routes)?;
    let vpns = build_vpns(&sy_config.vpns)?;
    writer.write_static_routes(&routes, false)?;
    writer.write_vpn_assignments(&vpns, true)?;
    tracing::info!(
        routes = routes.len(),
        vpns = vpns.len(),
        "config region written"
    );
    for vpn in &sy_config.vpns {
        tracing::info!(
            pattern = %vpn.pattern,
            match_kind = %vpn.match_kind,
            vpn_id = vpn.vpn_id,
            "vpn rule loaded"
        );
    }
    Ok(())
}

fn config_mtime(config_dir: &Path) -> Result<std::time::SystemTime, Box<dyn std::error::Error>> {
    let path = config_dir.join("sy-config-routes.yaml");
    Ok(fs::metadata(&path)?.modified()?)
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

async fn send_config_changed(
    client: &mut NodeClient,
    writer: &mut ConfigRegionWriter,
    _config: &SyConfigFile,
) -> Result<(), Box<dyn std::error::Error>> {
    let version = writer
        .read_snapshot()
        .map(|s| s.header.config_version)
        .unwrap_or(0);
    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
            dst: Destination::Broadcast,
            ttl: 2,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some("CONFIG_CHANGED".to_string()),
            target: Some("RT.*".to_string()),
            action: None,
            priority: None,
            context: None,
        },
        payload: serde_json::to_value(ConfigChangedPayload {
            config_version: version,
            changed: vec!["routes".to_string(), "vpns".to_string()],
        })?,
    };
    client.send(&msg).await?;
    tracing::info!(version = version, "config changed broadcast sent");
    Ok(())
}
