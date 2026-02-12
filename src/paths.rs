use std::path::{Path, PathBuf};

pub const CONFIG_DIR: &str = "/etc/fluxbee";
pub const STATE_DIR: &str = "/var/lib/fluxbee/state";
pub const OPA_VERSION_PATH: &str = "/var/lib/fluxbee/opa-version.txt";
pub const RUN_DIR: &str = "/var/run/fluxbee";
pub const ROUTER_SOCKET_DIR: &str = "/var/run/fluxbee/routers";

pub const LEGACY_CONFIG_DIR: &str = "/etc/json-router";
pub const LEGACY_STATE_DIR: &str = "/var/lib/json-router/state";
pub const LEGACY_OPA_VERSION_PATH: &str = "/var/lib/json-router/opa-version.txt";
pub const LEGACY_RUN_DIR: &str = "/var/run/json-router";
pub const LEGACY_ROUTER_SOCKET_DIR: &str = "/var/run/json-router/routers";

pub fn config_dir() -> PathBuf {
    pick_preferred(CONFIG_DIR, LEGACY_CONFIG_DIR)
}

pub fn state_dir() -> PathBuf {
    pick_preferred(STATE_DIR, LEGACY_STATE_DIR)
}

pub fn run_dir() -> PathBuf {
    pick_preferred(RUN_DIR, LEGACY_RUN_DIR)
}

pub fn router_socket_dir() -> PathBuf {
    pick_preferred(ROUTER_SOCKET_DIR, LEGACY_ROUTER_SOCKET_DIR)
}

pub fn opa_version_path() -> PathBuf {
    pick_preferred(OPA_VERSION_PATH, LEGACY_OPA_VERSION_PATH)
}

pub fn storage_root_dir() -> PathBuf {
    let primary = PathBuf::from("/var/lib/fluxbee");
    let legacy = PathBuf::from("/var/lib/json-router");
    if primary.exists() || !legacy.exists() {
        primary
    } else {
        legacy
    }
}

fn pick_preferred(primary: &str, legacy: &str) -> PathBuf {
    if Path::new(primary).exists() || !Path::new(legacy).exists() {
        PathBuf::from(primary)
    } else {
        PathBuf::from(legacy)
    }
}
