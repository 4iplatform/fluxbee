#![forbid(unsafe_code)]

//! `IO.blob` is the motherbee-local curator for public artifacts.
//!
//! It is intentionally not an authority and has no public channel. `SY.admin` authenticates the
//! producer, resolves publication ownership, and then sends one of the bounded worker commands
//! handled here. The worker validates a `BlobRef`, copies its bytes to `public/<full-sha256>`, and
//! maintains an idempotent publication/refcount ledger.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::blob::{BlobConfig, BlobError, BlobRef, BlobToolkit, BLOB_DEFAULT_MAX_BYTES};
use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_BLOB_CURATE, MSG_BLOB_CURATE_RESPONSE,
    MSG_BLOB_RELEASE, MSG_BLOB_RELEASE_RESPONSE, MSG_BLOB_STATUS_GET, MSG_BLOB_STATUS_GET_RESPONSE,
    MSG_NODE_STATUS_GET, SYSTEM_KIND,
};
use fluxbee_sdk::{
    managed_node_name, try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher,
};
use nix::fcntl::{open, OFlag};
use nix::sys::stat::{fstat, Mode, SFlag};
use nix::unistd::{close, read};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn std::error::Error + Send + Sync>;

const RPC_CH_CONTROL: &str = "control";
const LEDGER_SCHEMA_VERSION: u32 = 1;
const DEFAULT_BLOB_ROOT: &str = "/var/lib/fluxbee/blob";
const DEFAULT_LEDGER_PATH: &str = "/var/lib/fluxbee/state/io-blob/publications.json";

#[derive(Debug, Clone)]
struct WorkerConfig {
    node: NodeConfig,
    admin_hive: Option<String>,
    blob_root: PathBuf,
    public_root: PathBuf,
    ledger_path: PathBuf,
    max_bytes: u64,
}

