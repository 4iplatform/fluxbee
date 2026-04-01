use std::path::PathBuf;

use base64::Engine as _;
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

#[derive(Debug, Clone)]
pub struct ResolvedModelAttachment {
    pub blob_ref: BlobRef,
    pub path: PathBuf,
    pub text_content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedModelInput {
    pub main_text: String,
    pub prompt_text: String,
    pub attachments: Vec<ResolvedModelAttachment>,
}

#[derive(Debug, Clone)]
pub struct OpenAiUserContentOptions {
    pub image_detail: Option<String>,
}

impl Default for OpenAiUserContentOptions {
    fn default() -> Self {
        Self {
            image_detail: Some("auto".to_string()),
        }
    }
}

pub async fn build_openai_user_content_parts(
    input: &ResolvedModelInput,
) -> std::result::Result<Vec<Value>, ModelInputPayloadError> {
    build_openai_user_content_parts_with_options(input, &OpenAiUserContentOptions::default()).await
}

pub async fn build_openai_user_content_parts_with_options(
    input: &ResolvedModelInput,
    options: &OpenAiUserContentOptions,
) -> std::result::Result<Vec<Value>, ModelInputPayloadError> {
    let mut parts = Vec::new();
    if !input.main_text.trim().is_empty() {
        parts.push(serde_json::json!({
            "type": "input_text",
            "text": input.main_text,
        }));
    }

    for attachment in &input.attachments {
        if let Some(text) = &attachment.text_content {
            parts.push(serde_json::json!({
                "type": "input_text",
                "text": format!(
                    "Attachment ({}, {}):\n{}",
                    attachment.blob_ref.filename_original,
                    attachment.blob_ref.mime,
                    truncate_chars(text, 4000)
                )
            }));
            continue;
        }

        if is_openai_image_mime(&attachment.blob_ref.mime) {
            let bytes = tokio_fs::read(&attachment.path).await.map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    ModelInputPayloadError::BlobNotFound(format!(
                        "Failed to read attachment blob '{}': {}",
                        attachment.blob_ref.blob_name, err
                    ))
                } else {
                    ModelInputPayloadError::BlobIo(format!(
                        "Failed to read attachment blob '{}': {}",
                        attachment.blob_ref.blob_name, err
                    ))
                }
            })?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let image_url = format!("data:{};base64,{}", attachment.blob_ref.mime, encoded);
            parts.push(serde_json::json!({
                "type": "input_image",
                "image_url": image_url,
                "detail": options.image_detail.clone().unwrap_or_else(|| "auto".to_string()),
            }));
            continue;
        }

        let bytes = tokio_fs::read(&attachment.path).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ModelInputPayloadError::BlobNotFound(format!(
                    "Failed to read attachment blob '{}': {}",
                    attachment.blob_ref.blob_name, err
                ))
            } else {
                ModelInputPayloadError::BlobIo(format!(
                    "Failed to read attachment blob '{}': {}",
                    attachment.blob_ref.blob_name, err
                ))
            }
        })?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let filename = if attachment.blob_ref.filename_original.trim().is_empty() {
            attachment.blob_ref.blob_name.clone()
        } else {
            attachment.blob_ref.filename_original.clone()
        };
        parts.push(serde_json::json!({
            "type": "input_file",
            "filename": filename,
            "file_data": encoded,
        }));
    }

    Ok(parts)
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
    let resolved =
        resolve_model_input_from_payload_with_options(payload, &ModelInputOptions::default())
            .await?;
    Ok(resolved.prompt_text)
}

pub async fn build_model_input_from_payload_with_options(
    payload: &Value,
    options: &ModelInputOptions,
) -> std::result::Result<String, ModelInputPayloadError> {
    let resolved = resolve_model_input_from_payload_with_options(payload, options).await?;
    Ok(resolved.prompt_text)
}

pub async fn resolve_model_input_from_payload(
    payload: &Value,
) -> std::result::Result<ResolvedModelInput, ModelInputPayloadError> {
    resolve_model_input_from_payload_with_options(payload, &ModelInputOptions::default()).await
}

