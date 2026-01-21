use std::path::PathBuf;

#[derive(Debug)]
pub struct NodeConfig {
    pub name: String,
    pub router_socket: PathBuf,
    pub uuid_persistence_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("unimplemented")]
    Unimplemented,
}

pub struct NodeClient;

impl NodeClient {
    pub async fn connect(_config: NodeConfig) -> Result<Self, NodeError> {
        Err(NodeError::Unimplemented)
    }
}
