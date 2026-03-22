use fluxbee_sdk::node_client::NodeError;
use fluxbee_sdk::payload::PayloadError;

#[derive(Debug, thiserror::Error)]
pub enum AiSdkError {
    #[error("node client error: {0}")]
    Node(#[from] NodeError),
    #[error("payload error: {0}")]
    Payload(#[from] PayloadError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("recoverable failure after retries: {0}")]
    RecoverableExhausted(String),
}

pub type Result<T> = std::result::Result<T, AiSdkError>;

impl AiSdkError {
    pub fn is_recoverable(&self) -> bool {
        match self {
            AiSdkError::Node(node_err) => match node_err {
                NodeError::Io(_) => true,
                NodeError::Disconnected => true,
                NodeError::Timeout => true,
                NodeError::Json(_) => false,
                NodeError::Yaml(_) => false,
                NodeError::Uuid(_) => false,
                NodeError::InvalidAnnounce => false,
                NodeError::HandshakeFailed(_) => false,
            },
            AiSdkError::Http(err) => err.is_timeout() || err.is_connect() || err.is_request(),
            AiSdkError::Timeout(_) => true,
            AiSdkError::RecoverableExhausted(_) => true,
            AiSdkError::Payload(_) => false,
            AiSdkError::Json(_) => false,
            AiSdkError::Protocol(_) => false,
        }
    }
}
