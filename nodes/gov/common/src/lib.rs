use std::path::PathBuf;

use fluxbee_sdk::NodeConfig;

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn build_node_config(default_name: &str, default_version: &str) -> NodeConfig {
    let name = env_or("GOV_NODE_NAME", default_name);
    let version = env_or("GOV_NODE_VERSION", default_version);
    let router_socket = PathBuf::from(env_or(
        "GOV_ROUTER_SOCKET_DIR",
        "/var/run/fluxbee/routers",
    ));
    let uuid_persistence_dir = PathBuf::from(env_or(
        "GOV_UUID_PERSISTENCE_DIR",
        "/var/lib/fluxbee/state/nodes",
    ));
    let config_dir = PathBuf::from(env_or("GOV_CONFIG_DIR", "/etc/fluxbee"));

    NodeConfig {
        name,
        router_socket,
        uuid_persistence_dir,
        config_dir,
        version,
    }
}
