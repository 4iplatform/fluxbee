use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::node_client::NodeError;
use crate::payload::{PayloadError, TextV1Payload, TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES};
use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::split::{NodeReceiver, NodeSender};

pub mod constants {
    pub const BLOB_NAME_MAX_CHARS: usize = 128;
    pub const BLOB_HASH_LEN: usize = 16;
    pub const BLOB_PREFIX_LEN: usize = 2;
    pub const BLOB_NAME_ALLOWED: &str =
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-";
    pub const BLOB_RETRY_MAX_MS: u64 = 5_000;
    pub const BLOB_RETRY_INITIAL_MS: u64 = 100;
    pub const BLOB_RETRY_BACKOFF: f64 = 2.0;
    pub const BLOB_STAGING_TTL_HOURS: u64 = 24;
    pub const BLOB_ACTIVE_RETAIN_DAYS: u64 = 30;
    pub const BLOB_DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;
    pub const BLOB_MAX_EXT_CHARS: usize = 10;
    pub const TEXT_V1_DEFAULT_OVERHEAD_BYTES: usize = 2_048;
    pub const BLOB_SYNC_HINT_DEFAULT_TIMEOUT_MS: u64 = 30_000;
    pub const BLOB_SYNC_HINT_MAX_TIMEOUT_MS: u64 = 300_000;
    pub const BLOB_SYNC_HINT_RETRY_INTERVAL_MS: u64 = 500;
}

pub use constants::{
    BLOB_ACTIVE_RETAIN_DAYS, BLOB_DEFAULT_MAX_BYTES, BLOB_HASH_LEN, BLOB_MAX_EXT_CHARS,
    BLOB_NAME_ALLOWED, BLOB_NAME_MAX_CHARS, BLOB_PREFIX_LEN, BLOB_RETRY_BACKOFF,
    BLOB_RETRY_INITIAL_MS, BLOB_RETRY_MAX_MS, BLOB_STAGING_TTL_HOURS,
    BLOB_SYNC_HINT_DEFAULT_TIMEOUT_MS, BLOB_SYNC_HINT_MAX_TIMEOUT_MS,
    BLOB_SYNC_HINT_RETRY_INTERVAL_MS, TEXT_V1_DEFAULT_OVERHEAD_BYTES,
};

