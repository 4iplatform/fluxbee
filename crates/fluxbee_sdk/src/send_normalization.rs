use std::path::PathBuf;

use crate::blob::{BlobConfig, BlobToolkit, TEXT_V1_DEFAULT_OVERHEAD_BYTES};
use crate::payload::{PayloadError, TextV1Payload, TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES};
use crate::protocol::Message;

#[derive(Debug, Clone)]
struct SendNormalizationConfig {
    enabled: bool,
    blob_root: PathBuf,
    max_blob_bytes: Option<u64>,
    max_message_bytes: usize,
    message_overhead_bytes: usize,
}

impl Default for SendNormalizationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            blob_root: PathBuf::from("/var/lib/fluxbee/blob"),
            max_blob_bytes: None,
            max_message_bytes: TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES,
            message_overhead_bytes: TEXT_V1_DEFAULT_OVERHEAD_BYTES,
        }
    }
}

impl SendNormalizationConfig {
    fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Some(v) = std::env::var_os("FLUXBEE_DISABLE_AUTO_BLOB_SEND") {
            cfg.enabled = !looks_true(&v.to_string_lossy());
        }

        if let Some(v) = std::env::var_os("FLUXBEE_BLOB_ROOT") {
            let root = PathBuf::from(v);
            if !root.as_os_str().is_empty() {
                cfg.blob_root = root;
            }
        }

        if let Some(parsed) = parse_u64_env("FLUXBEE_BLOB_MAX_BYTES") {
            cfg.max_blob_bytes = Some(parsed);
        }

        if let Some(parsed) = parse_usize_env("FLUXBEE_TEXT_V1_MAX_MESSAGE_BYTES") {
            cfg.max_message_bytes = parsed;
        }

        if let Some(parsed) = parse_usize_env("FLUXBEE_TEXT_V1_MESSAGE_OVERHEAD_BYTES") {
            cfg.message_overhead_bytes = parsed;
        }

        cfg
    }
}

fn looks_true(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_usize_env(key: &str) -> Option<usize> {
    let raw = std::env::var(key).ok()?;
    let parsed = raw.trim().parse::<usize>().ok()?;
    if parsed == 0 {
        tracing::warn!(env = key, value = %raw, "ignoring invalid zero env override");
        return None;
    }
    Some(parsed)
}

fn parse_u64_env(key: &str) -> Option<u64> {
    let raw = std::env::var(key).ok()?;
    let parsed = raw.trim().parse::<u64>().ok()?;
    if parsed == 0 {
        tracing::warn!(env = key, value = %raw, "ignoring invalid zero env override");
        return None;
    }
    Some(parsed)
}

fn payload_error_code(error: &PayloadError) -> &'static str {
    match error {
        PayloadError::InvalidContract(_) => "invalid_text_payload",
        PayloadError::Json(_) => "invalid_text_payload",
        PayloadError::Blob(err) => match err {
            crate::blob::BlobError::NotFound(_) => "BLOB_NOT_FOUND",
            crate::blob::BlobError::Io(_) => "BLOB_IO_ERROR",
            crate::blob::BlobError::InvalidName(_) => "BLOB_INVALID_NAME",
            crate::blob::BlobError::InvalidRef(_) => "BLOB_INVALID_REF",
            crate::blob::BlobError::TooLarge { .. } => "BLOB_TOO_LARGE",
            crate::blob::BlobError::SyncHintTimeout { .. } => "BLOB_SYNC_HINT_TIMEOUT",
            crate::blob::BlobError::SyncHintFailed { .. } => "BLOB_SYNC_HINT_FAILED",
            crate::blob::BlobError::SyncHintTransport { .. } => "BLOB_SYNC_HINT_TRANSPORT",
            crate::blob::BlobError::NotImplemented => "BLOB_NOT_IMPLEMENTED",
        },
    }
}

