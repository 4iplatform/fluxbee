use std::path::PathBuf;

use fluxbee_sdk::blob::{BlobConfig, BlobRef, BlobToolkit, ResolveRetryConfig};
use fluxbee_sdk::payload::TextV1Payload;
use serde_json::Value;
use tokio::fs as tokio_fs;

use crate::errors::{AiSdkError, Result};

pub fn extract_text(payload: &Value) -> Option<String> {
    let parsed = TextV1Payload::from_value(payload).ok()?;
    parsed.content
}

pub fn build_text_response(content: impl Into<String>) -> Result<Value> {
    build_text_response_with_options(content, vec![], &TextResponseOptions::default())
}

const DEFAULT_MAX_ATTACHMENTS: usize = 8;
const DEFAULT_MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ModelInputOptions {
    pub multimodal: bool,
    pub max_attachments: usize,
    pub max_attachment_bytes: u64,
    pub resolve_retry: Option<ResolveRetryConfig>,
    pub blob_root: Option<PathBuf>,
}

impl Default for ModelInputOptions {
    fn default() -> Self {
        Self {
            multimodal: false,
            max_attachments: DEFAULT_MAX_ATTACHMENTS,
            max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
            resolve_retry: None,
            blob_root: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextResponseOptions {
    pub blob_root: Option<PathBuf>,
    pub max_message_bytes: usize,
    pub message_overhead_bytes: usize,
}

impl Default for TextResponseOptions {
    fn default() -> Self {
        Self {
            blob_root: None,
            max_message_bytes: fluxbee_sdk::payload::TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES,
            message_overhead_bytes: fluxbee_sdk::blob::TEXT_V1_DEFAULT_OVERHEAD_BYTES,
        }
    }
}

pub fn build_text_response_with_options(
    content: impl Into<String>,
    attachments: Vec<BlobRef>,
    options: &TextResponseOptions,
) -> Result<Value> {
    let content = content.into();
    let blob = blob_toolkit(options.blob_root.as_ref())
        .map_err(|err| AiSdkError::Protocol(err.to_string()))?;
    let payload = blob
        .build_text_v1_payload_with_limit(
            &content,
            attachments,
            options.max_message_bytes,
            options.message_overhead_bytes,
        )
        .map_err(AiSdkError::from)?;
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
    #[error("blob too large: {0}")]
    BlobTooLarge(String),
    #[error("unsupported attachment mime: {0}")]
    UnsupportedAttachmentMime(String),
    #[error("too many attachments: {0}")]
    TooManyAttachments(String),
}

impl ModelInputPayloadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPayload(_) => "invalid_payload",
            Self::BlobToolkitUnavailable(_) => "blob_toolkit_unavailable",
            Self::BlobNotFound(_) => "BLOB_NOT_FOUND",
            Self::BlobIo(_) => "BLOB_IO_ERROR",
            Self::BlobTooLarge(_) => "BLOB_TOO_LARGE",
            Self::UnsupportedAttachmentMime(_) => "unsupported_attachment_mime",
            Self::TooManyAttachments(_) => "too_many_attachments",
        }
    }

