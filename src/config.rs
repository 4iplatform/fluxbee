use std::env;
use std::path::PathBuf;

use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Config {
    pub router_uuid: Uuid,
    pub island_id: String,
    pub nodes_dir: PathBuf,
    pub scan_interval_ms: u64,
    pub fib_refresh_ms: u64,
    pub peer_shm_names: Vec<String>,
    pub uplink_listen: Option<String>,
    pub uplink_peers: Vec<UplinkPeerConfig>,
    pub static_routes: Vec<StaticRouteConfig>,
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

impl Config {
    pub fn from_env() -> Self {
        let router_uuid = env::var("JSR_ROUTER_UUID")
            .ok()
            .and_then(|value| Uuid::parse_str(&value).ok())
            .unwrap_or_else(Uuid::new_v4);

        let island_id = env::var("JSR_ISLAND_ID").unwrap_or_else(|_| "default".to_string());
        let nodes_dir = env::var("JSR_NODES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/run/mesh/nodes"));
        let scan_interval_ms = env::var("JSR_SCAN_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(250);
        let fib_refresh_ms = env::var("JSR_FIB_REFRESH_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1_000);
        let peer_shm_names = env::var("JSR_PEER_SHM")
            .map(|value| {
                value
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect()
            })
            .unwrap_or_else(|_| Vec::new());
        let uplink_listen = env::var("JSR_UPLINK_LISTEN").ok().filter(|v| !v.is_empty());
        let uplink_peers = env::var("JSR_UPLINK_PEERS")
            .map(parse_uplink_peers)
            .unwrap_or_else(|_| Vec::new());
        let static_routes = env::var("JSR_STATIC_ROUTES")
            .map(parse_static_routes)
            .unwrap_or_else(|_| Vec::new());

        Self {
            router_uuid,
            island_id,
            nodes_dir,
            scan_interval_ms,
            fib_refresh_ms,
            peer_shm_names,
            uplink_listen,
            uplink_peers,
            static_routes,
        }
    }
}

fn parse_uplink_peers(value: String) -> Vec<UplinkPeerConfig> {
    value
        .split(',')
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            let mut parts = item.splitn(2, '@');
            let link_id = parts.next()?.parse::<u32>().ok()?;
            let addr = parts.next()?.to_string();
            Some(UplinkPeerConfig { link_id, addr })
        })
        .collect()
}

fn parse_static_routes(value: String) -> Vec<StaticRouteConfig> {
    value
        .split(',')
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            let parts: Vec<&str> = item.split(':').collect();
            if parts.len() != 4 {
                return None;
            }
            let pattern = parts[0].to_string();
            let match_kind = parts[1].to_string();
            let next_hop_router = parts[2].to_string();
            let link_id = parts[3].parse::<u32>().ok()?;
            Some(StaticRouteConfig {
                pattern,
                match_kind,
                next_hop_router,
                link_id,
            })
        })
        .collect()
}
