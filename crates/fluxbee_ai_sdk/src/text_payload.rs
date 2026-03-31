use std::fs;
use std::path::PathBuf;

use fluxbee_sdk::blob::{BlobConfig, BlobRef, BlobToolkit};
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

#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelInputPayloadError {
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
    #[error("blob toolkit unavailable: {0}")]
    BlobToolkitUnavailable(String),
    #[error("blob not found: {0}")]
    BlobNotFound(String),
    #[error("blob io error: {0}")]
    BlobIo(String),
}

impl ModelInputPayloadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPayload(_) => "invalid_payload",
            Self::BlobToolkitUnavailable(_) => "blob_toolkit_unavailable",
            Self::BlobNotFound(_) => "BLOB_NOT_FOUND",
            Self::BlobIo(_) => "BLOB_IO_ERROR",
        }
    }

    pub fn retryable(&self) -> bool {
        !matches!(self, Self::InvalidPayload(_))
    }

    pub fn to_error_payload(&self) -> Value {
        serde_json::json!({
            "type": "error",
            "code": self.code(),
            "message": self.to_string(),
            "retryable": self.retryable()
        })
    }
}

pub fn build_model_input_from_payload(payload: &Value) -> std::result::Result<String, ModelInputPayloadError> {
    let is_text_v1 = payload
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|v| v.eq_ignore_ascii_case("text"));
    if !is_text_v1 {
        return Ok(extract_text(payload).unwrap_or_default());
    }

    let text_payload = TextV1Payload::from_value(payload).map_err(|err| {
        ModelInputPayloadError::InvalidPayload(format!("Invalid text/v1 payload: {err}"))
    })?;

    let blob = blob_toolkit()?;
    let content = if let Some(inline) = text_payload.content {
        inline
    } else if let Some(content_ref) = text_payload.content_ref {
        load_blob_text(&blob, &content_ref, "content_ref")?
    } else {
        String::new()
    };

    let mut attachment_blocks = Vec::new();
    for (idx, attachment) in text_payload.attachments.iter().enumerate() {
        let slot = idx + 1;
        let mut block = format!(
            "[attachment #{slot}] filename={} mime={} size={} blob_name={}",
            attachment.filename_original, attachment.mime, attachment.size, attachment.blob_name
        );
        if is_textual_mime(&attachment.mime) {
            let text = load_blob_text(&blob, attachment, "attachments[]")?;
            let preview = truncate_chars(&text, 4000);
            block.push('\n');
            block.push_str(&preview);
            if text.chars().count() > preview.chars().count() {
                block.push_str("\n[attachment truncated]");
            }
        }
        attachment_blocks.push(block);
    }

    if attachment_blocks.is_empty() {
        return Ok(content);
    }

    let mut assembled = String::new();
    assembled.push_str(&content);
    assembled.push_str("\n\n[attachments]\n");
    assembled.push_str(&attachment_blocks.join("\n\n"));
    Ok(assembled)
}

fn blob_toolkit() -> std::result::Result<BlobToolkit, ModelInputPayloadError> {
    let blob_root = env_nonempty("FLUXBEE_BLOB_ROOT")
        .or_else(|| env_nonempty("BLOB_ROOT"))
        .unwrap_or_else(|| "/var/lib/fluxbee/blob".to_string());
    let mut cfg = BlobConfig::default();
    cfg.blob_root = PathBuf::from(blob_root);
    BlobToolkit::new(cfg).map_err(|err| ModelInputPayloadError::BlobToolkitUnavailable(err.to_string()))
}

fn load_blob_text(
    blob: &BlobToolkit,
    blob_ref: &BlobRef,
    field: &str,
) -> std::result::Result<String, ModelInputPayloadError> {
    let path = blob.resolve(blob_ref);
    let data = fs::read(&path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ModelInputPayloadError::BlobNotFound(format!(
                "Failed to resolve {field} blob '{}': {}",
                blob_ref.blob_name, err
            ))
        } else {
            ModelInputPayloadError::BlobIo(format!(
                "Failed to resolve {field} blob '{}': {}",
                blob_ref.blob_name, err
            ))
        }
    })?;
    Ok(String::from_utf8_lossy(&data).to_string())
}

fn is_textual_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime.contains("json")
        || mime.contains("xml")
        || mime.contains("yaml")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_input_uses_inline_content_for_text_v1() {
        let payload = json!({
            "type": "text",
            "content": "hola mundo",
            "attachments": []
        });
        let result = build_model_input_from_payload(&payload).expect("inline should parse");
        assert_eq!(result, "hola mundo");
    }

    #[test]
    fn model_input_rejects_invalid_text_v1_contract() {
        let payload = json!({
            "type": "text",
            "attachments": []
        });
        let err = build_model_input_from_payload(&payload).expect_err("contract must fail");
        assert_eq!(err.code(), "invalid_payload");
        assert!(!err.retryable());
    }

    #[test]
    fn model_input_falls_back_to_extract_text_for_non_text_v1() {
        let payload = json!({"kind": "unknown"});
        let result = build_model_input_from_payload(&payload).expect("fallback should not fail");
        assert_eq!(result, "");
    }
}