    pub fn retryable(&self) -> bool {
        !matches!(
            self,
            Self::InvalidPayload(_)
                | Self::BlobTooLarge(_)
                | Self::UnsupportedAttachmentMime(_)
                | Self::TooManyAttachments(_)
        )
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

pub async fn build_model_input_from_payload(
    payload: &Value,
) -> std::result::Result<String, ModelInputPayloadError> {
    build_model_input_from_payload_with_options(payload, &ModelInputOptions::default()).await
}

pub async fn build_model_input_from_payload_with_options(
    payload: &Value,
    options: &ModelInputOptions,
) -> std::result::Result<String, ModelInputPayloadError> {
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

    if text_payload.attachments.len() > options.max_attachments {
        return Err(ModelInputPayloadError::TooManyAttachments(format!(
            "received {} attachments, max allowed is {}",
            text_payload.attachments.len(),
            options.max_attachments
        )));
    }

    let blob = blob_toolkit(options.blob_root.as_ref())?;
    let content = if let Some(inline) = text_payload.content {
        inline
    } else if let Some(content_ref) = text_payload.content_ref {
        validate_blob_limits(
            &content_ref,
            options.max_attachment_bytes,
            true,
            options.multimodal,
        )?;
        load_blob_text(&blob, &content_ref, "content_ref", options.resolve_retry).await?
    } else {
        String::new()
    };

    let mut attachment_blocks = Vec::new();
    for (idx, attachment) in text_payload.attachments.iter().enumerate() {
        let slot = idx + 1;
        validate_blob_limits(
            attachment,
            options.max_attachment_bytes,
            false,
            options.multimodal,
        )?;

        let mut block = format!(
            "[attachment: {} | {} | {}]",
            attachment.filename_original, attachment.mime, attachment.size
        );
        if is_textual_mime(&attachment.mime) {
            let text =
                load_blob_text(&blob, attachment, "attachments[]", options.resolve_retry).await?;
            block.push('\n');
            block.push_str(&truncate_chars(&text, 4000));
        }
        block.push_str("\n[attachment_end]");
        attachment_blocks.push(block);
    }

    if attachment_blocks.is_empty() {
        return Ok(content);
    }

    let mut assembled = String::new();
    assembled.push_str(&content);
    assembled.push_str("\n\n--- attachments ---\n");
    assembled.push_str(&attachment_blocks.join("\n\n"));
    assembled.push_str("\n--- end attachments ---");
    Ok(assembled)
}

fn blob_toolkit(
    blob_root_override: Option<&PathBuf>,
) -> std::result::Result<BlobToolkit, ModelInputPayloadError> {
    let blob_root = blob_root_override
        .map(|v| v.to_string_lossy().to_string())
        .or_else(|| env_nonempty("FLUXBEE_BLOB_ROOT"))
        .or_else(|| env_nonempty("BLOB_ROOT"))
        .unwrap_or_else(|| "/var/lib/fluxbee/blob".to_string());
    let mut cfg = BlobConfig::default();
    cfg.blob_root = PathBuf::from(blob_root);
    BlobToolkit::new(cfg).map_err(|err| ModelInputPayloadError::BlobToolkitUnavailable(err.to_string()))
}

async fn load_blob_text(
    blob: &BlobToolkit,
    blob_ref: &BlobRef,
    field: &str,
    resolve_retry: Option<ResolveRetryConfig>,
) -> std::result::Result<String, ModelInputPayloadError> {
    let path = if let Some(cfg) = resolve_retry {
        blob.resolve_with_retry(blob_ref, cfg).await.map_err(|err| {
            if matches!(err, fluxbee_sdk::blob::BlobError::NotFound(_)) {
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
        })?
    } else {
        blob.resolve(blob_ref)
    };
    let data = tokio_fs::read(&path).await.map_err(|err| {
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
    matches!(
        mime,
        "text/plain" | "text/markdown" | "application/json"
    )
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

fn validate_blob_limits(
    blob_ref: &BlobRef,
    max_attachment_bytes: u64,
    is_content_ref: bool,
    multimodal: bool,
) -> std::result::Result<(), ModelInputPayloadError> {
    if blob_ref.size > max_attachment_bytes {
        return Err(ModelInputPayloadError::BlobTooLarge(format!(
            "blob '{}' exceeds max_attachment_bytes: size={} max={}",
            blob_ref.blob_name, blob_ref.size, max_attachment_bytes
        )));
    }
    if !is_textual_mime(&blob_ref.mime) && (!multimodal || is_content_ref) {
        return Err(ModelInputPayloadError::UnsupportedAttachmentMime(format!(
            "mime '{}' is not supported for field {} when multimodal=false",
            blob_ref.mime,
            if is_content_ref {
                "content_ref"
            } else {
                "attachments[]"
            }
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::blob::BlobRef;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::time::{sleep, Duration};

    fn temp_blob_root() -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("fluxbee-ai-sdk-payload-{nanos}-{seq}"))
    }

    #[tokio::test]
    async fn model_input_uses_inline_content_for_text_v1() {
        let payload = json!({
            "type": "text",
            "content": "hola mundo",
            "attachments": []
        });
        let result = build_model_input_from_payload(&payload)
            .await
            .expect("inline should parse");
        assert_eq!(result, "hola mundo");
    }

    #[tokio::test]
    async fn model_input_rejects_invalid_text_v1_contract() {
        let payload = json!({
            "type": "text",
            "attachments": []
        });
        let err = build_model_input_from_payload(&payload)
            .await
            .expect_err("contract must fail");
        assert_eq!(err.code(), "invalid_payload");
        assert!(!err.retryable());
    }

    #[tokio::test]
    async fn model_input_falls_back_to_extract_text_for_non_text_v1() {
        let payload = json!({"kind": "unknown"});
        let result = build_model_input_from_payload(&payload)
            .await
            .expect("fallback should not fail");
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn model_input_rejects_unsupported_attachment_mime_when_not_multimodal() {
        let payload = json!({
            "type": "text",
            "content": "hola",
            "attachments": [{
                "type": "blob_ref",
                "blob_name": "image_0123456789abcdef.png",
                "size": 100,
                "mime": "image/png",
                "filename_original": "image.png",
                "spool_day": "2026-03-31"
            }]
        });
        let err = build_model_input_from_payload(&payload)
            .await
            .expect_err("mime must fail");
        assert_eq!(err.code(), "unsupported_attachment_mime");
    }

    #[tokio::test]
    async fn model_input_rejects_blob_too_large() {
        let payload = json!({
            "type": "text",
            "content": "hola",
            "attachments": [{
                "type": "blob_ref",
                "blob_name": "doc_0123456789abcdef.txt",
                "size": 11 * 1024 * 1024,
                "mime": "text/plain",
                "filename_original": "doc.txt",
                "spool_day": "2026-03-31"
            }]
        });
        let err = build_model_input_from_payload(&payload)
            .await
            .expect_err("size must fail");
        assert_eq!(err.code(), "BLOB_TOO_LARGE");
    }

    #[tokio::test]
    async fn model_input_rejects_too_many_attachments() {
        let sample = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "a_0123456789abcdef.txt".to_string(),
            size: 1,
            mime: "text/plain".to_string(),
            filename_original: "a.txt".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let attachments: Vec<BlobRef> = (0..9)
            .map(|idx| BlobRef {
                blob_name: format!("a{idx}_0123456789abcdef.txt"),
                ..sample.clone()
            })
            .collect();
        let payload = TextV1Payload {
            payload_type: "text".to_string(),
            content: Some("hola".to_string()),
            content_ref: None,
            attachments,
        }
        .to_value()
        .expect("serialize payload");
        let err = build_model_input_from_payload(&payload)
            .await
            .expect_err("count must fail");
        assert_eq!(err.code(), "too_many_attachments");
    }

    #[tokio::test]
    async fn model_input_returns_blob_not_found_for_missing_content_ref() {
        let payload = json!({
            "type": "text",
            "content_ref": {
                "type": "blob_ref",
                "blob_name": "missing_0123456789abcdef.txt",
                "size": 12,
                "mime": "text/plain",
                "filename_original": "missing.txt",
                "spool_day": "2026-03-31"
            },
            "attachments": []
        });
        let root = temp_blob_root();
        let options = ModelInputOptions {
            blob_root: Some(root.clone()),
            ..ModelInputOptions::default()
        };
        let err = build_model_input_from_payload_with_options(&payload, &options)
            .await
            .expect_err("missing blob must fail");
        assert_eq!(err.code(), "BLOB_NOT_FOUND");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn model_input_resolves_content_ref_with_retry_when_blob_appears_late() {
        let root = temp_blob_root();
        let toolkit = blob_toolkit(Some(&root)).expect("toolkit");
        let blob_ref = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "late_0123456789abcdef.txt".to_string(),
            size: 11,
            mime: "text/plain".to_string(),
            filename_original: "late.txt".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let target = toolkit.resolve(&blob_ref);
        std::fs::create_dir_all(
            target
                .parent()
                .expect("target parent must exist for test setup"),
        )
        .expect("create target parent");
        let delayed_path = target.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(40)).await;
            let _ = tokio_fs::write(&delayed_path, b"hola tardio").await;
        });

        let payload = json!({
            "type": "text",
            "content_ref": {
                "type": "blob_ref",
                "blob_name": blob_ref.blob_name,
                "size": blob_ref.size,
                "mime": blob_ref.mime,
                "filename_original": blob_ref.filename_original,
                "spool_day": blob_ref.spool_day
            },
            "attachments": []
        });
        let options = ModelInputOptions {
            resolve_retry: Some(ResolveRetryConfig {
                max_wait_ms: 500,
                initial_delay_ms: 10,
                backoff_factor: 1.4,
            }),
            blob_root: Some(root.clone()),
            ..ModelInputOptions::default()
        };
        let out = build_model_input_from_payload_with_options(&payload, &options)
            .await
            .expect("retry should resolve late blob");
        assert_eq!(out, "hola tardio");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn text_response_offloads_large_content_to_content_ref() {
        let root = temp_blob_root();
        let options = TextResponseOptions {
            blob_root: Some(root.clone()),
            max_message_bytes: 128,
            message_overhead_bytes: 64,
        };
        let large = "x".repeat(2048);
        let payload = build_text_response_with_options(large, vec![], &options)
            .expect("offload should succeed");
        let text = TextV1Payload::from_value(&payload).expect("payload parse");
        assert!(text.content.is_none());
        assert!(text.content_ref.is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}
