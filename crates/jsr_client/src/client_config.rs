use crate::nats::{resolve_local_nats_endpoint, validate_local_endpoint, NatsError};
use crate::node_client::NodeConfig;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub node: NodeConfig,
    pub nats_endpoint: Option<String>,
}

impl ClientConfig {
    pub fn new(node: NodeConfig) -> Self {
        Self {
            node,
            nats_endpoint: None,
        }
    }

    pub fn with_nats_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.nats_endpoint = Some(endpoint.into());
        self
    }

    pub fn resolved_nats_endpoint(&self) -> Result<String, NatsError> {
        if let Some(endpoint) = self.nats_endpoint.as_deref() {
            let endpoint = endpoint.trim();
            if !endpoint.is_empty() {
                validate_local_endpoint(endpoint)?;
                return Ok(endpoint.to_string());
            }
        }
        resolve_local_nats_endpoint(&self.node.config_dir)
    }
}