fn normalize_text_payload(
    trace_id: &str,
    cfg: &SendNormalizationConfig,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let parsed = match TextV1Payload::from_value(payload) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                trace_id = trace_id,
                canonical_code = payload_error_code(&err),
                error_detail = %err,
                "sdk send text/v1 normalization failed; forwarding payload as-is"
            );
            return payload.clone();
        }
    };

    if parsed.content_ref.is_some() {
        return payload.clone();
    }

    let Some(content) = parsed.content.as_deref() else {
        return payload.clone();
    };

    let toolkit = match BlobToolkit::new(BlobConfig {
        blob_root: cfg.blob_root.clone(),
        name_max_chars: crate::blob::BLOB_NAME_MAX_CHARS,
        max_blob_bytes: cfg.max_blob_bytes,
    }) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                trace_id = trace_id,
                canonical_code = "invalid_text_payload",
                error_detail = %err,
                "sdk send text/v1 normalization skipped: invalid blob runtime config"
            );
            return payload.clone();
        }
    };

    let inline_payload = TextV1Payload::new(content, parsed.attachments.clone());
    let inline_payload_bytes = match serde_json::to_vec(&inline_payload) {
        Ok(bytes) => bytes.len(),
        Err(err) => {
            tracing::warn!(
                trace_id = trace_id,
                canonical_code = "invalid_text_payload",
                error_detail = %err,
                "sdk send text/v1 normalization failed while estimating inline bytes"
            );
            return payload.clone();
        }
    };
    let estimated_total_bytes = inline_payload_bytes.saturating_add(cfg.message_overhead_bytes);

    let normalized = match toolkit.build_text_v1_payload_with_limit(
        content,
        parsed.attachments.clone(),
        cfg.max_message_bytes,
        cfg.message_overhead_bytes,
    ) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                trace_id = trace_id,
                canonical_code = payload_error_code(&err),
                error_detail = %err,
                "sdk send text/v1 normalization failed; forwarding payload as-is"
            );
            return payload.clone();
        }
    };

    let offload_to_blob = normalized.content_ref.is_some();
    tracing::debug!(
        trace_id = trace_id,
        text_v1_normalized = true,
        offload_to_blob = offload_to_blob,
        inline_bytes = inline_payload_bytes,
        estimated_total_bytes = estimated_total_bytes,
        max_message_bytes = cfg.max_message_bytes,
        has_attachments = !normalized.attachments.is_empty(),
        "sdk send text/v1 normalized"
    );
    if let Some(content_ref) = normalized.content_ref.as_ref() {
        tracing::info!(
            trace_id = trace_id,
            reason = "message_over_limit_or_content_ref",
            blob_name = %content_ref.blob_name,
            blob_size = content_ref.size,
            "sdk send text/v1 processed as blob"
        );
    }

    match normalized.to_value() {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                trace_id = trace_id,
                canonical_code = payload_error_code(&err),
                error_detail = %err,
                "sdk send text/v1 normalization failed while encoding; forwarding payload as-is"
            );
            payload.clone()
        }
    }
}

pub(crate) fn normalize_outbound_message(mut msg: Message) -> Message {
    let cfg = SendNormalizationConfig::from_env();

    if !cfg.enabled {
        return msg;
    }

    let payload_type = msg
        .payload
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if payload_type != "text" {
        return msg;
    }

    let trace_id = msg.routing.trace_id.clone();
    msg.payload = normalize_text_payload(&trace_id, &cfg, &msg.payload);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::BlobRef;
    use crate::protocol::{Destination, Meta, Routing};
    use serde_json::json;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn test_message(payload: serde_json::Value) -> Message {
        Message {
            routing: Routing {
                src: "src-node".to_string(),
                dst: Destination::Unicast("dst-node".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                ..Meta::default()
            },
            payload,
        }
    }

    fn test_blob_root() -> PathBuf {
        let path = std::env::temp_dir().join(format!("fluxbee-send-normalize-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp blob root");
        path
    }

    #[test]
    fn passthrough_non_text_payload() {
        let msg = test_message(json!({"type":"admin", "content":"noop"}));
        let normalized = normalize_outbound_message(msg.clone());
        assert_eq!(normalized.payload, msg.payload);
    }

    #[test]
    fn keeps_inline_text_under_limit() {
        let payload = json!({"type":"text","content":"hola","attachments":[]});
        let cfg = SendNormalizationConfig {
            enabled: true,
            blob_root: test_blob_root(),
            max_blob_bytes: None,
            max_message_bytes: 64 * 1024,
            message_overhead_bytes: 2048,
        };
        let normalized = normalize_text_payload("trace-test", &cfg, &payload);
        let parsed = TextV1Payload::from_value(&normalized).expect("parse normalized text payload");
        assert!(parsed.content.is_some());
        assert!(parsed.content_ref.is_none());
    }

    #[test]
    fn offloads_text_over_limit_to_content_ref() {
        let payload = json!({"type":"text","content":"A".repeat(5000),"attachments":[]});
        let cfg = SendNormalizationConfig {
            enabled: true,
            blob_root: test_blob_root(),
            max_blob_bytes: None,
            max_message_bytes: 1024,
            message_overhead_bytes: 128,
        };
        let normalized = normalize_text_payload("trace-test", &cfg, &payload);
        let parsed = TextV1Payload::from_value(&normalized).expect("parse normalized text payload");
        assert!(parsed.content.is_none());
        assert!(parsed.content_ref.is_some());
    }

    #[test]
    fn keeps_existing_content_ref_unchanged() {
        let content_ref = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "content_0123456789abcdef.txt".to_string(),
            size: 10,
            mime: "text/plain".to_string(),
            filename_original: "content.txt".to_string(),
            spool_day: "2026-04-01".to_string(),
        };
        let payload = TextV1Payload::with_content_ref(content_ref.clone(), vec![])
            .to_value()
            .expect("to value");
        let cfg = SendNormalizationConfig {
            enabled: true,
            blob_root: test_blob_root(),
            max_blob_bytes: None,
            max_message_bytes: 1024,
            message_overhead_bytes: 128,
        };
        let normalized = normalize_text_payload("trace-test", &cfg, &payload);
        let parsed = TextV1Payload::from_value(&normalized).expect("parse normalized text payload");
        assert!(parsed.content.is_none());
        assert_eq!(parsed.content_ref.as_ref(), Some(&content_ref));
    }
}
