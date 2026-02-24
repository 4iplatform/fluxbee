use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::payload::{PayloadError, TextV1Payload, TEXT_V1_DEFAULT_MESSAGE_MAX_BYTES};

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
    pub const BLOB_MAX_EXT_CHARS: usize = 10;
    pub const TEXT_V1_DEFAULT_OVERHEAD_BYTES: usize = 2_048;
}

pub use constants::{
    BLOB_HASH_LEN, BLOB_MAX_EXT_CHARS, BLOB_NAME_ALLOWED, BLOB_NAME_MAX_CHARS, BLOB_PREFIX_LEN,
    BLOB_RETRY_BACKOFF, BLOB_RETRY_INITIAL_MS, BLOB_RETRY_MAX_MS, BLOB_STAGING_TTL_HOURS,
    TEXT_V1_DEFAULT_OVERHEAD_BYTES,
};

#[derive(Debug, Clone)]
pub struct BlobConfig {
    pub blob_root: PathBuf,
    pub name_max_chars: usize,
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            blob_root: PathBuf::from("/var/lib/fluxbee/blob"),
            name_max_chars: BLOB_NAME_MAX_CHARS,
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
    #[error("BLOB_NOT_IMPLEMENTED")]
    NotImplemented,
}

#[derive(Debug, Clone)]
pub struct BlobToolkit {
    cfg: BlobConfig,
}

impl BlobToolkit {
    pub fn new(cfg: BlobConfig) -> Result<Self, BlobError> {
        if cfg.blob_root.as_os_str().is_empty() {
            return Err(BlobError::InvalidRef("blob_root is required".to_string()));
        }
        if cfg.name_max_chars == 0 {
            return Err(BlobError::InvalidRef(
                "name_max_chars must be > 0".to_string(),
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
        let data =
            std::fs::read(source_path).map_err(|err| map_io_error(err, "read source file"))?;
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
        Ok(blob_ref)
    }

    pub fn put_bytes(
        &self,
        data: &[u8],
        original_filename: &str,
        mime: &str,
    ) -> Result<BlobRef, BlobError> {
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
        Ok(blob_ref)
    }

    pub fn promote(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        Self::validate_blob_ref(blob_ref)?;
        let from = self.staging_path(blob_ref);
        let to = self.resolve(blob_ref);
        if to.exists() {
            return Ok(());
        }

        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| map_io_error(err, "create active directory"))?;
        }

        std::fs::rename(&from, &to).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                BlobError::NotFound(blob_ref.blob_name.clone())
            } else {
                map_io_error(err, "promote staging->active")
            }
        })?;
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
        let prefix = Self::prefix(&blob_ref.blob_name);
        self.cfg
            .blob_root
            .join("active")
            .join(prefix)
            .join(&blob_ref.blob_name)
    }

    pub async fn resolve_with_retry(
        &self,
        blob_ref: &BlobRef,
        cfg: ResolveRetryConfig,
    ) -> Result<PathBuf, BlobError> {
        Self::validate_blob_ref(blob_ref)?;
        let path = self.resolve(blob_ref);
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

        loop {
            let elapsed = started.elapsed();
            if elapsed >= max_wait {
                return Err(BlobError::NotFound(blob_ref.blob_name.clone()));
            }

            let remaining = max_wait - elapsed;
            let sleep_for = Duration::from_millis(delay_ms).min(remaining);
            tokio::time::sleep(sleep_for).await;

            if path.exists() {
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
        self.resolve(blob_ref).exists()
    }

    pub fn stat(&self, blob_ref: &BlobRef) -> Result<BlobStat, BlobError> {
        Self::validate_blob_ref(blob_ref)?;
        let path = self.resolve(blob_ref);
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
            Err(err) => Err(BlobError::Io(err.to_string())),
        }
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
        std::fs::create_dir_all(&staging_dir)
            .map_err(|err| map_io_error(err, "create staging directory"))?;
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

    #[test]
    fn producer_consumer_local_e2e_with_blob_ref() {
        let (producer, root) = test_toolkit();
        let consumer = BlobToolkit::new(BlobConfig {
            blob_root: root.path.clone(),
            name_max_chars: BLOB_NAME_MAX_CHARS,
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

        std::fs::set_permissions(&root.path, Permissions::from_mode(0o755)).expect("restore perms");
    }
}