pub async fn resolve_model_input_from_payload_with_options(
    payload: &Value,
    options: &ModelInputOptions,
) -> std::result::Result<ResolvedModelInput, ModelInputPayloadError> {
    let is_text_v1 = payload
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|v| v.eq_ignore_ascii_case("text"));
    if !is_text_v1 {
        let main_text = extract_text(payload).unwrap_or_default();
        return Ok(ResolvedModelInput {
            prompt_text: main_text.clone(),
            main_text,
            attachments: Vec::new(),
        });
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
    let main_text = if let Some(inline) = text_payload.content {
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

    let mut resolved_attachments = Vec::new();
    let mut attachment_blocks = Vec::new();
    for (idx, attachment) in text_payload.attachments.iter().enumerate() {
        validate_blob_limits(
            attachment,
            options.max_attachment_bytes,
            false,
            options.multimodal,
        )?;

        let path =
            resolve_blob_path(&blob, attachment, "attachments[]", options.resolve_retry).await?;
        let mut text_content = None;
        if is_textual_mime(&attachment.mime) {
            text_content =
                Some(load_blob_text_from_path(&path, attachment, "attachments[]").await?);
        }
        resolved_attachments.push(ResolvedModelAttachment {
            blob_ref: attachment.clone(),
            path: path.clone(),
            text_content,
        });

        let slot = idx + 1;
        let mut block = format!(
            "[attachment #{slot}: {} | {} | {}]",
            attachment.filename_original, attachment.mime, attachment.size
        );
        if let Some(text) = resolved_attachments
            .last()
            .and_then(|resolved| resolved.text_content.as_ref())
        {
            block.push('\n');
            block.push_str(&truncate_chars(text, 4000));
        }
        block.push_str("\n[attachment_end]");
        attachment_blocks.push(block);
    }

    if attachment_blocks.is_empty() {
        return Ok(ResolvedModelInput {
            prompt_text: main_text.clone(),
            main_text,
            attachments: resolved_attachments,
        });
    }

    let mut prompt_text = String::new();
    prompt_text.push_str(&main_text);
    prompt_text.push_str("\n\n--- attachments ---\n");
    prompt_text.push_str(&attachment_blocks.join("\n\n"));
    prompt_text.push_str("\n--- end attachments ---");
    Ok(ResolvedModelInput {
        main_text,
        prompt_text,
        attachments: resolved_attachments,
    })
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
    BlobToolkit::new(cfg)
        .map_err(|err| ModelInputPayloadError::BlobToolkitUnavailable(err.to_string()))
}

async fn load_blob_text(
    blob: &BlobToolkit,
    blob_ref: &BlobRef,
    field: &str,
    resolve_retry: Option<ResolveRetryConfig>,
) -> std::result::Result<String, ModelInputPayloadError> {
    let path = resolve_blob_path(blob, blob_ref, field, resolve_retry).await?;
    load_blob_text_from_path(&path, blob_ref, field).await
}

async fn resolve_blob_path(
    blob: &BlobToolkit,
    blob_ref: &BlobRef,
    field: &str,
    resolve_retry: Option<ResolveRetryConfig>,
) -> std::result::Result<PathBuf, ModelInputPayloadError> {
    if let Some(cfg) = resolve_retry {
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
        })
    } else {
        Ok(blob.resolve(blob_ref))
    }
}

