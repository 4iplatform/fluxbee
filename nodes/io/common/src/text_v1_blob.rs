#![forbid(unsafe_code)]

use fluxbee_sdk::blob::{BlobConfig, BlobError, BlobRef, BlobToolkit, ResolveRetryConfig};
use fluxbee_sdk::payload::{PayloadError, TextV1Payload, TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES};
use serde_json::Value;
use std::path::PathBuf;

const DEFAULT_MESSAGE_OVERHEAD_BYTES: usize = 2_048;
const DEFAULT_MAX_ATTACHMENTS: usize = 8;
const DEFAULT_MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct IoTextBlobConfig {
    pub max_message_bytes: usize,
    pub message_overhead_bytes: usize,
    pub max_attachments: usize,
    pub max_attachment_bytes: u64,
    pub max_total_attachment_bytes: u64,
    pub allowed_mimes: Vec<String>,
}

impl Default for IoTextBlobConfig {
    fn default() -> Self {
        Self {
            max_message_bytes: TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES,
            message_overhead_bytes: DEFAULT_MESSAGE_OVERHEAD_BYTES,
            max_attachments: DEFAULT_MAX_ATTACHMENTS,
            max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
            max_total_attachment_bytes: DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
            allowed_mimes: vec![
                "image/jpeg".to_string(),
                "image/png".to_string(),
                "image/webp".to_string(),
                "image/gif".to_string(),
                "application/pdf".to_string(),
                "text/plain".to_string(),
                "text/markdown".to_string(),
                "application/json".to_string(),
            ],
        }
    }
}