#[derive(Debug, Clone)]
pub struct BlobConfig {
    pub blob_root: PathBuf,
    pub name_max_chars: usize,
    pub max_blob_bytes: Option<u64>,
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            blob_root: PathBuf::from("/var/lib/fluxbee/blob"),
            name_max_chars: BLOB_NAME_MAX_CHARS,
            max_blob_bytes: Some(BLOB_DEFAULT_MAX_BYTES),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResolveRetryConfig {
    pub max_wait_ms: u64,
    pub initial_delay_ms: u64,
    pub backoff_factor: f64,
}

impl Default for ResolveRetryConfig {
    fn default() -> Self {
        Self {
            max_wait_ms: BLOB_RETRY_MAX_MS,
            initial_delay_ms: BLOB_RETRY_INITIAL_MS,
            backoff_factor: BLOB_RETRY_BACKOFF,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobRef {
    #[serde(rename = "type")]
    pub ref_type: String,
    pub blob_name: String,
    pub size: u64,
    pub mime: String,
    pub filename_original: String,
    pub spool_day: String,
}

impl BlobRef {
    pub fn validate(&self) -> Result<(), BlobError> {
        if self.ref_type != "blob_ref" {
            return Err(BlobError::InvalidRef(format!(
                "field type must be 'blob_ref' (got '{}')",
                self.ref_type
            )));
        }
        validate_blob_name(&self.blob_name)?;
        if self.mime.trim().is_empty() {
            return Err(BlobError::InvalidRef("field mime is required".to_string()));
        }
        if self.filename_original.trim().is_empty() {
            return Err(BlobError::InvalidRef(
                "field filename_original is required".to_string(),
            ));
        }
        if !is_iso_day(&self.spool_day) {
            return Err(BlobError::InvalidRef(
                "field spool_day must be YYYY-MM-DD".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BlobStat {
    pub size_on_disk: u64,
    pub exists: bool,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct BlobGcOptions {
    pub staging_ttl_hours: u64,
    pub active_retain_days: u64,
    pub apply: bool,
}

impl Default for BlobGcOptions {
    fn default() -> Self {
        Self {
            staging_ttl_hours: BLOB_STAGING_TTL_HOURS,
            active_retain_days: BLOB_ACTIVE_RETAIN_DAYS,
            apply: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlobGcPassReport {
    pub apply: bool,
    pub scanned_files: u64,
    pub candidate_files: u64,
    pub deleted_files: u64,
    pub deleted_bytes: u64,
    pub skipped_non_blob_files: u64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlobGcReport {
    pub apply: bool,
    pub staging_ttl_hours: u64,
    pub active_retain_days: u64,
    pub staging: BlobGcPassReport,
    pub active: BlobGcPassReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BlobMetricsSnapshot {
    pub blob_put_total: u64,
    pub blob_put_bytes_total: u64,
    pub blob_resolve_total: u64,
    pub blob_resolve_retry_total: u64,
    pub blob_errors_total: u64,
}

#[derive(Debug, Clone)]
pub struct PublishBlobRequest<'a> {
    pub data: &'a [u8],
    pub filename_original: &'a str,
    pub mime: &'a str,
    pub targets: Vec<String>,
    pub wait_for_idle: bool,
    pub timeout_ms: u64,
}

impl<'a> PublishBlobRequest<'a> {
    pub fn new(
        data: &'a [u8],
        filename_original: &'a str,
        mime: &'a str,
        targets: Vec<String>,
    ) -> Self {
        Self {
            data,
            filename_original,
            mime,
            targets,
            wait_for_idle: true,
            timeout_ms: BLOB_SYNC_HINT_DEFAULT_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncHintTargetResult {
    pub target: String,
    pub status: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishBlobResult {
    pub blob_ref: BlobRef,
    pub timeout_ms: u64,
    pub targets: Vec<SyncHintTargetResult>,
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("BLOB_NOT_FOUND: {0}")]
    NotFound(String),
    #[error("BLOB_IO_ERROR: {0}")]
    Io(String),
    #[error("BLOB_INVALID_NAME: {0}")]
    InvalidName(String),
    #[error("BLOB_INVALID_REF: {0}")]
    InvalidRef(String),
    #[error("BLOB_TOO_LARGE: size={size} max={max}")]
    TooLarge { size: u64, max: u64 },
    #[error("BLOB_SYNC_HINT_TIMEOUT: target={target} timeout_ms={timeout_ms}")]
    SyncHintTimeout { target: String, timeout_ms: u64 },
    #[error("BLOB_SYNC_HINT_FAILED: target={target} detail={detail}")]
    SyncHintFailed { target: String, detail: String },
    #[error("BLOB_SYNC_HINT_TRANSPORT: target={target} detail={detail}")]
    SyncHintTransport { target: String, detail: String },
    #[error("BLOB_NOT_IMPLEMENTED")]
    NotImplemented,
}

#[derive(Debug, Clone)]
pub struct BlobToolkit {
    cfg: BlobConfig,
}

static BLOB_PUT_TOTAL: AtomicU64 = AtomicU64::new(0);
static BLOB_PUT_BYTES_TOTAL: AtomicU64 = AtomicU64::new(0);
static BLOB_RESOLVE_TOTAL: AtomicU64 = AtomicU64::new(0);
static BLOB_RESOLVE_RETRY_TOTAL: AtomicU64 = AtomicU64::new(0);
static BLOB_ERRORS_TOTAL: AtomicU64 = AtomicU64::new(0);

impl BlobToolkit {
    pub fn metrics_snapshot() -> BlobMetricsSnapshot {
        BlobMetricsSnapshot {
            blob_put_total: BLOB_PUT_TOTAL.load(Ordering::Relaxed),
            blob_put_bytes_total: BLOB_PUT_BYTES_TOTAL.load(Ordering::Relaxed),
            blob_resolve_total: BLOB_RESOLVE_TOTAL.load(Ordering::Relaxed),
            blob_resolve_retry_total: BLOB_RESOLVE_RETRY_TOTAL.load(Ordering::Relaxed),
            blob_errors_total: BLOB_ERRORS_TOTAL.load(Ordering::Relaxed),
        }
    }

    pub fn new(cfg: BlobConfig) -> Result<Self, BlobError> {
        if cfg.blob_root.as_os_str().is_empty() {
            return Err(BlobError::InvalidRef("blob_root is required".to_string()));
        }
        if cfg.name_max_chars == 0 {
            return Err(BlobError::InvalidRef(
                "name_max_chars must be > 0".to_string(),
            ));
        }
        if cfg.max_blob_bytes.is_some_and(|max| max == 0) {
            return Err(BlobError::InvalidRef(
                "max_blob_bytes must be > 0 when configured".to_string(),
            ));
        }
        Ok(Self { cfg })
    }

    pub fn validate_blob_name(blob_name: &str) -> Result<(), BlobError> {
        validate_blob_name(blob_name)
    }

    pub fn validate_blob_ref(blob_ref: &BlobRef) -> Result<(), BlobError> {
        blob_ref.validate()
    }

    pub fn put(&self, source_path: &Path, original_filename: &str) -> Result<BlobRef, BlobError> {
        let result = (|| -> Result<BlobRef, BlobError> {
            if let Some(source_size) = std::fs::metadata(source_path).ok().map(|m| m.len()) {
                self.enforce_size_limit(source_size)?;
            }
            let data =
                std::fs::read(source_path).map_err(|err| map_io_error(err, "read source file"))?;
            self.enforce_size_limit(data.len() as u64)?;
            let fallback_name = source_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("blob.bin");
            let requested_name = if original_filename.trim().is_empty() {
                fallback_name
            } else {
                original_filename
            };
            let (blob_ref, staging_path) =
                self.build_blob_ref_and_staging_path(&data, requested_name, None)?;
            std::fs::write(&staging_path, data)
                .map_err(|err| map_io_error(err, "write staging file"))?;
            set_file_mode_0640(&staging_path)?;
            Ok(blob_ref)
        })();
        match result {
            Ok(blob_ref) => {
                metrics_record_put(blob_ref.size);
                Ok(blob_ref)
            }
            Err(err) => {
                metrics_record_error();
                Err(err)
            }
        }
    }

    pub fn put_bytes(
        &self,
        data: &[u8],
        original_filename: &str,
        mime: &str,
    ) -> Result<BlobRef, BlobError> {
        let result = (|| -> Result<BlobRef, BlobError> {
            self.enforce_size_limit(data.len() as u64)?;
            let fallback_name = if original_filename.trim().is_empty() {
                "blob.bin"
            } else {
                original_filename
            };
            let mime_override = if mime.trim().is_empty() {
                None
            } else {
                Some(mime)
            };
            let (blob_ref, staging_path) =
                self.build_blob_ref_and_staging_path(data, fallback_name, mime_override)?;
            std::fs::write(&staging_path, data)
                .map_err(|err| map_io_error(err, "write staging file"))?;
            set_file_mode_0640(&staging_path)?;
            Ok(blob_ref)
        })();
        match result {
            Ok(blob_ref) => {
                metrics_record_put(blob_ref.size);
                Ok(blob_ref)
            }
            Err(err) => {
                metrics_record_error();
                Err(err)
            }
        }
    }

    /// Publica un blob local (`put_bytes` + `promote`) y confirma convergencia por target
    /// usando `SYSTEM_SYNC_HINT`.
    ///
    /// Nota: este helper consume mensajes del `NodeReceiver` mientras espera respuestas por
    /// `trace_id`; para evitar interferencias, se recomienda usar un receptor dedicado de control.
    pub async fn publish_blob_and_confirm(
        &self,
        sender: &NodeSender,
        receiver: &mut NodeReceiver,
        request: PublishBlobRequest<'_>,
    ) -> Result<PublishBlobResult, BlobError> {
        if request.targets.is_empty() {
            return Err(BlobError::InvalidRef(
                "publish_blob_and_confirm requires at least one target".to_string(),
            ));
        }

        let timeout_ms = request.timeout_ms.max(1).min(BLOB_SYNC_HINT_MAX_TIMEOUT_MS);

        let blob_ref = self.put_bytes(request.data, request.filename_original, request.mime)?;
        self.promote(&blob_ref)?;

        let mut target_results = Vec::with_capacity(request.targets.len());
        for raw_target in request.targets {
            let target = normalize_orchestrator_target(&raw_target)?;
            let started = Instant::now();
            let deadline = started + Duration::from_millis(timeout_ms);

            loop {
                let now = Instant::now();
                if now >= deadline {
                    return Err(BlobError::SyncHintTimeout { target, timeout_ms });
                }
                let remaining = deadline - now;
                let remaining_ms = remaining.as_millis().min(u128::from(u64::MAX)) as u64;
                let attempt_timeout_ms = remaining_ms.max(1).min(BLOB_SYNC_HINT_MAX_TIMEOUT_MS);

                let trace_id = Uuid::new_v4().to_string();
                let payload = json!({
                    "channel": "blob",
                    "folder_id": "fluxbee-blob",
                    "wait_for_idle": request.wait_for_idle,
                    "timeout_ms": attempt_timeout_ms,
                });
                let message = Message {
                    routing: Routing {
                        src: sender.uuid().to_string(),
                        dst: Destination::Unicast(target.clone()),
                        ttl: 16,
                        trace_id: trace_id.clone(),
                    },
                    meta: Meta {
                        msg_type: SYSTEM_KIND.to_string(),
                        msg: Some("SYSTEM_SYNC_HINT".to_string()),
                        scope: None,
                        target: None,
                        action: None,
                        priority: None,
                        context: None,
                    },
                    payload,
                };

                sender
                    .send(message)
                    .await
                    .map_err(|err| BlobError::SyncHintTransport {
                        target: target.clone(),
                        detail: format!("send SYSTEM_SYNC_HINT failed: {err}"),
                    })?;

                let response = wait_for_sync_hint_response(
                    receiver,
                    &trace_id,
                    &target,
                    Duration::from_millis(attempt_timeout_ms),
                )
                .await?;

                if response.status == "ok" {
                    target_results.push(SyncHintTargetResult {
                        target,
                        status: response.status,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
                    break;
                }

                if response.status == "sync_pending" {
                    if Instant::now() >= deadline {
                        return Err(BlobError::SyncHintTimeout { target, timeout_ms });
                    }
                    tokio::time::sleep(Duration::from_millis(BLOB_SYNC_HINT_RETRY_INTERVAL_MS))
                        .await;
                    continue;
                }

                let detail = response
                    .error_code
                    .map(|code| format!("error_code={code}"))
                    .or(response.message)
                    .unwrap_or_else(|| "sync hint returned error".to_string());
                return Err(BlobError::SyncHintFailed { target, detail });
            }
        }

        Ok(PublishBlobResult {
            blob_ref,
            timeout_ms,
            targets: target_results,
        })
    }

    pub fn promote(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        let result = (|| -> Result<(), BlobError> {
            Self::validate_blob_ref(blob_ref)?;
            let from = self.staging_path(blob_ref);
            let to = self.resolve_path(blob_ref);
            if to.exists() {
                return Ok(());
            }

            if let Some(parent) = to.parent() {
                ensure_dir_mode_0750(parent)?;
            }

            std::fs::rename(&from, &to).map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    BlobError::NotFound(blob_ref.blob_name.clone())
                } else {
                    map_io_error(err, "promote staging->active")
                }
            })?;
            set_file_mode_0640(&to)?;
            Ok(())
        })();
        if let Err(err) = result {
            metrics_record_error();
            return Err(err);
        }
        Ok(())
    }

    pub fn build_text_v1_payload(
        &self,
        content: &str,
        attachments: Vec<BlobRef>,
    ) -> Result<TextV1Payload, PayloadError> {
        self.build_text_v1_payload_with_limit(
            content,
            attachments,
            TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES,
            TEXT_V1_DEFAULT_OVERHEAD_BYTES,
        )
    }

    pub fn build_text_v1_payload_with_limit(
        &self,
        content: &str,
        attachments: Vec<BlobRef>,
        max_message_bytes: usize,
        message_overhead_bytes: usize,
    ) -> Result<TextV1Payload, PayloadError> {
        let inline_payload = TextV1Payload::new(content, attachments.clone());
        let inline_payload_bytes = serde_json::to_vec(&inline_payload)?.len();
        let estimated_total = inline_payload_bytes.saturating_add(message_overhead_bytes);
        if estimated_total <= max_message_bytes {
            inline_payload.validate()?;
            return Ok(inline_payload);
        }

        let content_ref = self
            .put_bytes(content.as_bytes(), "content.txt", "text/plain")
            .map_err(PayloadError::from)?;
        self.promote(&content_ref).map_err(PayloadError::from)?;
        let payload = TextV1Payload::with_content_ref(content_ref, attachments);
        payload.validate()?;
        Ok(payload)
    }

    pub fn resolve(&self, blob_ref: &BlobRef) -> PathBuf {
        metrics_record_resolve();
        self.resolve_path(blob_ref)
    }

    pub async fn resolve_with_retry(
        &self,
        blob_ref: &BlobRef,
        cfg: ResolveRetryConfig,
    ) -> Result<PathBuf, BlobError> {
        if let Err(err) = Self::validate_blob_ref(blob_ref) {
            metrics_record_error();
            return Err(err);
        }
        metrics_record_resolve();
        let path = self.resolve_path(blob_ref);
        if path.exists() {
            return Ok(path);
        }

        let started = Instant::now();
        let max_wait = Duration::from_millis(cfg.max_wait_ms);
        let mut delay_ms = cfg.initial_delay_ms.max(1);
        let backoff = if cfg.backoff_factor >= 1.0 {
            cfg.backoff_factor
        } else {
            1.0
        };
        let mut retry_attempts: u64 = 0;

        loop {
            let elapsed = started.elapsed();
            if elapsed >= max_wait {
                if retry_attempts > 0 {
                    BLOB_RESOLVE_RETRY_TOTAL.fetch_add(retry_attempts, Ordering::Relaxed);
                }
                metrics_record_error();
                return Err(BlobError::NotFound(blob_ref.blob_name.clone()));
            }

            let remaining = max_wait - elapsed;
            let sleep_for = Duration::from_millis(delay_ms).min(remaining);
            tokio::time::sleep(sleep_for).await;
            retry_attempts = retry_attempts.saturating_add(1);

            if path.exists() {
                if retry_attempts > 0 {
                    BLOB_RESOLVE_RETRY_TOTAL.fetch_add(retry_attempts, Ordering::Relaxed);
                }
                return Ok(path);
            }

            delay_ms = ((delay_ms as f64) * backoff).ceil() as u64;
            delay_ms = delay_ms.max(1);
        }
    }

    pub fn exists(&self, blob_ref: &BlobRef) -> bool {
        if Self::validate_blob_ref(blob_ref).is_err() {
            return false;
        }
        self.resolve_path(blob_ref).exists()
    }

    pub fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat, BlobError> {
        if let Err(err) = Self::validate_blob_ref(blob_ref) {
            metrics_record_error();
            return Err(err);
        }
        metrics_record_resolve();
        let path = self.resolve_path(blob_ref);
        match std::fs::metadata(&path) {
            Ok(meta) => Ok(BlobStat {
                size_on_disk: meta.len(),
                exists: true,
                path,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BlobStat {
                size_on_disk: 0,
                exists: false,
                path,
            }),
            Err(err) => {
                metrics_record_error();
                Err(BlobError::Io(err.to_string()))
            }
        }
    }

    pub fn run_gc(&self, options: BlobGcOptions) -> Result<BlobGcReport, BlobError> {
        let result = (|| -> Result<BlobGcReport, BlobError> {
            let now = std::time::SystemTime::now();
            let staging = self.cleanup_staging_orphans_with_now(
                options.staging_ttl_hours,
                options.apply,
                now,
            )?;
            let active = self.gc_active_by_spool_day_with_now(
                options.active_retain_days,
                options.apply,
                now,
            )?;
            Ok(BlobGcReport {
                apply: options.apply,
                staging_ttl_hours: options.staging_ttl_hours,
                active_retain_days: options.active_retain_days,
                staging,
                active,
            })
        })();
        if let Err(err) = result {
            metrics_record_error();
            return Err(err);
        }
        result
    }

    pub fn cleanup_staging_orphans(
        &self,
        ttl_hours: u64,
        apply: bool,
    ) -> Result<BlobGcPassReport, BlobError> {
        self.cleanup_staging_orphans_with_now(ttl_hours, apply, std::time::SystemTime::now())
    }

    pub fn gc_active_by_spool_day(
        &self,
        retain_days: u64,
        apply: bool,
    ) -> Result<BlobGcPassReport, BlobError> {
        self.gc_active_by_spool_day_with_now(retain_days, apply, std::time::SystemTime::now())
    }

    fn cleanup_staging_orphans_with_now(
        &self,
        ttl_hours: u64,
        apply: bool,
        now: std::time::SystemTime,
    ) -> Result<BlobGcPassReport, BlobError> {
        let mut report = BlobGcPassReport {
            apply,
            ..BlobGcPassReport::default()
        };
        let staging_root = self.cfg.blob_root.join("staging");
        let cutoff = now
            .checked_sub(Duration::from_secs(ttl_hours.saturating_mul(3600)))
            .unwrap_or(std::time::UNIX_EPOCH);
        for prefix_dir in list_prefix_dirs(&staging_root)? {
            for item in std::fs::read_dir(&prefix_dir)
                .map_err(|err| map_io_error(err, "read staging prefix directory"))?
            {
                let item = match item {
                    Ok(entry) => entry,
                    Err(err) => {
                        report
                            .errors
                            .push(format!("read staging directory entry failed: {err}"));
                        continue;
                    }
                };
                let path = item.path();
                let file_type = match item.file_type() {
                    Ok(value) => value,
                    Err(err) => {
                        report.errors.push(format!(
                            "read file type failed for '{}': {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                if !file_type.is_file() {
                    continue;
                }
                report.scanned_files = report.scanned_files.saturating_add(1);

                let filename = match path.file_name().and_then(|s| s.to_str()) {
                    Some(value) => value,
                    None => {
                        report.skipped_non_blob_files =
                            report.skipped_non_blob_files.saturating_add(1);
                        continue;
                    }
                };
                if validate_blob_name(filename).is_err() {
                    report.skipped_non_blob_files = report.skipped_non_blob_files.saturating_add(1);
                    continue;
                }

                let metadata = match std::fs::metadata(&path) {
                    Ok(value) => value,
                    Err(err) => {
                        report.errors.push(format!(
                            "read staging metadata failed for '{}': {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                let modified = match metadata.modified() {
                    Ok(value) => value,
                    Err(err) => {
                        report.errors.push(format!(
                            "read staging mtime failed for '{}': {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                if modified > cutoff {
                    continue;
                }
                report.candidate_files = report.candidate_files.saturating_add(1);
                if apply {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            report.deleted_files = report.deleted_files.saturating_add(1);
                            report.deleted_bytes =
                                report.deleted_bytes.saturating_add(metadata.len());
                        }
                        Err(err) => {
                            report.errors.push(format!(
                                "remove staging file failed for '{}': {err}",
                                path.display()
                            ));
                        }
                    }
                }
            }
            if apply {
                remove_dir_if_empty(&prefix_dir, &mut report.errors);
            }
        }
        Ok(report)
    }

    fn gc_active_by_spool_day_with_now(
        &self,
        retain_days: u64,
        apply: bool,
        now: std::time::SystemTime,
    ) -> Result<BlobGcPassReport, BlobError> {
        let mut report = BlobGcPassReport {
            apply,
            ..BlobGcPassReport::default()
        };
        let active_root = self.cfg.blob_root.join("active");
        // Conservative approximation of spool_day: file mtime day.
        let cutoff = now
            .checked_sub(Duration::from_secs(retain_days.saturating_mul(24 * 3600)))
            .unwrap_or(std::time::UNIX_EPOCH);
        for prefix_dir in list_prefix_dirs(&active_root)? {
            for item in std::fs::read_dir(&prefix_dir)
                .map_err(|err| map_io_error(err, "read active prefix directory"))?
            {
                let item = match item {
                    Ok(entry) => entry,
                    Err(err) => {
                        report
                            .errors
                            .push(format!("read active directory entry failed: {err}"));
                        continue;
                    }
                };
                let path = item.path();
                let file_type = match item.file_type() {
                    Ok(value) => value,
                    Err(err) => {
                        report.errors.push(format!(
                            "read file type failed for '{}': {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                if !file_type.is_file() {
                    continue;
                }
                report.scanned_files = report.scanned_files.saturating_add(1);

                let filename = match path.file_name().and_then(|s| s.to_str()) {
                    Some(value) => value,
                    None => {
                        report.skipped_non_blob_files =
                            report.skipped_non_blob_files.saturating_add(1);
                        continue;
                    }
                };
                if validate_blob_name(filename).is_err() {
                    report.skipped_non_blob_files = report.skipped_non_blob_files.saturating_add(1);
                    continue;
                }

                let metadata = match std::fs::metadata(&path) {
                    Ok(value) => value,
                    Err(err) => {
                        report.errors.push(format!(
                            "read active metadata failed for '{}': {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                let modified = match metadata.modified() {
                    Ok(value) => value,
                    Err(err) => {
                        report.errors.push(format!(
                            "read active mtime failed for '{}': {err}",
                            path.display()
                        ));
                        continue;
                    }
                };
                if modified > cutoff {
                    continue;
                }
                report.candidate_files = report.candidate_files.saturating_add(1);
                if apply {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            report.deleted_files = report.deleted_files.saturating_add(1);
                            report.deleted_bytes =
                                report.deleted_bytes.saturating_add(metadata.len());
                        }
                        Err(err) => {
                            report.errors.push(format!(
                                "remove active file failed for '{}': {err}",
                                path.display()
                            ));
                        }
                    }
                }
            }
            if apply {
                remove_dir_if_empty(&prefix_dir, &mut report.errors);
            }
        }
        Ok(report)
    }

    pub fn prefix(blob_name: &str) -> &str {
        parse_blob_name(blob_name)
            .map(|parts| &parts.hash[..BLOB_PREFIX_LEN])
            .unwrap_or("00")
    }

    fn staging_path(&self, blob_ref: &BlobRef) -> PathBuf {
        let prefix = Self::prefix(&blob_ref.blob_name);
        self.cfg
            .blob_root
            .join("staging")
            .join(prefix)
            .join(&blob_ref.blob_name)
    }

    fn resolve_path(&self, blob_ref: &BlobRef) -> PathBuf {
        let prefix = Self::prefix(&blob_ref.blob_name);
        self.cfg
            .blob_root
            .join("active")
            .join(prefix)
            .join(&blob_ref.blob_name)
    }

    fn build_blob_ref_and_staging_path(
        &self,
        data: &[u8],
        original_filename: &str,
        mime_override: Option<&str>,
    ) -> Result<(BlobRef, PathBuf), BlobError> {
        let hash_hex = sha256_hex(data);
        let hash16 = &hash_hex[..BLOB_HASH_LEN];

        let (name, ext, filename_original) =
            sanitize_filename(original_filename, self.cfg.name_max_chars);
        let blob_name = format!("{name}_{hash16}.{ext}");
        Self::validate_blob_name(&blob_name)?;

        let prefix = &hash16[..BLOB_PREFIX_LEN];
        let staging_dir = self.cfg.blob_root.join("staging").join(prefix);
        ensure_dir_mode_0750(&staging_dir)?;
        let staging_path = staging_dir.join(&blob_name);

        let mime = mime_override
            .map(str::to_string)
            .unwrap_or_else(|| guess_mime(&filename_original, &ext));
        let spool_day = Utc::now().format("%F").to_string();
        let blob_ref = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name,
            size: data.len() as u64,
            mime,
            filename_original,
            spool_day,
        };
        Ok((blob_ref, staging_path))
    }

    fn enforce_size_limit(&self, size: u64) -> Result<(), BlobError> {
        if let Some(max) = self.cfg.max_blob_bytes {
            if size > max {
                return Err(BlobError::TooLarge { size, max });
            }
        }
        Ok(())
    }
}

struct BlobNameParts<'a> {
    name: &'a str,
    hash: &'a str,
    ext: &'a str,
}

fn parse_blob_name(blob_name: &str) -> Option<BlobNameParts<'_>> {
    let underscore_idx = blob_name.rfind('_')?;
    let dot_idx = blob_name.rfind('.')?;
    if underscore_idx == 0 || dot_idx <= underscore_idx + 1 || dot_idx + 1 >= blob_name.len() {
        return None;
    }
    let name = &blob_name[..underscore_idx];
    let hash = &blob_name[underscore_idx + 1..dot_idx];
    let ext = &blob_name[dot_idx + 1..];
    Some(BlobNameParts { name, hash, ext })
}

fn validate_blob_name(blob_name: &str) -> Result<(), BlobError> {
    if blob_name.is_empty() {
        return Err(BlobError::InvalidName("blob_name is empty".to_string()));
    }
    if blob_name.contains('/') || blob_name.contains('\\') || blob_name.contains("..") {
        return Err(BlobError::InvalidName(
            "path traversal is not allowed".to_string(),
        ));
    }

    let parts = parse_blob_name(blob_name).ok_or_else(|| {
        BlobError::InvalidName("expected <name>_<hash16>.<ext> format".to_string())
    })?;

    if parts.name.is_empty() {
        return Err(BlobError::InvalidName("name segment is empty".to_string()));
    }
    if parts.name.len() > BLOB_NAME_MAX_CHARS {
        return Err(BlobError::InvalidName(format!(
            "name segment exceeds {BLOB_NAME_MAX_CHARS} chars"
        )));
    }
    if !parts
        .name
        .chars()
        .all(|ch| ch.is_ascii() && BLOB_NAME_ALLOWED.contains(ch))
    {
        return Err(BlobError::InvalidName(
            "name segment contains invalid characters".to_string(),
        ));
    }

    if parts.hash.len() != BLOB_HASH_LEN || !parts.hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(BlobError::InvalidName(format!(
            "hash segment must be {BLOB_HASH_LEN} hex chars"
        )));
    }

    if parts.ext.is_empty() || parts.ext.len() > BLOB_MAX_EXT_CHARS {
        return Err(BlobError::InvalidName(format!(
            "extension length must be 1..={BLOB_MAX_EXT_CHARS}"
        )));
    }
    if !parts.ext.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err(BlobError::InvalidName(
            "extension must be alphanumeric".to_string(),
        ));
    }
    if parts.ext.chars().any(|ch| ch.is_ascii_uppercase()) {
        return Err(BlobError::InvalidName(
            "extension must be lowercase".to_string(),
        ));
    }

    Ok(())
}

fn is_iso_day(value: &str) -> bool {
    if value.len() != 10 {
        return false;
    }
    let bytes = value.as_bytes();
    bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
}

fn map_io_error(err: std::io::Error, ctx: &str) -> BlobError {
    BlobError::Io(format!("{ctx}: {err}"))
}

fn metrics_record_put(size: u64) {
    BLOB_PUT_TOTAL.fetch_add(1, Ordering::Relaxed);
    BLOB_PUT_BYTES_TOTAL.fetch_add(size, Ordering::Relaxed);
}

fn metrics_record_resolve() {
    BLOB_RESOLVE_TOTAL.fetch_add(1, Ordering::Relaxed);
}

fn metrics_record_error() {
    BLOB_ERRORS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

fn ensure_dir_mode_0750(path: &Path) -> Result<(), BlobError> {
    std::fs::create_dir_all(path).map_err(|err| map_io_error(err, "create directory"))?;
    #[cfg(unix)]
    {
        let perms = Permissions::from_mode(0o750);
        std::fs::set_permissions(path, perms)
            .map_err(|err| map_io_error(err, "set directory permissions"))?;
    }
    Ok(())
}

fn set_file_mode_0640(path: &Path) -> Result<(), BlobError> {
    #[cfg(unix)]
    {
        let perms = Permissions::from_mode(0o640);
        std::fs::set_permissions(path, perms)
            .map_err(|err| map_io_error(err, "set file permissions"))?;
    }
    Ok(())
}

fn list_prefix_dirs(root: &Path) -> Result<Vec<PathBuf>, BlobError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut dirs = Vec::new();
    for item in std::fs::read_dir(root).map_err(|err| map_io_error(err, "read directory"))? {
        let item = item.map_err(|err| map_io_error(err, "read directory entry"))?;
        let path = item.path();
        let file_type = item
            .file_type()
            .map_err(|err| map_io_error(err, "read directory entry type"))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(value) => value,
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        dirs.push(path);
    }
    Ok(dirs)
}

fn remove_dir_if_empty(path: &Path, errors: &mut Vec<String>) {
    if !path.exists() {
        return;
    }
    match std::fs::read_dir(path) {
        Ok(mut entries) => {
            if entries.next().is_none() {
                if let Err(err) = std::fs::remove_dir(path) {
                    errors.push(format!(
                        "remove empty directory failed for '{}': {err}",
                        path.display()
                    ));
                }
            }
        }
        Err(err) => {
            errors.push(format!(
                "read directory for empty-check failed '{}': {err}",
                path.display()
            ));
        }
    }
}

#[derive(Debug, Clone)]
struct SyncHintResponse {
    status: String,
    error_code: Option<String>,
    message: Option<String>,
}

fn normalize_orchestrator_target(raw_target: &str) -> Result<String, BlobError> {
    let trimmed = raw_target.trim();
    if trimmed.is_empty() {
        return Err(BlobError::InvalidRef(
            "sync hint target cannot be empty".to_string(),
        ));
    }
    if trimmed.contains('@') {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("SY.orchestrator@{trimmed}"))
    }
}

fn parse_sync_hint_response_payload(
    payload: &serde_json::Value,
) -> Result<SyncHintResponse, String> {
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if status.is_empty() {
        return Err("missing payload.status in SYSTEM_SYNC_HINT_RESPONSE".to_string());
    }
    Ok(SyncHintResponse {
        status,
        error_code: payload
            .get("error_code")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        message: payload
            .get("message")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
    })
}

async fn wait_for_sync_hint_response(
    receiver: &mut NodeReceiver,
    trace_id: &str,
    target: &str,
    timeout: Duration,
) -> Result<SyncHintResponse, BlobError> {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(BlobError::SyncHintTimeout {
                target: target.to_string(),
                timeout_ms: timeout.as_millis() as u64,
            });
        }
        let remaining = deadline - now;
        let message = match receiver.recv_timeout(remaining).await {
            Ok(msg) => msg,
            Err(NodeError::Timeout) => {
                return Err(BlobError::SyncHintTimeout {
                    target: target.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            Err(err) => {
                return Err(BlobError::SyncHintTransport {
                    target: target.to_string(),
                    detail: format!("receive SYSTEM_SYNC_HINT_RESPONSE failed: {err}"),
                });
            }
        };

        if message.routing.trace_id != trace_id {
            continue;
        }

        if message.meta.msg_type == SYSTEM_KIND
            && message.meta.msg.as_deref() == Some(MSG_UNREACHABLE)
        {
            let reason = message
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            return Err(BlobError::SyncHintFailed {
                target: target.to_string(),
                detail: format!("router unreachable: reason={reason} original_dst={original_dst}"),
            });
        }

        if message.meta.msg_type == SYSTEM_KIND
            && message.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED)
        {
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let last_hop = message
                .payload
                .get("last_hop")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            return Err(BlobError::SyncHintFailed {
                target: target.to_string(),
                detail: format!(
                    "router ttl exceeded: original_dst={original_dst} last_hop={last_hop}"
                ),
            });
        }

        if message.meta.msg_type != SYSTEM_KIND
            || message.meta.msg.as_deref() != Some("SYSTEM_SYNC_HINT_RESPONSE")
        {
            continue;
        }

        return parse_sync_hint_response_payload(&message.payload).map_err(|detail| {
            BlobError::SyncHintFailed {
                target: target.to_string(),
                detail,
            }
        });
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in hash {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn sanitize_filename(original: &str, max_name_chars: usize) -> (String, String, String) {
    let filename_original = if original.trim().is_empty() {
        "blob.bin".to_string()
    } else {
        original.trim().to_string()
    };
    let filename_path = Path::new(&filename_original);
    let stem_raw = filename_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("blob");
    let ext_raw = filename_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");

    let mut stem = String::with_capacity(stem_raw.len());
    let mut prev_underscore = false;
    for ch in stem_raw.chars() {
        let candidate = if ch.is_ascii() && BLOB_NAME_ALLOWED.contains(ch) {
            ch
        } else {
            '_'
        };
        if candidate == '_' {
            if !prev_underscore {
                stem.push('_');
                prev_underscore = true;
            }
        } else {
            stem.push(candidate);
            prev_underscore = false;
        }
    }
    let trimmed = stem.trim_matches('_');
    let mut stem = if trimmed.is_empty() {
        "blob".to_string()
    } else {
        trimmed.to_string()
    };
    if stem.len() > max_name_chars {
        stem.truncate(max_name_chars);
    }

    let mut ext = ext_raw.to_ascii_lowercase();
    if ext.is_empty() {
        ext = "bin".to_string();
    }
    if ext.len() > BLOB_MAX_EXT_CHARS {
        ext.truncate(BLOB_MAX_EXT_CHARS);
    }
    if !ext.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        ext = "bin".to_string();
    }

    (stem, ext, filename_original)
}

fn guess_mime(filename_original: &str, ext: &str) -> String {
    mime_guess::from_path(filename_original)
        .first_raw()
        .map(str::to_string)
        .unwrap_or_else(|| match ext {
            "txt" => "text/plain".to_string(),
            _ => "application/octet-stream".to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[cfg(unix)]
    use std::{fs::Permissions, os::unix::fs::PermissionsExt};

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
        let root = std::env::temp_dir().join(format!("fluxbee-sdk-blob-{nanos}-{seq}"));
        let toolkit = BlobToolkit::new(BlobConfig {
            blob_root: root.clone(),
            name_max_chars: BLOB_NAME_MAX_CHARS,
            max_blob_bytes: None,
        })
        .expect("toolkit must be created");
        (toolkit, TestRoot { path: root })
    }

    #[test]
    fn validate_blob_name_rules() {
        assert!(BlobToolkit::validate_blob_name("factura_0123456789abcdef.png").is_ok());
        assert_eq!(BlobToolkit::prefix("factura_0123456789abcdef.png"), "01");

        assert!(BlobToolkit::validate_blob_name("factura_0123456789abcde.png").is_err());
        assert!(BlobToolkit::validate_blob_name("factura_0123456789abcdef.PNG").is_err());
        assert!(BlobToolkit::validate_blob_name("../x_0123456789abcdef.png").is_err());
    }

    #[test]
    fn normalize_orchestrator_target_rules() {
        assert_eq!(
            normalize_orchestrator_target("worker-220").expect("normalize hive id"),
            "SY.orchestrator@worker-220"
        );
        assert_eq!(
            normalize_orchestrator_target("SY.orchestrator@worker-220")
                .expect("normalize full target"),
            "SY.orchestrator@worker-220"
        );
        assert!(normalize_orchestrator_target("  ").is_err());
    }

    #[test]
    fn put_bytes_promote_and_resolve_roundtrip() {
        let (toolkit, root) = test_toolkit();
        let data = b"hello-blob";
        let blob_ref = toolkit
            .put_bytes(data, "Factura   Marzo@@2026.PNG", "")
            .expect("put_bytes");

        assert_eq!(blob_ref.ref_type, "blob_ref");
        assert!(blob_ref.blob_name.ends_with(".png"));
        assert!(blob_ref.blob_name.starts_with("Factura_Marzo_2026_"));
        assert!(!toolkit.exists(&blob_ref));

        let staging = root
            .path
            .join("staging")
            .join(BlobToolkit::prefix(&blob_ref.blob_name))
            .join(&blob_ref.blob_name);
        assert!(staging.exists());

        toolkit.promote(&blob_ref).expect("promote");
        assert!(toolkit.exists(&blob_ref));
        let resolved = toolkit.resolve(&blob_ref);
        let read_back = std::fs::read(resolved).expect("read promoted");
        assert_eq!(read_back, data);
    }

    #[test]
    fn put_from_file_works() {
        let (toolkit, root) = test_toolkit();
        let src = root.path.join("input.txt");
        std::fs::create_dir_all(&root.path).expect("create test root");
        std::fs::write(&src, b"file-content").expect("write source");

        let blob_ref = toolkit.put(&src, "").expect("put from file");
        toolkit.promote(&blob_ref).expect("promote");
        let stat = toolkit.stat(&blob_ref).expect("stat");
        assert!(stat.exists);
        assert_eq!(stat.size_on_disk, b"file-content".len() as u64);
    }

    #[tokio::test]
    async fn metrics_snapshot_tracks_blob_operations() {
        let before = BlobToolkit::metrics_snapshot();
        let (toolkit, _root) = test_toolkit();

        let blob_ref = toolkit
            .put_bytes(b"xyz", "metrics.txt", "text/plain")
            .expect("put bytes");
        toolkit.promote(&blob_ref).expect("promote");
        let _ = toolkit.resolve(&blob_ref);

        let missing = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "missing_metrics_0123456789abcdef.txt".to_string(),
            size: 1,
            mime: "text/plain".to_string(),
            filename_original: "missing_metrics.txt".to_string(),
            spool_day: "2026-03-09".to_string(),
        };
        let _ = toolkit
            .resolve_with_retry(
                &missing,
                ResolveRetryConfig {
                    max_wait_ms: 25,
                    initial_delay_ms: 5,
                    backoff_factor: 1.0,
                },
            )
            .await
            .expect_err("missing blob must fail");

        let after = BlobToolkit::metrics_snapshot();
        assert!(after.blob_put_total >= before.blob_put_total + 1);
        assert!(after.blob_put_bytes_total >= before.blob_put_bytes_total + 3);
        assert!(after.blob_resolve_total >= before.blob_resolve_total + 2);
        assert!(after.blob_resolve_retry_total >= before.blob_resolve_retry_total + 1);
        assert!(after.blob_errors_total >= before.blob_errors_total + 1);
    }

    #[test]
    fn producer_consumer_local_e2e_with_blob_ref() {
        let (producer, root) = test_toolkit();
        let consumer = BlobToolkit::new(BlobConfig {
            blob_root: root.path.clone(),
            name_max_chars: BLOB_NAME_MAX_CHARS,
            max_blob_bytes: None,
        })
        .expect("consumer toolkit");

        let blob_ref = producer
            .put_bytes(b"hello-e2e", "e2e.txt", "text/plain")
            .expect("producer put");
        producer.promote(&blob_ref).expect("producer promote");

        assert!(consumer.exists(&blob_ref));
        let payload = TextV1Payload::new("mensaje", vec![blob_ref.clone()]);
        let value = payload.to_value().expect("payload to value");
        let parsed = TextV1Payload::from_value(&value).expect("payload from value");
        let resolved = consumer.resolve(&parsed.attachments[0]);
        let read = std::fs::read(resolved).expect("consumer read");
        assert_eq!(read, b"hello-e2e");
    }

    #[tokio::test]
    async fn resolve_with_retry_finds_late_file() {
        let (toolkit, root) = test_toolkit();
        let blob_ref = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "late_0123456789abcdef.txt".to_string(),
            size: 4,
            mime: "text/plain".to_string(),
            filename_original: "late.txt".to_string(),
            spool_day: "2026-02-24".to_string(),
        };
        let target = toolkit.resolve(&blob_ref);
        let parent = target.parent().unwrap().to_path_buf();
        let to_write = target.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            std::fs::create_dir_all(parent).expect("create active dir");
            std::fs::write(to_write, b"late").expect("write late file");
        });

        let got = toolkit
            .resolve_with_retry(
                &blob_ref,
                ResolveRetryConfig {
                    max_wait_ms: 1_000,
                    initial_delay_ms: 10,
                    backoff_factor: 2.0,
                },
            )
            .await
            .expect("resolve with retry");

        assert!(got.exists());
        drop(root);
    }

    #[test]
    fn text_v1_helper_offloads_large_content_to_blob_ref() {
        let (toolkit, _root) = test_toolkit();
        let large_content = "x".repeat(8_192);
        let payload = toolkit
            .build_text_v1_payload_with_limit(&large_content, vec![], 1_024, 128)
            .expect("payload helper");
        assert!(payload.content.is_none());
        assert!(payload.content_ref.is_some());
    }

    #[test]
    fn promote_missing_blob_returns_not_found() {
        let (toolkit, _root) = test_toolkit();
        let missing = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "missing_0123456789abcdef.txt".to_string(),
            size: 1,
            mime: "text/plain".to_string(),
            filename_original: "missing.txt".to_string(),
            spool_day: "2026-02-24".to_string(),
        };
        let err = toolkit.promote(&missing).expect_err("promote should fail");
        assert!(matches!(err, BlobError::NotFound(_)));
    }

    #[test]
    fn put_missing_source_returns_io_error() {
        let (toolkit, root) = test_toolkit();
        let missing_src = root.path.join("no-existe.txt");
        let err = toolkit
            .put(&missing_src, "no-existe.txt")
            .expect_err("put should fail");
        assert!(matches!(err, BlobError::Io(_)));
    }

    #[test]
    fn contract_blob_too_large_from_put_bytes() {
        let (toolkit, _root) = test_toolkit();
        let limited = BlobToolkit::new(BlobConfig {
            blob_root: toolkit.cfg.blob_root.clone(),
            name_max_chars: BLOB_NAME_MAX_CHARS,
            max_blob_bytes: Some(3),
        })
        .expect("limited toolkit");
        let err = limited
            .put_bytes(b"1234", "too-large.txt", "text/plain")
            .expect_err("must fail by size policy");
        assert!(err.to_string().starts_with("BLOB_TOO_LARGE"));
        match err {
            BlobError::TooLarge { size, max } => {
                assert_eq!(size, 4);
                assert_eq!(max, 3);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn contract_blob_too_large_from_put_file() {
        let (toolkit, root) = test_toolkit();
        let src = root.path.join("large.bin");
        std::fs::create_dir_all(&root.path).expect("create root");
        std::fs::write(&src, b"123456").expect("write source");
        let limited = BlobToolkit::new(BlobConfig {
            blob_root: toolkit.cfg.blob_root.clone(),
            name_max_chars: BLOB_NAME_MAX_CHARS,
            max_blob_bytes: Some(5),
        })
        .expect("limited toolkit");
        let err = limited
            .put(&src, "large.bin")
            .expect_err("must fail by size");
        assert!(matches!(err, BlobError::TooLarge { size: 6, max: 5 }));
    }

    #[tokio::test]
    async fn resolve_with_retry_timeout_returns_not_found() {
        let (toolkit, _root) = test_toolkit();
        let missing = BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: "timeout_0123456789abcdef.txt".to_string(),
            size: 1,
            mime: "text/plain".to_string(),
            filename_original: "timeout.txt".to_string(),
            spool_day: "2026-02-24".to_string(),
        };
        let err = toolkit
            .resolve_with_retry(
                &missing,
                ResolveRetryConfig {
                    max_wait_ms: 25,
                    initial_delay_ms: 5,
                    backoff_factor: 2.0,
                },
            )
            .await
            .expect_err("resolve_with_retry should timeout");
        assert!(matches!(err, BlobError::NotFound(_)));
        assert!(err.to_string().starts_with("BLOB_NOT_FOUND"));
    }

    #[cfg(unix)]
    #[test]
    fn put_bytes_permission_error_returns_io() {
        let (toolkit, root) = test_toolkit();
        std::fs::create_dir_all(&root.path).expect("create root");
        std::fs::set_permissions(&root.path, Permissions::from_mode(0o555))
            .expect("set readonly perms");

        let err = toolkit
            .put_bytes(b"x", "perm.txt", "text/plain")
            .expect_err("put_bytes should fail with permission denied");
        assert!(matches!(err, BlobError::Io(_)));
        assert!(err.to_string().starts_with("BLOB_IO_ERROR"));

        std::fs::set_permissions(&root.path, Permissions::from_mode(0o755)).expect("restore perms");
    }

    #[cfg(unix)]
    #[test]
    fn secure_modes_staging_and_active_are_applied() {
        let (toolkit, root) = test_toolkit();
        let blob_ref = toolkit
            .put_bytes(b"secure", "secure.txt", "text/plain")
            .expect("put_bytes");

        let staging_dir = root
            .path
            .join("staging")
            .join(BlobToolkit::prefix(&blob_ref.blob_name));
        let staging_file = staging_dir.join(&blob_ref.blob_name);
        let staging_dir_mode = std::fs::metadata(&staging_dir)
            .expect("staging dir metadata")
            .permissions()
            .mode()
            & 0o777;
        let staging_file_mode = std::fs::metadata(&staging_file)
            .expect("staging file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(staging_dir_mode, 0o750);
        assert_eq!(staging_file_mode, 0o640);

        toolkit.promote(&blob_ref).expect("promote");
        let active_dir = root
            .path
            .join("active")
            .join(BlobToolkit::prefix(&blob_ref.blob_name));
        let active_file = active_dir.join(&blob_ref.blob_name);
        let active_dir_mode = std::fs::metadata(&active_dir)
            .expect("active dir metadata")
            .permissions()
            .mode()
            & 0o777;
        let active_file_mode = std::fs::metadata(&active_file)
            .expect("active file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(active_dir_mode, 0o750);
        assert_eq!(active_file_mode, 0o640);
    }

    #[test]
    fn staging_gc_reports_candidates_in_dry_run() {
        let (toolkit, root) = test_toolkit();
        let blob_ref = toolkit
            .put_bytes(b"orphan", "orphan.txt", "text/plain")
            .expect("put_bytes");
        let staging_file = root
            .path
            .join("staging")
            .join(BlobToolkit::prefix(&blob_ref.blob_name))
            .join(&blob_ref.blob_name);
        assert!(staging_file.exists());

        let now = std::time::SystemTime::now()
            .checked_add(Duration::from_secs(1))
            .expect("advance now");
        let report = toolkit
            .cleanup_staging_orphans_with_now(0, false, now)
            .expect("staging gc dry-run");
        assert_eq!(report.candidate_files, 1);
        assert_eq!(report.deleted_files, 0);
        assert!(staging_file.exists());
    }

    #[test]
    fn staging_gc_apply_deletes_orphans() {
        let (toolkit, root) = test_toolkit();
        let blob_ref = toolkit
            .put_bytes(b"orphan", "orphan2.txt", "text/plain")
            .expect("put_bytes");
        let prefix_dir = root
            .path
            .join("staging")
            .join(BlobToolkit::prefix(&blob_ref.blob_name));
        let staging_file = prefix_dir.join(&blob_ref.blob_name);
        assert!(staging_file.exists());

        let now = std::time::SystemTime::now()
            .checked_add(Duration::from_secs(1))
            .expect("advance now");
        let report = toolkit
            .cleanup_staging_orphans_with_now(0, true, now)
            .expect("staging gc apply");
        assert_eq!(report.candidate_files, 1);
        assert_eq!(report.deleted_files, 1);
        assert!(!staging_file.exists());
        assert!(!prefix_dir.exists());
    }

    #[test]
    fn active_gc_by_spool_day_apply_deletes_old_candidates() {
        let (toolkit, root) = test_toolkit();
        let blob_ref = toolkit
            .put_bytes(b"old-active", "active-gc.txt", "text/plain")
            .expect("put_bytes");
        toolkit.promote(&blob_ref).expect("promote");
        let active_file = root
            .path
            .join("active")
            .join(BlobToolkit::prefix(&blob_ref.blob_name))
            .join(&blob_ref.blob_name);
        assert!(active_file.exists());

        let now = std::time::SystemTime::now()
            .checked_add(Duration::from_secs(1))
            .expect("advance now");
        let dry_run = toolkit
            .gc_active_by_spool_day_with_now(0, false, now)
            .expect("active gc dry-run");
        assert_eq!(dry_run.candidate_files, 1);
        assert_eq!(dry_run.deleted_files, 0);
        assert!(active_file.exists());

        let apply = toolkit
            .gc_active_by_spool_day_with_now(0, true, now)
            .expect("active gc apply");
        assert_eq!(apply.candidate_files, 1);
        assert_eq!(apply.deleted_files, 1);
        assert!(!active_file.exists());
    }
}