async fn load_blob_text_from_path(
    path: &PathBuf,
    blob_ref: &BlobRef,
    field: &str,
) -> std::result::Result<String, ModelInputPayloadError> {
    let data = tokio_fs::read(path).await.map_err(|err| {
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
    matches!(mime, "text/plain" | "text/markdown" | "application/json")
}

fn is_openai_image_mime(mime: &str) -> bool {
    matches!(mime, "image/png" | "image/jpeg" | "image/webp")
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
    use std::path::PathBuf;
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

    #[tokio::test]
    async fn resolve_model_input_accepts_image_when_multimodal_enabled() {
        let payload = json!({
            "type": "text",
            "content": "describe la imagen",
            "attachments": [{
                "type": "blob_ref",
                "blob_name": "img_0123456789abcdef.png",
                "size": 2048,
                "mime": "image/png",
                "filename_original": "image.png",
                "spool_day": "2026-03-31"
            }]
        });
        let options = ModelInputOptions {
            multimodal: true,
            ..ModelInputOptions::default()
        };
        let out = resolve_model_input_from_payload_with_options(&payload, &options)
            .await
            .expect("image should be accepted in multimodal");
        assert_eq!(out.main_text, "describe la imagen");
        assert_eq!(out.attachments.len(), 1);
        assert_eq!(out.attachments[0].blob_ref.mime, "image/png");
        assert!(out.attachments[0].text_content.is_none());
    }

    #[tokio::test]
    async fn resolve_model_input_builds_mixed_prompt_and_structured_attachments() {
        let root = temp_blob_root();
        let toolkit = blob_toolkit(Some(&root)).expect("toolkit");
        let text_blob = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "doc_0123456789abcdef.txt".to_string(),
            size: 15,
            mime: "text/plain".to_string(),
            filename_original: "error.txt".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let text_path = toolkit.resolve(&text_blob);
        std::fs::create_dir_all(
            text_path
                .parent()
                .expect("text blob parent must exist for test setup"),
        )
        .expect("create blob parent");
        std::fs::write(&text_path, b"linea uno\nlinea dos").expect("write text blob");

        let image_blob = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "img_0123456789abcdef.png".to_string(),
            size: 2048,
            mime: "image/png".to_string(),
            filename_original: "image.png".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let payload = json!({
            "type": "text",
            "content": "analiza los adjuntos",
            "attachments": [
                {
                    "type": text_blob.ref_type,
                    "blob_name": text_blob.blob_name,
                    "size": text_blob.size,
                    "mime": text_blob.mime,
                    "filename_original": text_blob.filename_original,
                    "spool_day": text_blob.spool_day
                },
                {
                    "type": image_blob.ref_type,
                    "blob_name": image_blob.blob_name,
                    "size": image_blob.size,
                    "mime": image_blob.mime,
                    "filename_original": image_blob.filename_original,
                    "spool_day": image_blob.spool_day
                }
            ]
        });
        let options = ModelInputOptions {
            multimodal: true,
            blob_root: Some(root.clone()),
            ..ModelInputOptions::default()
        };
        let out = resolve_model_input_from_payload_with_options(&payload, &options)
            .await
            .expect("mixed payload should resolve");
        assert_eq!(out.attachments.len(), 2);
        assert_eq!(
            out.attachments[0].text_content.as_deref(),
            Some("linea uno\nlinea dos")
        );
        assert!(out.attachments[1].text_content.is_none());
        assert!(out.prompt_text.contains("--- attachments ---"));
        assert!(out.prompt_text.contains("linea uno"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn build_openai_user_content_parts_serializes_text_and_image() {
        let root = temp_blob_root();
        let toolkit = blob_toolkit(Some(&root)).expect("toolkit");
        let image_blob = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "img_0123456789abcdef.png".to_string(),
            size: 4,
            mime: "image/png".to_string(),
            filename_original: "image.png".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let image_path = toolkit.resolve(&image_blob);
        std::fs::create_dir_all(
            image_path
                .parent()
                .expect("image blob parent must exist for test setup"),
        )
        .expect("create blob parent");
        std::fs::write(&image_path, [0_u8, 1_u8, 2_u8, 3_u8]).expect("write image bytes");

        let input = ResolvedModelInput {
            main_text: "mirÃ¡".to_string(),
            prompt_text: "mirÃ¡".to_string(),
            attachments: vec![ResolvedModelAttachment {
                blob_ref: image_blob,
                path: image_path,
                text_content: None,
            }],
        };
        let parts = build_openai_user_content_parts(&input)
            .await
            .expect("parts should build");
        assert_eq!(parts[0]["type"], "input_text");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["detail"], "auto");
        assert!(parts[1]["image_url"]
            .as_str()
            .expect("image_url should be string")
            .starts_with("data:image/png;base64,"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    #[tokio::test]
    async fn build_openai_user_content_parts_serializes_non_image_as_input_file() {
        let root = temp_blob_root();
        let toolkit = blob_toolkit(Some(&root)).expect("toolkit");
        let file_blob = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "doc_0123456789abcdef.pdf".to_string(),
            size: 12,
            mime: "application/pdf".to_string(),
            filename_original: "doc.pdf".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let file_path = toolkit.resolve(&file_blob);
        std::fs::create_dir_all(
            file_path
                .parent()
                .expect("file blob parent must exist for test setup"),
        )
        .expect("create blob parent");
        std::fs::write(&file_path, b"%PDF-test").expect("write file bytes");
        let input = ResolvedModelInput {
            main_text: "mira".to_string(),
            prompt_text: "mira".to_string(),
            attachments: vec![ResolvedModelAttachment {
                blob_ref: file_blob,
                path: file_path,
                text_content: None,
            }],
        };
        let parts = build_openai_user_content_parts(&input)
            .await
            .expect("pdf should map to input_file");
        assert_eq!(parts[1]["type"], "input_file");
        assert_eq!(parts[1]["filename"], "doc.pdf");
        assert!(
            parts[1]["file_data"]
                .as_str()
                .expect("file_data should be string")
                .len()
                > 4
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn build_openai_user_content_parts_supports_image_detail_override() {
        let root = temp_blob_root();
        let toolkit = blob_toolkit(Some(&root)).expect("toolkit");
        let image_blob = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "img_0123456789abcdef.jpg".to_string(),
            size: 4,
            mime: "image/jpeg".to_string(),
            filename_original: "image.jpg".to_string(),
            spool_day: "2026-03-31".to_string(),
        };
        let image_path = toolkit.resolve(&image_blob);
        std::fs::create_dir_all(
            image_path
                .parent()
                .expect("image blob parent must exist for test setup"),
        )
        .expect("create blob parent");
        std::fs::write(&image_path, [0_u8, 1_u8, 2_u8, 3_u8]).expect("write image bytes");
        let input = ResolvedModelInput {
            main_text: "mira".to_string(),
            prompt_text: "mira".to_string(),
            attachments: vec![ResolvedModelAttachment {
                blob_ref: image_blob,
                path: image_path,
                text_content: None,
            }],
        };
        let opts = OpenAiUserContentOptions {
            image_detail: Some("high".to_string()),
        };
        let parts = build_openai_user_content_parts_with_options(&input, &opts)
            .await
            .expect("image with detail should work");
        assert_eq!(parts[1]["type"], "input_image");
        assert_eq!(parts[1]["detail"], "high");
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
