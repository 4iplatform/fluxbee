use std::path::PathBuf;

use crate::client_config::ClientConfig;
use crate::split::{NodeReceiver, NodeSender};

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub name: String,
    pub router_socket: PathBuf,
    pub uuid_persistence_dir: PathBuf,
    pub config_dir: PathBuf,
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("invalid announce")]
    InvalidAnnounce,
    #[error("handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("disconnected")]
    Disconnected,
    #[error("timeout")]
    Timeout,
}

pub async fn connect(_config: &NodeConfig) -> Result<(NodeSender, NodeReceiver), NodeError> {
    Err(NodeError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "fluxbee_sdk node transport is only available on unix targets",
    )))
}

pub async fn connect_with_client_config(
    config: &ClientConfig,
) -> Result<(NodeSender, NodeReceiver), NodeError> {
    connect(&config.node).await
}