#[derive(Debug, Clone)]
struct BlobCurator {
    toolkit: BlobToolkit,
    blob_root: PathBuf,
    public_root: PathBuf,
    ledger_path: PathBuf,
    max_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct CurateRequest {
    publication_id: String,
    tenant_id: String,
    publisher_l2_name: String,
    blob_ref: BlobRef,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseRequest {
    publication_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct StatusRequest {
    #[serde(default)]
    publication_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
struct PublicationRecord {
    publication_id: String,
    tenant_id: String,
    publisher_l2_name: String,
    blob_ref: BlobRef,
    public_name: String,
    sha256: String,
    size: u64,
    created_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PublicationLedger {
    schema_version: u32,
    publications: BTreeMap<String, PublicationRecord>,
}

impl Default for PublicationLedger {
    fn default() -> Self {
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            publications: BTreeMap::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum CuratorError {
    #[error("unauthorized worker caller: {0}")]
    Unauthorized(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("publication conflict: {0}")]
    Conflict(String),
    #[error("blob not found: {0}")]
    NotFound(String),
    #[error("blob is too large: size={size} max={max}")]
    TooLarge { size: u64, max: u64 },
    #[error("blob integrity error: {0}")]
    Integrity(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("blob error: {0}")]
    Blob(#[from] BlobError),
    #[error("system error: {0}")]
    System(String),
}

impl CuratorError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::InvalidRequest(_) => "INVALID_REQUEST",
            Self::Conflict(_) => "PUBLICATION_CONFLICT",
            Self::NotFound(_) => "BLOB_NOT_FOUND",
            Self::TooLarge { .. } => "BLOB_TOO_LARGE",
            Self::Integrity(_) => "BLOB_INTEGRITY_ERROR",
            Self::Blob(BlobError::NotFound(_)) => "BLOB_NOT_FOUND",
            Self::Blob(BlobError::InvalidName(_) | BlobError::InvalidRef(_)) => "INVALID_REQUEST",
            Self::Blob(BlobError::TooLarge { .. }) => "BLOB_TOO_LARGE",
            Self::Io(_) | Self::Json(_) | Self::Blob(_) | Self::System(_) => "SERVICE_FAILED",
        }
    }
}

struct FdGuard(RawFd);

impl Drop for FdGuard {
    fn drop(&mut self) {
        let _ = close(self.0);
    }
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or(
            "JSR_LOG_LEVEL",
            "info,io_blob=debug,fluxbee_sdk=info",
        )))
        .init();

    let cfg = build_worker_config()?;
    let profile = OperationalRouteProfile::builder()
        .command_channel(RPC_CH_CONTROL)
        .post_pending_rule(
            RouteMatch::exact(SYSTEM_KIND, MSG_BLOB_CURATE),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        .post_pending_rule(
            RouteMatch::exact(SYSTEM_KIND, MSG_BLOB_RELEASE),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        .post_pending_rule(
            RouteMatch::exact(SYSTEM_KIND, MSG_BLOB_STATUS_GET),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        .post_pending_rule(
            RouteMatch::exact(SYSTEM_KIND, MSG_NODE_STATUS_GET),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        .build()?;

    let dispatcher =
        RouterDispatcher::connect_with_retry(cfg.node.clone(), Duration::from_secs(1), profile)
            .await?;
    let sender = dispatcher.sender_snapshot();
    let full_name = sender.full_name().to_string();
    let local_hive = full_name
        .rsplit_once('@')
        .map(|(_, hive)| hive.to_string())
        .unwrap_or_else(|| "motherbee".to_string());
    let admin_hive = cfg.admin_hive.as_deref().unwrap_or(&local_hive);
    let expected_admin = format!("SY.admin@{admin_hive}");
    let curator = BlobCurator::new(&cfg)?;
    let mut incoming = dispatcher.take_command_receiver(RPC_CH_CONTROL).await?;

    tracing::info!(
        node = %full_name,
        expected_admin = %expected_admin,
        public_root = %cfg.public_root.display(),
        ledger_path = %cfg.ledger_path.display(),
        "IO.blob curator ready"
    );

    while let Some(message) = incoming.recv().await {
        if try_handle_default_node_status(&sender, &message).await? {
            continue;
        }

        let response_msg = response_message_for(message.meta.msg.as_deref());
        let result = match authorize_admin_message(&message, &expected_admin) {
            Err(err) => Err(err),
            Ok(()) => {
                let curator = curator.clone();
                let command = message.meta.msg.clone().unwrap_or_default();
                let payload = message.payload.clone();
                tokio::task::spawn_blocking(move || curator.handle(&command, payload))
                    .await
                    .map_err(|err| CuratorError::System(format!("curator worker failed: {err}")))?
            }
        };

        let payload = match result {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(
                    trace_id = %message.routing.trace_id,
                    caller = ?message.routing.src_l2_name,
                    command = ?message.meta.msg,
                    error_code = err.code(),
                    error = %err,
                    "IO.blob command rejected"
                );
                json!({
                    "status": "error",
                    "error_code": err.code(),
                    "error_detail": err.to_string(),
                })
            }
        };
        sender
            .send(build_reply(&sender, &message, response_msg, payload))
            .await?;
    }

    Ok(())
}

impl BlobCurator {
    fn new(cfg: &WorkerConfig) -> Result<Self, CuratorError> {
        ensure_dir(&cfg.public_root, 0o750)?;
        if let Some(parent) = cfg.ledger_path.parent() {
            ensure_dir(parent, 0o750)?;
        }
        let toolkit = BlobToolkit::new(BlobConfig {
            blob_root: cfg.blob_root.clone(),
            max_blob_bytes: Some(cfg.max_bytes),
            ..BlobConfig::default()
        })?;
        let curator = Self {
            toolkit,
            blob_root: cfg.blob_root.clone(),
            public_root: cfg.public_root.clone(),
            ledger_path: cfg.ledger_path.clone(),
            max_bytes: cfg.max_bytes,
        };
        curator.load_ledger()?;
        Ok(curator)
    }

    fn handle(&self, command: &str, payload: Value) -> Result<Value, CuratorError> {
        match command {
            MSG_BLOB_CURATE => self.curate(decode_request(payload)?),
            MSG_BLOB_RELEASE => self.release(decode_request(payload)?),
            MSG_BLOB_STATUS_GET => self.status(decode_request(payload)?),
            other => Err(CuratorError::InvalidRequest(format!(
                "unsupported worker command '{other}'"
            ))),
        }
    }

    fn curate(&self, request: CurateRequest) -> Result<Value, CuratorError> {
        validate_publication_id(&request.publication_id)?;
        validate_tenant_id(&request.tenant_id)?;
        if request.publisher_l2_name.trim().is_empty() {
            return Err(CuratorError::InvalidRequest(
                "publisher_l2_name is required".to_string(),
            ));
        }
        BlobToolkit::validate_blob_ref(&request.blob_ref)?;
        if request.blob_ref.size > self.max_bytes {
            return Err(CuratorError::TooLarge {
                size: request.blob_ref.size,
                max: self.max_bytes,
            });
        }

        let mut ledger = self.load_ledger()?;
        if let Some(existing) = ledger.publications.get(&request.publication_id) {
            if existing.tenant_id != request.tenant_id
                || existing.publisher_l2_name != request.publisher_l2_name
                || existing.blob_ref != request.blob_ref
            {
                return Err(CuratorError::Conflict(format!(
                    "publication_id '{}' already exists with different facts",
                    request.publication_id
                )));
            }
            self.verify_or_repair_public_file(existing)?;
            let ref_count = ref_count(&ledger, &existing.public_name);
            return Ok(curate_response(existing, false, false, ref_count));
        }

        let (sha256, size, file_created) = self.materialize_public_file(&request.blob_ref)?;
        let record = PublicationRecord {
            publication_id: request.publication_id.clone(),
            tenant_id: request.tenant_id,
            publisher_l2_name: request.publisher_l2_name,
            blob_ref: request.blob_ref,
            public_name: sha256.clone(),
            sha256,
            size,
            created_at_ms: now_epoch_ms(),
        };
        ledger
            .publications
            .insert(request.publication_id, record.clone());
        self.persist_ledger(&ledger)?;
        let count = ref_count(&ledger, &record.public_name);
        Ok(curate_response(&record, true, file_created, count))
    }

    fn release(&self, request: ReleaseRequest) -> Result<Value, CuratorError> {
        validate_publication_id(&request.publication_id)?;
        let mut ledger = self.load_ledger()?;
        let Some(record) = ledger.publications.remove(&request.publication_id) else {
            return Ok(json!({
                "status": "ok",
                "publication_id": request.publication_id,
                "released": false,
                "file_deleted": false,
                "ref_count": 0,
            }));
        };
        let remaining = ref_count(&ledger, &record.public_name);
        self.persist_ledger(&ledger)?;

        let mut file_deleted = false;
        if remaining == 0 {
            let path = self.public_path(&record.public_name)?;
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                    return Err(CuratorError::Integrity(format!(
                        "refusing to remove non-regular public path '{}'",
                        path.display()
                    )))
                }
                Ok(_) => {
                    fs::remove_file(path)?;
                    file_deleted = true;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(json!({
            "status": "ok",
            "publication_id": request.publication_id,
            "released": true,
            "file_deleted": file_deleted,
            "ref_count": remaining,
        }))
    }

    fn status(&self, request: StatusRequest) -> Result<Value, CuratorError> {
        let ledger = self.load_ledger()?;
        if let Some(publication_id) = request.publication_id {
            validate_publication_id(&publication_id)?;
            return match ledger.publications.get(&publication_id) {
                Some(record) => Ok(json!({ "status": "ok", "publication": record })),
                None => Err(CuratorError::NotFound(format!(
                    "publication '{}' does not exist",
                    publication_id
                ))),
            };
        }
        let unique_blobs = ledger
            .publications
            .values()
            .map(|record| record.public_name.as_str())
            .collect::<HashSet<_>>()
            .len();
        Ok(json!({
            "status": "ok",
            "publication_count": ledger.publications.len(),
            "unique_blob_count": unique_blobs,
            "public_root": self.public_root,
        }))
    }

    fn materialize_public_file(
        &self,
        blob_ref: &BlobRef,
    ) -> Result<(String, u64, bool), CuratorError> {
        let source = self.toolkit.resolve(blob_ref);
        let active_root = self.blob_root.join("active");
        ensure_source_under_root(&source, &active_root)?;
        let meta = fs::symlink_metadata(&source).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                CuratorError::NotFound(blob_ref.blob_name.clone())
            } else {
                CuratorError::Io(err)
            }
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(CuratorError::Integrity(
                "source blob must be a regular non-symlink file".to_string(),
            ));
        }
        if meta.len() != blob_ref.size {
            return Err(CuratorError::Integrity(format!(
                "source size {} does not match BlobRef size {}",
                meta.len(),
                blob_ref.size
            )));
        }
        if meta.len() > self.max_bytes {
            return Err(CuratorError::TooLarge {
                size: meta.len(),
                max: self.max_bytes,
            });
        }

        let temp_path =
            self.public_root
                .join(format!(".curate-{}-{}", std::process::id(), Uuid::new_v4()));
        let copied = copy_hash_no_follow(&source, &temp_path, self.max_bytes);
        let (sha256, size) = match copied {
            Ok(result) => result,
            Err(err) => {
                let _ = fs::remove_file(&temp_path);
                return Err(err);
            }
        };
        if size != blob_ref.size {
            let _ = fs::remove_file(&temp_path);
            return Err(CuratorError::Integrity(format!(
                "copied size {size} does not match BlobRef size {}",
                blob_ref.size
            )));
        }
        let expected_hash16 = embedded_hash16(&blob_ref.blob_name).ok_or_else(|| {
            CuratorError::Integrity("BlobRef name has no embedded hash16".to_string())
        })?;
        if !sha256[..16].eq_ignore_ascii_case(expected_hash16) {
            let _ = fs::remove_file(&temp_path);
            return Err(CuratorError::Integrity(format!(
                "BlobRef hash16 '{}' does not match content SHA-256",
                expected_hash16
            )));
        }

        let final_path = self.public_path(&sha256)?;
        let file_created = match fs::hard_link(&temp_path, &final_path) {
            Ok(()) => true,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_file_hash(&final_path, &sha256, size, self.max_bytes)?;
                false
            }
            Err(err) => {
                let _ = fs::remove_file(&temp_path);
                return Err(err.into());
            }
        };
        fs::remove_file(&temp_path)?;
        fs::set_permissions(&final_path, fs::Permissions::from_mode(0o640))?;
        Ok((sha256, size, file_created))
    }

    fn verify_or_repair_public_file(&self, record: &PublicationRecord) -> Result<(), CuratorError> {
        let path = self.public_path(&record.public_name)?;
        match verify_file_hash(&path, &record.sha256, record.size, self.max_bytes) {
            Ok(()) => Ok(()),
            Err(CuratorError::NotFound(_)) => {
                let (sha256, size, _) = self.materialize_public_file(&record.blob_ref)?;
                if sha256 != record.sha256 || size != record.size {
                    return Err(CuratorError::Integrity(
                        "repaired public file differs from ledger".to_string(),
                    ));
                }
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn public_path(&self, public_name: &str) -> Result<PathBuf, CuratorError> {
        if !is_full_sha256(public_name) {
            return Err(CuratorError::Integrity(format!(
                "invalid public_name '{public_name}'"
            )));
        }
        Ok(self.public_root.join(public_name))
    }

    fn load_ledger(&self) -> Result<PublicationLedger, CuratorError> {
        let raw = match fs::read(&self.ledger_path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PublicationLedger::default())
            }
            Err(err) => return Err(err.into()),
        };
        let ledger: PublicationLedger = serde_json::from_slice(&raw)?;
        if ledger.schema_version != LEDGER_SCHEMA_VERSION {
            return Err(CuratorError::Integrity(format!(
                "unsupported ledger schema_version {}",
                ledger.schema_version
            )));
        }
        for (id, record) in &ledger.publications {
            if id != &record.publication_id || !is_full_sha256(&record.public_name) {
                return Err(CuratorError::Integrity(format!(
                    "invalid ledger record '{id}'"
                )));
            }
        }
        Ok(ledger)
    }

    fn persist_ledger(&self, ledger: &PublicationLedger) -> Result<(), CuratorError> {
        let parent = self
            .ledger_path
            .parent()
            .ok_or_else(|| CuratorError::InvalidRequest("ledger path has no parent".to_string()))?;
        ensure_dir(parent, 0o750)?;
        let temp_path = parent.join(format!(
            ".publications-{}-{}.tmp",
            std::process::id(),
            Uuid::new_v4()
        ));
        let result = (|| -> Result<(), CuratorError> {
            let data = serde_json::to_vec_pretty(ledger)?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o640)
                .open(&temp_path)?;
            file.write_all(&data)?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp_path, &self.ledger_path)?;
            fs::set_permissions(&self.ledger_path, fs::Permissions::from_mode(0o640))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }
}

fn copy_hash_no_follow(
    source: &Path,
    destination: &Path,
    max_bytes: u64,
) -> Result<(String, u64), CuratorError> {
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o640)
        .open(destination)?;
    let result = hash_no_follow(source, max_bytes, |chunk| {
        output.write_all(chunk)?;
        Ok(())
    })?;
    output.flush()?;
    output.sync_all()?;
    Ok(result)
}

fn hash_no_follow<F>(
    source: &Path,
    max_bytes: u64,
    mut consume: F,
) -> Result<(String, u64), CuratorError>
where
    F: FnMut(&[u8]) -> Result<(), CuratorError>,
{
    let fd = open(source, OFlag::O_RDONLY | OFlag::O_NOFOLLOW, Mode::empty())
        .map_err(|err| CuratorError::System(format!("open source: {err}")))?;
    let fd = FdGuard(fd);
    let stat = fstat(fd.0).map_err(|err| CuratorError::System(format!("fstat source: {err}")))?;
    let kind = SFlag::from_bits_truncate(stat.st_mode);
    if !kind.contains(SFlag::S_IFREG) {
        return Err(CuratorError::Integrity(
            "source blob is not a regular file".to_string(),
        ));
    }
    if stat.st_size < 0 || stat.st_size as u64 > max_bytes {
        return Err(CuratorError::TooLarge {
            size: stat.st_size.max(0) as u64,
            max: max_bytes,
        });
    }

    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = read(fd.0, &mut buffer)
            .map_err(|err| CuratorError::System(format!("read source: {err}")))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > max_bytes {
            return Err(CuratorError::TooLarge {
                size: total,
                max: max_bytes,
            });
        }
        hasher.update(&buffer[..count]);
        consume(&buffer[..count])?;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn verify_file_hash(
    path: &Path,
    expected_sha256: &str,
    expected_size: u64,
    max_bytes: u64,
) -> Result<(), CuratorError> {
    let meta = fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CuratorError::NotFound(path.display().to_string())
        } else {
            CuratorError::Io(err)
        }
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(CuratorError::Integrity(format!(
            "public path '{}' is not a regular file",
            path.display()
        )));
    }
    let (actual_sha256, actual_size) = hash_no_follow(path, max_bytes, |_| Ok(()))?;
    if actual_size != expected_size || actual_sha256 != expected_sha256 {
        return Err(CuratorError::Integrity(format!(
            "public file verification failed for '{}'",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_source_under_root(source: &Path, active_root: &Path) -> Result<(), CuratorError> {
    let canonical_root = active_root.canonicalize().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CuratorError::NotFound(active_root.display().to_string())
        } else {
            CuratorError::Io(err)
        }
    })?;
    let canonical_source = source.canonicalize().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            CuratorError::NotFound(source.display().to_string())
        } else {
            CuratorError::Io(err)
        }
    })?;
    if !canonical_source.starts_with(canonical_root) {
        return Err(CuratorError::Integrity(
            "source blob resolves outside active root".to_string(),
        ));
    }
    Ok(())
}

fn curate_response(
    record: &PublicationRecord,
    publication_created: bool,
    file_created: bool,
    ref_count: usize,
) -> Value {
    json!({
        "status": "ok",
        "publication_id": record.publication_id,
        "public_name": record.public_name,
        "sha256": record.sha256,
        "size": record.size,
        "created": publication_created,
        "file_created": file_created,
        "ref_count": ref_count,
    })
}

fn ref_count(ledger: &PublicationLedger, public_name: &str) -> usize {
    ledger
        .publications
        .values()
        .filter(|record| record.public_name == public_name)
        .count()
}

fn decode_request<T: DeserializeOwned>(payload: Value) -> Result<T, CuratorError> {
    serde_json::from_value(payload)
        .map_err(|err| CuratorError::InvalidRequest(format!("invalid command payload: {err}")))
}

fn authorize_admin_message(message: &Message, expected_admin: &str) -> Result<(), CuratorError> {
    if !message.meta.msg_type.eq_ignore_ascii_case(SYSTEM_KIND) {
        return Err(CuratorError::Unauthorized(
            "worker commands must use SYSTEM kind".to_string(),
        ));
    }
    if message.routing.src_l2_name.as_deref().map(str::trim) != Some(expected_admin) {
        return Err(CuratorError::Unauthorized(format!(
            "expected router-stamped caller {expected_admin}"
        )));
    }
    Ok(())
}

fn response_message_for(request: Option<&str>) -> &'static str {
    match request {
        Some(MSG_BLOB_CURATE) => MSG_BLOB_CURATE_RESPONSE,
        Some(MSG_BLOB_RELEASE) => MSG_BLOB_RELEASE_RESPONSE,
        Some(MSG_BLOB_STATUS_GET) => MSG_BLOB_STATUS_GET_RESPONSE,
        _ => MSG_BLOB_STATUS_GET_RESPONSE,
    }
}

fn build_reply(
    sender: &NodeSender,
    request: &Message,
    response_msg: &str,
    payload: Value,
) -> Message {
    Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: request.routing.ttl.max(1),
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(response_msg.to_string()),
            action: Some(response_msg.to_string()),
            ..Meta::default()
        },
        payload,
    }
}

