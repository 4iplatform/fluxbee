use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use jsr_client::{NodeClient, NodeConfig};
use jsr_client::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, SCOPE_GLOBAL,
    SYSTEM_KIND,
};

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router/state";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router/routers";

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
    role: Option<String>,
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
    #[serde(default)]
    match_kind: Option<String>,
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
    #[serde(default)]
    match_kind: Option<String>,
    vpn_id: u32,
    #[serde(default)]
    priority: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_admin supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(
        std::env::var("JSR_CONFIG_DIR").unwrap_or_else(|_| DEFAULT_CONFIG_DIR.to_string()),
    );
    let state_dir = PathBuf::from(
        std::env::var("JSR_STATE_DIR").unwrap_or_else(|_| DEFAULT_STATE_DIR.to_string()),
    );
    let socket_dir = PathBuf::from(
        std::env::var("JSR_SOCKET_DIR").unwrap_or_else(|_| DEFAULT_SOCKET_DIR.to_string()),
    );

    let island = load_island(&config_dir)?;
    if island.role.as_deref() != Some("mother") {
        tracing::warn!("SY.admin solo corre en mother island; role != mother");
        return Ok(());
    }

    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let mut client = NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!("connected to router");

    let mut last_version = 0u64;
    let mut ticker = time::interval(Duration::from_secs(2));
    loop {
        ticker.tick().await;
        let config = load_config(&config_dir)?;
        if config.version <= last_version {
            continue;
        }
        broadcast_config_changed(
            &mut client,
            "routes",
            config.version,
            serde_json::to_value(&config.routes)?,
        )
        .await?;
        broadcast_config_changed(
            &mut client,
            "vpn",
            config.version,
            serde_json::to_value(&config.vpns)?,
        )
        .await?;
        last_version = config.version;
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

async fn broadcast_config_changed(
    client: &mut NodeClient,
    subsystem: &str,
    version: u64,
    config: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_CONFIG_CHANGED.to_string()),
            scope: Some(SCOPE_GLOBAL.to_string()),
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload: serde_json::to_value(ConfigChangedPayload {
            subsystem: subsystem.to_string(),
            version,
            config,
        })?,
    };
    client.send(&msg).await?;
    tracing::info!(subsystem = subsystem, version = version, "config changed broadcast sent");
    Ok(())
}
