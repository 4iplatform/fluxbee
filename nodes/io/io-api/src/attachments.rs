use axum::extract::Multipart;
use axum::http::StatusCode;
use fluxbee_sdk::blob::BlobToolkit;
use io_common::text_v1_blob::{InboundAttachmentInput, IoBlobContractError, IoTextBlobConfig};
use serde_json::Value;

pub(crate) async fn collect_multipart_blob_attachments(
    multipart: &mut Multipart,
    blob_toolkit: &BlobToolkit,
    cfg: &IoTextBlobConfig,
) -> std::result::Result<(Value, Vec<InboundAttachmentInput>), (StatusCode, &'static str, String)> {
    let mut metadata: Option<Value> = None;
    let mut attachments = Vec::new();
    let allowed_mimes = cfg
        .allowed_mimes
        .iter()
        .map(|mime| normalize_mime(mime))
        .collect::<std::collections::HashSet<_>>();
    let mut total_size: u64 = 0;

    while let Some(field) = multipart.next_field().await.map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            "invalid_multipart",
            format!("Invalid multipart field: {err}"),
        )
    })? {
        let name = field.name().unwrap_or("").to_string();
        if name == "metadata" {
            let bytes = field.bytes().await.map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    "invalid_multipart",
                    format!("Cannot read metadata part: {err}"),
                )
            })?;
            let value = serde_json::from_slice::<Value>(&bytes).map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    "invalid_json",
                    format!("metadata part is not valid JSON: {err}"),
                )
            })?;
            metadata = Some(value);
            continue;
        }

        if attachments.len() >= cfg.max_attachments {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_many_attachments",
                format!("Too many attachments: max {}", cfg.max_attachments),
            ));
        }

        let filename = field.file_name().unwrap_or("attachment.bin").to_string();
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        if !allowed_mimes.contains(&normalize_mime(&mime)) {
            return Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_attachment_mime",
                format!("Unsupported attachment mime: {mime}"),
            ));
        }

        let bytes = field.bytes().await.map_err(|err| {
            (
                StatusCode::BAD_REQUEST,
                "invalid_multipart",
                format!("Cannot read attachment bytes: {err}"),
            )
        })?;
        let size = bytes.len() as u64;
        if size > cfg.max_attachment_bytes {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "attachment_too_large",
                format!(
                    "Attachment exceeds max size of {}",
                    cfg.max_attachment_bytes
                ),
            ));
        }
        if total_size.saturating_add(size) > cfg.max_total_attachment_bytes {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "attachments_too_large",
                format!(
                    "Total attachment bytes exceed max {}",
                    cfg.max_total_attachment_bytes
                ),
            ));
        }

        let blob_ref = blob_toolkit
            .put_bytes(&bytes, &filename, &mime)
            .map_err(|err| {
                let code = match err {
                    fluxbee_sdk::blob::BlobError::TooLarge { .. } => "attachment_too_large",
                    _ => "blob_put_error",
                };
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    code,
                    format!("Failed to materialize attachment: {err}"),
                )
            })?;
        blob_toolkit.promote(&blob_ref).map_err(|err| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "blob_promote_error",
                format!("Failed to promote attachment blob: {err}"),
            )
        })?;
        tracing::debug!(
            blob_name = %blob_ref.blob_name,
            filename = %blob_ref.filename_original,
            mime = %blob_ref.mime,
            size = blob_ref.size,
            spool_day = %blob_ref.spool_day,
            "io-api attachment materialized"
        );

        total_size = total_size.saturating_add(size);
        attachments.push(InboundAttachmentInput { blob_ref });
    }

    let metadata = metadata.ok_or_else(|| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            "multipart/form-data requires a metadata JSON part".to_string(),
        )
    })?;
    Ok((metadata, attachments))
}

pub(crate) fn effective_blob_payload_cfg(
    effective_config: Option<&Value>,
    defaults: &IoTextBlobConfig,
) -> IoTextBlobConfig {
    let mut cfg = defaults.clone();
    let Some(ingress) = effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(Value::as_object)
    else {
        return cfg;
    };
    if let Some(value) = ingress
        .get("max_attachments_per_request")
        .and_then(Value::as_u64)
    {
        if let Ok(value) = usize::try_from(value) {
            cfg.max_attachments = value;
        }
    }
    if let Some(value) = ingress
        .get("max_attachment_size_bytes")
        .and_then(Value::as_u64)
    {
        cfg.max_attachment_bytes = value;
    }
    if let Some(value) = ingress
        .get("max_total_attachment_bytes")
        .and_then(Value::as_u64)
    {
        cfg.max_total_attachment_bytes = value;
    }
    if let Some(values) = ingress.get("allowed_mime_types").and_then(Value::as_array) {
        let parsed = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if !parsed.is_empty() {
            cfg.allowed_mimes = parsed;
        }
    }
    cfg
}

fn normalize_mime(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

pub(crate) fn map_blob_error_to_http(
    err: &IoBlobContractError,
) -> (StatusCode, &'static str, String) {
    match err {
        IoBlobContractError::UnsupportedMime { .. } => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            err.canonical_code(),
            err.to_string(),
        ),
        IoBlobContractError::BlobTooLarge { .. }
        | IoBlobContractError::TooManyAttachments { .. } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            err.canonical_code(),
            err.to_string(),
        ),
        _ => (
            StatusCode::UNPROCESSABLE_ENTITY,
            err.canonical_code(),
            err.to_string(),
        ),
    }
}