fn validate_publication_id(value: &str) -> Result<(), CuratorError> {
    validate_prefixed_uuid(value, "pub", "publication_id")
}

fn validate_tenant_id(value: &str) -> Result<(), CuratorError> {
    validate_prefixed_uuid(value, "tnt", "tenant_id")
}

fn validate_prefixed_uuid(value: &str, prefix: &str, field: &str) -> Result<(), CuratorError> {
    let raw = value
        .trim()
        .strip_prefix(&format!("{prefix}:"))
        .ok_or_else(|| CuratorError::InvalidRequest(format!("{field} must be {prefix}:<uuid>")))?;
    Uuid::parse_str(raw)
        .map(|_| ())
        .map_err(|_| CuratorError::InvalidRequest(format!("{field} must be {prefix}:<uuid>")))
}

fn embedded_hash16(blob_name: &str) -> Option<&str> {
    let (stem, _) = blob_name.rsplit_once('.')?;
    let (_, hash) = stem.rsplit_once('_')?;
    (hash.len() == 16 && hash.chars().all(|ch| ch.is_ascii_hexdigit())).then_some(hash)
}

fn is_full_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

fn ensure_dir(path: &Path, mode: u32) -> Result<(), std::io::Error> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn build_worker_config() -> Result<WorkerConfig, CuratorError> {
    let blob_root = PathBuf::from(env_or("IO_BLOB_BLOB_ROOT", DEFAULT_BLOB_ROOT));
    let public_root = env("IO_BLOB_PUBLIC_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| blob_root.join("public"));
    let max_bytes = env("IO_BLOB_MAX_BYTES")
        .map(|raw| {
            raw.parse::<u64>().map_err(|_| {
                CuratorError::InvalidRequest("IO_BLOB_MAX_BYTES must be an integer".to_string())
            })
        })
        .transpose()?
        .unwrap_or(BLOB_DEFAULT_MAX_BYTES);
    if max_bytes == 0 {
        return Err(CuratorError::InvalidRequest(
            "IO_BLOB_MAX_BYTES must be greater than zero".to_string(),
        ));
    }
    Ok(WorkerConfig {
        node: NodeConfig {
            name: managed_node_name("IO.blob", &["IO_BLOB_NODE_NAME"]),
            router_socket: PathBuf::from(env_or(
                "IO_BLOB_ROUTER_SOCKET_DIR",
                "/var/run/fluxbee/routers",
            )),
            uuid_persistence_dir: PathBuf::from(env_or(
                "IO_BLOB_UUID_PERSISTENCE_DIR",
                "/var/lib/fluxbee/state/nodes",
            )),
            uuid_mode: NodeUuidMode::Persistent,
            config_dir: PathBuf::from(env_or("IO_BLOB_CONFIG_DIR", "/etc/fluxbee")),
            version: env_or("IO_BLOB_NODE_VERSION", "0.1.0"),
        },
        admin_hive: env("IO_BLOB_ADMIN_HIVE"),
        blob_root,
        public_root,
        ledger_path: PathBuf::from(env_or("IO_BLOB_LEDGER_PATH", DEFAULT_LEDGER_PATH)),
        max_bytes,
    })
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env(key).unwrap_or_else(|| default.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "io-blob-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&path).expect("create temp root");
        path
    }

    fn test_curator(root: &Path) -> BlobCurator {
        let cfg = WorkerConfig {
            node: NodeConfig {
                name: "IO.blob".to_string(),
                router_socket: root.join("routers"),
                uuid_persistence_dir: root.join("nodes"),
                uuid_mode: NodeUuidMode::Ephemeral,
                config_dir: root.to_path_buf(),
                version: "test".to_string(),
            },
            admin_hive: Some("motherbee".to_string()),
            blob_root: root.join("blob"),
            public_root: root.join("blob/public"),
            ledger_path: root.join("state/io-blob/publications.json"),
            max_bytes: 1024 * 1024,
        };
        ensure_dir(&cfg.blob_root.join("active"), 0o750).expect("active root");
        BlobCurator::new(&cfg).expect("curator")
    }

    fn create_blob(curator: &BlobCurator, data: &[u8], filename: &str, mime: &str) -> BlobRef {
        let blob_ref = curator
            .toolkit
            .put_bytes(data, filename, mime)
            .expect("put bytes");
        curator.toolkit.promote(&blob_ref).expect("promote");
        blob_ref
    }

    fn request(publication_id: &str, blob_ref: BlobRef) -> CurateRequest {
        CurateRequest {
            publication_id: publication_id.to_string(),
            tenant_id: "tnt:11111111-1111-4111-8111-111111111111".to_string(),
            publisher_l2_name: "AI.report@motherbee".to_string(),
            blob_ref,
        }
    }

    #[test]
    fn curate_is_idempotent_and_uses_full_sha256() {
        let root = temp_root("idempotent");
        let curator = test_curator(&root);
        let blob_ref = create_blob(&curator, b"interactive report", "report.html", "text/html");
        let req = request("pub:22222222-2222-4222-8222-222222222222", blob_ref);

        let first = curator.curate(req.clone()).expect("first curate");
        assert_eq!(first["status"], "ok");
        assert_eq!(first["created"], true);
        assert_eq!(first["file_created"], true);
        let sha = first["sha256"].as_str().expect("sha");
        assert!(is_full_sha256(sha));
        assert_eq!(
            fs::read(curator.public_root.join(sha)).unwrap(),
            b"interactive report"
        );

        let second = curator.curate(req).expect("idempotent curate");
        assert_eq!(second["created"], false);
        assert_eq!(second["ref_count"], 1);
        let ledger = curator.load_ledger().expect("ledger");
        assert_eq!(ledger.publications.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn release_keeps_deduplicated_file_until_last_publication() {
        let root = temp_root("refcount");
        let curator = test_curator(&root);
        let first_ref = create_blob(&curator, b"same bytes", "one.pdf", "application/pdf");
        let second_ref = create_blob(&curator, b"same bytes", "two.pdf", "application/pdf");
        let first = curator
            .curate(request(
                "pub:33333333-3333-4333-8333-333333333333",
                first_ref,
            ))
            .expect("first");
        let second = curator
            .curate(request(
                "pub:44444444-4444-4444-8444-444444444444",
                second_ref,
            ))
            .expect("second");
        assert_eq!(first["sha256"], second["sha256"]);
        assert_eq!(second["ref_count"], 2);
        let path = curator.public_root.join(first["sha256"].as_str().unwrap());

        let released = curator
            .release(ReleaseRequest {
                publication_id: "pub:33333333-3333-4333-8333-333333333333".to_string(),
            })
            .expect("release first");
        assert_eq!(released["ref_count"], 1);
        assert!(path.exists());

        let released = curator
            .release(ReleaseRequest {
                publication_id: "pub:44444444-4444-4444-8444-444444444444".to_string(),
            })
            .expect("release second");
        assert_eq!(released["ref_count"], 0);
        assert_eq!(released["file_deleted"], true);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn publication_id_cannot_be_reused_with_different_facts() {
        let root = temp_root("conflict");
        let curator = test_curator(&root);
        let one = create_blob(&curator, b"one", "one.txt", "text/plain");
        let two = create_blob(&curator, b"two", "two.txt", "text/plain");
        let id = "pub:55555555-5555-4555-8555-555555555555";
        curator.curate(request(id, one)).expect("first");
        let err = curator.curate(request(id, two)).expect_err("conflict");
        assert_eq!(err.code(), "PUBLICATION_CONFLICT");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn curate_rejects_content_that_no_longer_matches_blob_ref() {
        let root = temp_root("tampered");
        let curator = test_curator(&root);
        let blob_ref = create_blob(&curator, b"safe bytes", "report.txt", "text/plain");
        fs::write(curator.toolkit.resolve(&blob_ref), b"evil bytes").expect("tamper blob");

        let err = curator
            .curate(request(
                "pub:66666666-6666-4666-8666-666666666666",
                blob_ref,
            ))
            .expect_err("integrity error");
        assert_eq!(err.code(), "BLOB_INTEGRITY_ERROR");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_status_payload_is_an_invalid_request() {
        let root = temp_root("invalid-status");
        let curator = test_curator(&root);
        let err = curator
            .handle(MSG_BLOB_STATUS_GET, json!({ "publication_id": 42 }))
            .expect_err("invalid payload");
        assert_eq!(err.code(), "INVALID_REQUEST");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn worker_accepts_only_exact_same_hive_admin() {
        let mut message = Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some("SY.admin@motherbee".to_string()),
                dst: Destination::Unicast("IO.blob@motherbee".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_BLOB_CURATE.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        };
        assert!(authorize_admin_message(&message, "SY.admin@motherbee").is_ok());
        message.routing.src_l2_name = Some("SY.admin@worker1".to_string());
        assert!(authorize_admin_message(&message, "SY.admin@motherbee").is_err());
        message.routing.src_l2_name = Some("IO.cloud@motherbee".to_string());
        assert!(authorize_admin_message(&message, "SY.admin@motherbee").is_err());
    }
}
