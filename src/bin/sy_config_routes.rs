use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use jsr_client::{NodeClient, NodeConfig};
use jsr_client::protocol::{ConfigChangedPayload, MSG_CONFIG_CHANGED, SYSTEM_KIND};
use json_router::shm::{
    copy_bytes_with_len, now_epoch_ms, ConfigRegionWriter, StaticRouteEntry, VpnAssignment,
    ACTION_DROP, ACTION_FORWARD, FLAG_ACTIVE, MATCH_EXACT, MATCH_GLOB, MATCH_PREFIX,
};


#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct SyConfigFile {
    version: u64,
    updated_at: String,
    #[serde(default)]
    routes: Vec<RouteConfig>,
    #[serde(default)]
    vpns: Vec<VpnConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
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

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
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
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_config_routes supports only Linux targets.");
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

    let shm_name = format!("/jsr-config-{}", island.island_id);
    let node_uuid = load_or_create_uuid(&state_dir.join("nodes"), "SY.config.routes")?;
    let mut writer = ConfigRegionWriter::open_or_create(&shm_name, node_uuid, &island.island_id)?;

    apply_config(&mut writer, &sy_config)?;

    let node_config = NodeConfig {
        name: "SY.config.routes".to_string(),
        router_socket: router_socket.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let mut client = NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!("connected to router");

    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                writer.update_heartbeat();
            }
            msg = client.recv() => {
                let msg = match msg {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!("recv error: {err} (reconnecting)");
                        client = NodeClient::connect_with_retry(
                            &node_config,
                            Duration::from_secs(1),
                        )
                        .await?;
                        tracing::info!("reconnected to router");
                        continue;
                    }
                };
                if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_CONFIG_CHANGED) {
                    continue;
                }
                let payload: ConfigChangedPayload = serde_json::from_value(msg.payload)?;
                tracing::info!(
                    subsystem = %payload.subsystem,
                    payload_version = payload.version,
                    current_version = sy_config.version,
                    "config changed received"
                );
                if payload.version != 0 && payload.version <= sy_config.version {
                    tracing::info!(
                        payload_version = payload.version,
                        current_version = sy_config.version,
                        "config version not newer; skipping"
                    );
                    continue;
                }
                let mut next_config = sy_config.clone();
                match payload.subsystem.as_str() {
                    "routes" => {
                        if let Some(routes) = parse_routes(&payload.config)? {
                            next_config.routes = routes;
                        }
                    }
                    "vpn" | "vpns" => {
                        if let Some(vpns) = parse_vpns(&payload.config)? {
                            next_config.vpns = vpns;
                        }
                    }
                    _ => {
                        continue;
                    }
                }
                if next_config.routes == sy_config.routes && next_config.vpns == sy_config.vpns {
                    tracing::info!(
                        subsystem = %payload.subsystem,
                        version = payload.version,
                        "config unchanged; skipping apply"
                    );
                    continue;
                }
                if payload.version == 0 {
                    next_config.version = sy_config.version.saturating_add(1);
                } else {
                    next_config.version = payload.version;
                }
                next_config.updated_at = now_epoch_ms().to_string();
                apply_config(&mut writer, &next_config)?;
                write_config(&config_dir, &next_config)?;
                sy_config = next_config;
                tracing::info!(
                    subsystem = %payload.subsystem,
                    payload_version = payload.version,
                    applied_version = sy_config.version,
                    "config changed applied"
                );
            }
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

fn write_config(config_dir: &Path, config: &SyConfigFile) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_dir.join("sy-config-routes.yaml");
    let data = serde_yaml::to_string(config)?;
    fs::write(&path, data)?;
    Ok(())
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

fn parse_routes(
    value: &serde_json::Value,
) -> Result<Option<Vec<RouteConfig>>, Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(None);
    }
    if value.is_array() {
        let routes: Vec<RouteConfig> = serde_json::from_value(value.clone())?;
        return Ok(Some(routes));
    }
    if let Some(routes_value) = value.get("routes") {
        let routes: Vec<RouteConfig> = serde_json::from_value(routes_value.clone())?;
        return Ok(Some(routes));
    }
    Ok(None)
}

fn parse_vpns(
    value: &serde_json::Value,
) -> Result<Option<Vec<VpnConfig>>, Box<dyn std::error::Error>> {
    if value.is_null() {
        return Ok(None);
    }
    if value.is_array() {
        let vpns: Vec<VpnConfig> = serde_json::from_value(value.clone())?;
        return Ok(Some(vpns));
    }
    if let Some(vpns_value) = value.get("vpns") {
        let vpns: Vec<VpnConfig> = serde_json::from_value(vpns_value.clone())?;
        return Ok(Some(vpns));
    }
    Ok(None)
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

// CONFIG_CHANGED lo emite SY.admin (mother island).
