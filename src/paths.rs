use std::path::PathBuf;

pub const CONFIG_DIR: &str = "/etc/fluxbee";
pub const STATE_DIR: &str = "/var/lib/fluxbee/state";
pub const OPA_VERSION_PATH: &str = "/var/lib/fluxbee/opa-version.txt";
pub const RUN_DIR: &str = "/var/run/fluxbee";
pub const ROUTER_SOCKET_DIR: &str = "/var/run/fluxbee/routers";

pub fn config_dir() -> PathBuf {
    PathBuf::from(CONFIG_DIR)
}

pub fn state_dir() -> PathBuf {
    PathBuf::from(STATE_DIR)
}

pub fn run_dir() -> PathBuf {
    PathBuf::from(RUN_DIR)
}

pub fn router_socket_dir() -> PathBuf {
    PathBuf::from(ROUTER_SOCKET_DIR)
}

pub fn opa_version_path() -> PathBuf {
    PathBuf::from(OPA_VERSION_PATH)
}

pub fn storage_root_dir() -> PathBuf {
    PathBuf::from("/var/lib/fluxbee")
}
