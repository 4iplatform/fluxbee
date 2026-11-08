use std::fmt;

use fluxbee_sdk::blob::BlobError;
use fluxbee_sdk::node_client::NodeError;
use fluxbee_sdk::payload::PayloadError;

use crate::llm::AiProvider;

#[derive(Debug, thiserror::Error)]
pub enum AiSdkError {
    #[error("node client error: {0}")]
    Node(#[from] NodeError),
    #[error("payload error: {0}")]
    Payload(#[from] PayloadError),
    #[error("blob error: {0}")]
    Blob(#[from] BlobError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The provider answered the call with an error status, after its retries (FINDINGS A-184).
    #[error("{0}")]
    Provider(ProviderError),
    /// The model declined to answer: Anthropic's `stop_reason: refusal`, an OpenAI refusal, or
    /// OpenAI's content filter. It is the model's answer, not a fault of the call (FINDINGS A-184).
    #[error("{provider} model refused to answer ({reason})")]
    Refused {
        provider: AiProvider,
        reason: &'static str,
    },
    /// The model's answer hit its token limit and was cut (`model_settings.max_output_tokens`).
    /// The call was answered and billed; sending the same request again gets the same cut.
    #[error("{provider} response truncated: hit {limit} (raise model_settings.max_output_tokens)")]
    Truncated {
        provider: AiProvider,
        limit: &'static str,
    },
    #[error("invalid response contract: {detail}")]
    InvalidResponseContract { detail: String },
    #[error("invalid structured output: {detail}")]
    InvalidStructuredOutput { detail: String },
    #[error("invalid menu: {detail}")]
    InvalidMenu { detail: String },
    #[error("conflicting contracts: the message carries a WF's menu (meta.context.wf.menu) and a response_envelope")]
    ConflictingContracts,
    #[error("artifact MIME not allowed: {mime}")]
    ArtifactMimeNotAllowed { mime: String },
    #[error("artifact filename invalid: {detail}")]
    ArtifactFilenameInvalid { detail: String },
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("recoverable failure after retries: {0}")]
    RecoverableExhausted(String),
}

pub type Result<T> = std::result::Result<T, AiSdkError>;

/// What a person gets from an agent whose AI provider gave no answer (`is_provider_failure`): one
/// fixed text in English, and nothing more; the error itself goes to the node's log (FINDINGS
/// A-184, the operator's call of 2026-11-08). The same in every AI node that answers people.
pub const PROVIDER_FAILURE_TEXT: &str = "There is a problem with the AI provider this agent uses, \
                                         so it could not answer. Please contact your administrator.";

/// An AI provider's answer to a call that did not succeed: its HTTP status and what its error body
/// names, never the body itself (audit B6: OpenAI's 401 body echoes the key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub provider: AiProvider,
    pub status: u16,
    /// The body's `error.type` (`rate_limit_error`, `invalid_request_error`, …), or
    /// `unknown_error` when the body names none (a proxy's HTML page).
    pub error_type: String,
    /// OpenAI's `error.param`: the request field it refused, an attachment's on a 400.
    pub param: Option<String>,
    /// The provider's id for the request: Anthropic's `request-id`, OpenAI's `x-request-id`.
    pub request_id: Option<String>,
    /// The body's `error.message`, any key it echoes redacted.
    pub message: String,
}

/// What a provider's error status says, for whoever answers on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// 400, 404, 422: a request the provider does not take.
    InvalidRequest,
    /// 401, 403: the key.
    Auth,
    /// 408.
    Timeout,
    /// 429: its rate or its quota.
    RateLimited,
    /// 5xx, Anthropic's 529 (overloaded) among them.
    Unavailable,
    /// Any other status, 409 among them.
    Other,
}

impl ProviderError {
    pub fn kind(&self) -> ProviderErrorKind {
        match self.status {
            400 | 404 | 422 => ProviderErrorKind::InvalidRequest,
            401 | 403 => ProviderErrorKind::Auth,
            408 => ProviderErrorKind::Timeout,
            429 => ProviderErrorKind::RateLimited,
            500..=599 => ProviderErrorKind::Unavailable,
            _ => ProviderErrorKind::Other,
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} error status={} type={}",
            self.provider, self.status, self.error_type
        )?;
        if let Some(param) = &self.param {
            write!(f, " param={param}")?;
        }
        if let Some(request_id) = &self.request_id {
            write!(f, " request_id={request_id}")?;
        }
        write!(f, " message={}", self.message)
    }
}

impl From<ProviderError> for AiSdkError {
    fn from(err: ProviderError) -> Self {
        AiSdkError::Provider(err)
    }
}

impl AiSdkError {
    /// The model's answer hit its token limit and was cut (`model_settings.max_output_tokens`).
    /// The call was answered and billed; sending the same request again gets the same cut.
    pub fn is_truncated_answer(&self) -> bool {
        matches!(self, AiSdkError::Truncated { .. })
    }

    /// The provider gave no answer: an error status after its retries, no connection or an answer
    /// that could not be read, none in time, or the model's refusal. Neither the model's answer
    /// nor the caller's request is at fault (FINDINGS A-184).
    pub fn is_provider_failure(&self) -> bool {
        matches!(
            self,
            AiSdkError::Provider(_)
                | AiSdkError::Http(_)
                | AiSdkError::Timeout(_)
                | AiSdkError::Refused { .. }
        )
    }

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
                NodeError::MessageTooLarge { .. } => false,
            },
            AiSdkError::Http(err) => err.is_timeout() || err.is_connect() || err.is_request(),
            AiSdkError::Blob(err) => matches!(err, BlobError::NotFound(_) | BlobError::Io(_)),
            AiSdkError::Timeout(_) => true,
            AiSdkError::RecoverableExhausted(_) => true,
            AiSdkError::Payload(_) => false,
            AiSdkError::Json(_) => false,
            AiSdkError::Protocol(_) => false,
            // The SDK already asked again whatever status warrants it.
            AiSdkError::Provider(_) => false,
            AiSdkError::Refused { .. } => false,
            AiSdkError::Truncated { .. } => false,
            AiSdkError::InvalidResponseContract { .. } => false,
            AiSdkError::InvalidStructuredOutput { .. } => false,
            AiSdkError::InvalidMenu { .. } => false,
            AiSdkError::ConflictingContracts => false,
            AiSdkError::ArtifactMimeNotAllowed { .. } => false,
            AiSdkError::ArtifactFilenameInvalid { .. } => false,
        }
    }
}
