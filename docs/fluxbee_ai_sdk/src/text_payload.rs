use fluxbee_sdk::payload::TextV1Payload;
use serde_json::Value;

use crate::errors::{AiSdkError, Result};

pub fn extract_text(payload: &Value) -> Option<String> {
    let parsed = TextV1Payload::from_value(payload).ok()?;
    parsed.content
}

pub fn build_text_response(content: impl Into<String>) -> Result<Value> {
    let payload = TextV1Payload::new(content, vec![]);
    payload.to_value().map_err(AiSdkError::from)
}