impl IoTextBlobConfig {
    pub fn validate(&self) -> Result<(), IoBlobContractError> {
        if self.max_message_bytes == 0 {
            return Err(IoBlobContractError::InvalidTextPayload(
                "max_message_bytes must be > 0".to_string(),
            ));
        }
        if self.message_overhead_bytes == 0 {
            return Err(IoBlobContractError::InvalidTextPayload(
                "message_overhead_bytes must be > 0".to_string(),
            ));
        }
        if self.max_attachments == 0 {
            return Err(IoBlobContractError::InvalidTextPayload(
                "max_attachments must be > 0".to_string(),
            ));
        }
        if self.max_attachment_bytes == 0 || self.max_total_attachment_bytes == 0 {
            return Err(IoBlobContractError::InvalidTextPayload(
                "attachment size limits must be > 0".to_string(),
            ));
        }
        if self.max_total_attachment_bytes < self.max_attachment_bytes {
            return Err(IoBlobContractError::InvalidTextPayload(
                "max_total_attachment_bytes must be >= max_attachment_bytes".to_string(),
            ));
        }
        if self.allowed_mimes.is_empty() {
            return Err(IoBlobContractError::InvalidTextPayload(
                "allowed_mimes must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct IoBlobRuntimeConfig {
    pub blob_root: PathBuf,
    pub max_blob_bytes: Option<u64>,
    pub text_v1: IoTextBlobConfig,
    pub resolve_retry: ResolveRetryConfig,
}

impl Default for IoBlobRuntimeConfig {
    fn default() -> Self {
        Self {
            blob_root: PathBuf::from("/var/lib/fluxbee/blob"),
            max_blob_bytes: None,
            text_v1: IoTextBlobConfig::default(),
            resolve_retry: ResolveRetryConfig::default(),
        }
    }
}

impl IoBlobRuntimeConfig {
    pub fn validate(&self) -> Result<(), IoBlobContractError> {
        if self.blob_root.as_os_str().is_empty() {
            return Err(IoBlobContractError::InvalidTextPayload(
                "blob_root must not be empty".to_string(),
            ));
        }
        self.text_v1.validate()?;
        Ok(())
    }

    pub fn build_toolkit(&self) -> Result<BlobToolkit, IoBlobContractError> {
        self.validate()?;
        BlobToolkit::new(BlobConfig {
            blob_root: self.blob_root.clone(),
            name_max_chars: fluxbee_sdk::blob::BLOB_NAME_MAX_CHARS,
            max_blob_bytes: self.max_blob_bytes,
        })
        .map_err(IoBlobContractError::from)
    }
}

#[derive(Debug, Clone)]
pub struct InboundAttachmentInput {
    pub blob_ref: BlobRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextV1NormalizationDecision {
    pub processed_as_blob: bool,
    pub inline_payload_bytes: usize,
    pub estimated_total_bytes: usize,
    pub max_message_bytes: usize,
    pub has_attachments: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedAttachment {
    pub blob_ref: BlobRef,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResolvedTextV1Payload {
    pub text: String,
    pub attachments: Vec<ResolvedAttachment>,
}

#[derive(Debug, thiserror::Error)]
pub enum IoBlobContractError {
    #[error("BLOB_TOO_LARGE: size={actual} max={max}")]
    BlobTooLarge { actual: u64, max: u64 },
    #[error("unsupported_attachment_mime: {mime}")]
    UnsupportedMime { mime: String },
    #[error("too_many_attachments: actual={actual} max={max}")]
    TooManyAttachments { actual: usize, max: usize },
    #[error("invalid_text_payload: {0}")]
    InvalidTextPayload(String),
    #[error(transparent)]
    Blob(#[from] BlobError),
    #[error(transparent)]
    Payload(#[from] PayloadError),
}

impl IoBlobContractError {
    pub fn canonical_code(&self) -> &'static str {
        match self {
            Self::BlobTooLarge { .. } => "BLOB_TOO_LARGE",
            Self::UnsupportedMime { .. } => "unsupported_attachment_mime",
            Self::TooManyAttachments { .. } => "too_many_attachments",
            Self::InvalidTextPayload(_) => "invalid_text_payload",
            Self::Blob(blob) => match blob {
                BlobError::NotFound(_) => "BLOB_NOT_FOUND",
                BlobError::Io(_) => "BLOB_IO_ERROR",
                BlobError::InvalidName(_) => "BLOB_INVALID_NAME",
                BlobError::TooLarge { .. } => "BLOB_TOO_LARGE",
                BlobError::InvalidRef(_) => "BLOB_INVALID_REF",
                BlobError::SyncHintTimeout { .. } => "BLOB_SYNC_HINT_TIMEOUT",
                BlobError::SyncHintFailed { .. } => "BLOB_SYNC_HINT_FAILED",
                BlobError::SyncHintTransport { .. } => "BLOB_SYNC_HINT_TRANSPORT",
                BlobError::NotImplemented => "BLOB_NOT_IMPLEMENTED",
            },
            Self::Payload(_) => "invalid_text_payload",
        }
    }
}

pub fn build_text_v1_inbound_payload(
    blob: &BlobToolkit,
    cfg: &IoTextBlobConfig,
    content: &str,
    attachments: Vec<InboundAttachmentInput>,
) -> Result<Value, IoBlobContractError> {
    cfg.validate()?;
    if attachments.len() > cfg.max_attachments {
        return Err(IoBlobContractError::TooManyAttachments {
            actual: attachments.len(),
            max: cfg.max_attachments,
        });
    }

    let allowed_mimes = cfg
        .allowed_mimes
        .iter()
        .map(|m| normalize_mime(m))
        .collect::<std::collections::HashSet<_>>();

    let mut total_size: u64 = 0;
    let mut blob_refs = Vec::with_capacity(attachments.len());
    for input in attachments {
        input.blob_ref.validate()?;
        if !allowed_mimes.contains(&normalize_mime(&input.blob_ref.mime)) {
            return Err(IoBlobContractError::UnsupportedMime {
                mime: input.blob_ref.mime.clone(),
            });
        }
        if input.blob_ref.size > cfg.max_attachment_bytes {
            return Err(IoBlobContractError::BlobTooLarge {
                actual: input.blob_ref.size,
                max: cfg.max_attachment_bytes,
            });
        }
        total_size = total_size.saturating_add(input.blob_ref.size);
        blob_refs.push(input.blob_ref);
    }

    if total_size > cfg.max_total_attachment_bytes {
        return Err(IoBlobContractError::BlobTooLarge {
            actual: total_size,
            max: cfg.max_total_attachment_bytes,
        });
    }

    let payload = blob.build_text_v1_payload_with_limit(
        content,
        blob_refs,
        cfg.max_message_bytes,
        cfg.message_overhead_bytes,
    )?;
    payload.to_value().map_err(IoBlobContractError::from)
}

pub fn normalize_text_v1_inbound_payload(
    blob: &BlobToolkit,
    cfg: &IoTextBlobConfig,
    payload: &Value,
) -> Result<(Value, TextV1NormalizationDecision), IoBlobContractError> {
    cfg.validate()?;
    let parsed = TextV1Payload::from_value(payload)?;
    let has_attachments = !parsed.attachments.is_empty();

    match (parsed.content.clone(), parsed.content_ref.clone()) {
        (Some(content), None) => {
            let inline_payload = TextV1Payload::new(&content, parsed.attachments.clone());
            let inline_payload_bytes = serde_json::to_vec(&inline_payload)
                .map_err(PayloadError::from)?
                .len();
            let estimated_total_bytes =
                inline_payload_bytes.saturating_add(cfg.message_overhead_bytes);
            let normalized = build_text_v1_inbound_payload(
                blob,
                cfg,
                &content,
                parsed
                    .attachments
                    .into_iter()
                    .map(|blob_ref| InboundAttachmentInput { blob_ref })
                    .collect(),
            )?;
            let normalized_parsed = TextV1Payload::from_value(&normalized)?;
            Ok((
                normalized,
                TextV1NormalizationDecision {
                    processed_as_blob: normalized_parsed.content_ref.is_some(),
                    inline_payload_bytes,
                    estimated_total_bytes,
                    max_message_bytes: cfg.max_message_bytes,
                    has_attachments,
                },
            ))
        }
        (None, Some(content_ref)) => {
            content_ref.validate()?;
            validate_blob_refs_limits(cfg, &parsed.attachments)?;
            let normalized = parsed.to_value()?;
            Ok((
                normalized,
                TextV1NormalizationDecision {
                    processed_as_blob: true,
                    inline_payload_bytes: 0,
                    estimated_total_bytes: 0,
                    max_message_bytes: cfg.max_message_bytes,
                    has_attachments,
                },
            ))
        }
        _ => Err(IoBlobContractError::InvalidTextPayload(
            "invalid text/v1 payload (content/content_ref)".to_string(),
        )),
    }
}

pub async fn resolve_text_v1_for_outbound(
    blob: &BlobToolkit,
    cfg: &IoTextBlobConfig,
    payload: &Value,
    use_retry: bool,
) -> Result<ResolvedTextV1Payload, IoBlobContractError> {
    cfg.validate()?;
    let parsed = TextV1Payload::from_value(payload)?;
    validate_blob_refs_limits(cfg, &parsed.attachments)?;

    let text = match (parsed.content.clone(), parsed.content_ref.clone()) {
        (Some(content), None) => content,
        (None, Some(content_ref)) => {
            content_ref.validate()?;
            let path = if use_retry {
                blob.resolve_with_retry(&content_ref, ResolveRetryConfig::default())
                    .await?
            } else {
                blob.resolve(&content_ref)
            };
            let bytes = std::fs::read(&path)
                .map_err(|err| BlobError::Io(format!("read content_ref file: {err}")))?;
            String::from_utf8(bytes).map_err(|_| {
                IoBlobContractError::InvalidTextPayload(
                    "content_ref is not valid UTF-8 text".to_string(),
                )
            })?
        }
        _ => {
            return Err(IoBlobContractError::InvalidTextPayload(
                "invalid text/v1 payload (content/content_ref)".to_string(),
            ))
        }
    };

    let mut attachments = Vec::with_capacity(parsed.attachments.len());
    for blob_ref in parsed.attachments {
        let path = if use_retry {
            blob.resolve_with_retry(&blob_ref, ResolveRetryConfig::default())
                .await?
        } else {
            blob.resolve(&blob_ref)
        };
        if !path.exists() {
            return Err(IoBlobContractError::Blob(BlobError::NotFound(
                blob_ref.blob_name.clone(),
            )));
        }
        attachments.push(ResolvedAttachment { blob_ref, path });
    }

    Ok(ResolvedTextV1Payload { text, attachments })
}

fn normalize_mime(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn validate_blob_refs_limits(
    cfg: &IoTextBlobConfig,
    refs: &[BlobRef],
) -> Result<(), IoBlobContractError> {
    if refs.len() > cfg.max_attachments {
        return Err(IoBlobContractError::TooManyAttachments {
            actual: refs.len(),
            max: cfg.max_attachments,
        });
    }

    let allowed_mimes = cfg
        .allowed_mimes
        .iter()
        .map(|m| normalize_mime(m))
        .collect::<std::collections::HashSet<_>>();

    let mut total_size: u64 = 0;
    for blob_ref in refs {
        blob_ref.validate()?;
        if !allowed_mimes.contains(&normalize_mime(&blob_ref.mime)) {
            return Err(IoBlobContractError::UnsupportedMime {
                mime: blob_ref.mime.clone(),
            });
        }
        if blob_ref.size > cfg.max_attachment_bytes {
            return Err(IoBlobContractError::BlobTooLarge {
                actual: blob_ref.size,
                max: cfg.max_attachment_bytes,
            });
        }
        total_size = total_size.saturating_add(blob_ref.size);
    }

    if total_size > cfg.max_total_attachment_bytes {
        return Err(IoBlobContractError::BlobTooLarge {
            actual: total_size,
            max: cfg.max_total_attachment_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::blob::{BlobConfig, BlobToolkit, BLOB_NAME_MAX_CHARS};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

    struct TestRoot {
        path: PathBuf,
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_toolkit() -> (BlobToolkit, TestRoot) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("io-common-text-v1-blob-{nanos}-{seq}"));
        let toolkit = BlobToolkit::new(BlobConfig {
            blob_root: root.clone(),
            name_max_chars: BLOB_NAME_MAX_CHARS,
            max_blob_bytes: None,
        })
        .expect("toolkit must be created");
        (toolkit, TestRoot { path: root })
    }

    fn sample_blob_ref() -> BlobRef {
        BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "sample_0123456789abcdef.png".to_string(),
            size: 100,
            mime: "image/png".to_string(),
            filename_original: "sample.png".to_string(),
            spool_day: "2026-03-27".to_string(),
        }
    }

    #[test]
    fn rejects_too_many_attachments() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig {
            max_attachments: 1,
            ..IoTextBlobConfig::default()
        };
        let attachments = vec![
            InboundAttachmentInput {
                blob_ref: sample_blob_ref(),
            },
            InboundAttachmentInput {
                blob_ref: sample_blob_ref(),
            },
        ];

        let err = build_text_v1_inbound_payload(&toolkit, &cfg, "hola", attachments)
            .expect_err("must fail by max attachments");
        assert!(matches!(
            err,
            IoBlobContractError::TooManyAttachments { actual: 2, max: 1 }
        ));
    }

    #[test]
    fn rejects_unsupported_mime() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig::default();
        let attachment = InboundAttachmentInput {
            blob_ref: BlobRef {
                mime: "application/zip".to_string(),
                ..sample_blob_ref()
            },
        };
        let err = build_text_v1_inbound_payload(&toolkit, &cfg, "hola", vec![attachment])
            .expect_err("must fail unsupported mime");
        assert!(matches!(err, IoBlobContractError::UnsupportedMime { .. }));
    }

    #[test]
    fn rejects_total_size_limit() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig {
            max_total_attachment_bytes: 150,
            ..IoTextBlobConfig::default()
        };
        let a = InboundAttachmentInput {
            blob_ref: BlobRef {
                size: 100,
                ..sample_blob_ref()
            },
        };
        let b = InboundAttachmentInput {
            blob_ref: BlobRef {
                size: 100,
                ..sample_blob_ref()
            },
        };
        let err = build_text_v1_inbound_payload(&toolkit, &cfg, "hola", vec![a, b])
            .expect_err("must fail total size");
        assert!(matches!(
            err,
            IoBlobContractError::BlobTooLarge {
                actual: 200,
                max: 150
            }
        ));
    }

    #[test]
    fn offloads_large_content_to_content_ref() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig {
            max_message_bytes: 256,
            message_overhead_bytes: 128,
            ..IoTextBlobConfig::default()
        };
        let content = "x".repeat(4_096);
        let payload = build_text_v1_inbound_payload(&toolkit, &cfg, &content, vec![])
            .expect("payload should be built");
        let parsed = TextV1Payload::from_value(&payload).expect("parse text_v1");
        assert!(parsed.content.is_none());
        assert!(parsed.content_ref.is_some());
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn keeps_inline_content_when_it_fits() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig::default();
        let payload = build_text_v1_inbound_payload(&toolkit, &cfg, "hola", vec![])
            .expect("payload should be built");
        let parsed = TextV1Payload::from_value(&payload).expect("parse text_v1");
        assert_eq!(parsed.content.as_deref(), Some("hola"));
        assert!(parsed.content_ref.is_none());
    }

    #[test]
    fn normalize_text_v1_keeps_inline_under_limit() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig::default();
        let payload = TextV1Payload::new("hola", vec![])
            .to_value()
            .expect("payload value");

        let (normalized, decision) =
            normalize_text_v1_inbound_payload(&toolkit, &cfg, &payload).expect("normalize");
        let parsed = TextV1Payload::from_value(&normalized).expect("parse normalized");
        assert_eq!(parsed.content.as_deref(), Some("hola"));
        assert!(parsed.content_ref.is_none());
        assert!(!decision.processed_as_blob);
        assert!(decision.inline_payload_bytes > 0);
    }

    #[test]
    fn normalize_text_v1_offloads_over_limit() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig {
            max_message_bytes: 256,
            message_overhead_bytes: 128,
            ..IoTextBlobConfig::default()
        };
        let content = "x".repeat(1024);
        let payload = TextV1Payload::new(&content, vec![])
            .to_value()
            .expect("payload value");

        let (normalized, decision) =
            normalize_text_v1_inbound_payload(&toolkit, &cfg, &payload).expect("normalize");
        let parsed = TextV1Payload::from_value(&normalized).expect("parse normalized");
        assert!(parsed.content_ref.is_some());
        assert!(decision.processed_as_blob);
        assert!(decision.estimated_total_bytes > cfg.max_message_bytes);
    }

    #[test]
    fn canonical_code_mapping_is_stable() {
        let a = IoBlobContractError::UnsupportedMime {
            mime: "application/zip".to_string(),
        };
        assert_eq!(a.canonical_code(), "unsupported_attachment_mime");
        let b = IoBlobContractError::TooManyAttachments { actual: 9, max: 8 };
        assert_eq!(b.canonical_code(), "too_many_attachments");
        let c = IoBlobContractError::Blob(BlobError::NotFound("x".to_string()));
        assert_eq!(c.canonical_code(), "BLOB_NOT_FOUND");
    }

    #[test]
    fn config_validation_rejects_invalid_limits() {
        let cfg = IoTextBlobConfig {
            max_attachment_bytes: 10,
            max_total_attachment_bytes: 5,
            ..IoTextBlobConfig::default()
        };
        let err = cfg.validate().expect_err("invalid cfg must fail");
        assert_eq!(err.canonical_code(), "invalid_text_payload");
    }

    #[test]
    fn runtime_config_builds_toolkit() {
        let mut runtime = IoBlobRuntimeConfig::default();
        runtime.blob_root = std::env::temp_dir().join("io-common-runtime-blob-root");
        runtime.max_blob_bytes = Some(50 * 1024 * 1024);
        let toolkit = runtime.build_toolkit().expect("runtime must build toolkit");
        let _ = toolkit;
    }

    #[tokio::test]
    async fn resolves_content_ref_for_outbound() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig::default();
        let content_ref = toolkit
            .put_bytes(b"contenido desde blob", "content.txt", "text/plain")
            .expect("put content ref");
        toolkit.promote(&content_ref).expect("promote content ref");
        let payload = TextV1Payload::with_content_ref(content_ref, vec![])
            .to_value()
            .expect("payload value");

        let resolved = resolve_text_v1_for_outbound(&toolkit, &cfg, &payload, false)
            .await
            .expect("resolve outbound");
        assert_eq!(resolved.text, "contenido desde blob");
        assert!(resolved.attachments.is_empty());
    }

    #[tokio::test]
    async fn resolves_attachments_for_outbound() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig::default();
        let attachment = toolkit
            .put_bytes(b"attachment-data", "a.png", "image/png")
            .expect("put attachment");
        toolkit.promote(&attachment).expect("promote attachment");

        let payload = TextV1Payload::new("hola", vec![attachment.clone()])
            .to_value()
            .expect("payload value");

        let resolved = resolve_text_v1_for_outbound(&toolkit, &cfg, &payload, false)
            .await
            .expect("resolve outbound");
        assert_eq!(resolved.text, "hola");
        assert_eq!(resolved.attachments.len(), 1);
        assert_eq!(resolved.attachments[0].blob_ref, attachment);
        assert!(resolved.attachments[0].path.exists());
    }

    #[tokio::test]
    async fn outbound_rejects_unsupported_attachment_mime() {
        let (toolkit, _root) = test_toolkit();
        let cfg = IoTextBlobConfig::default();
        let payload = TextV1Payload::new(
            "hola",
            vec![BlobRef {
                mime: "application/zip".to_string(),
                ..sample_blob_ref()
            }],
        )
        .to_value()
        .expect("payload value");

        let err = resolve_text_v1_for_outbound(&toolkit, &cfg, &payload, false)
            .await
            .expect_err("must reject unsupported mime");
        assert!(matches!(err, IoBlobContractError::UnsupportedMime { .. }));
    }
}
