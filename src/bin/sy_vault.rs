use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::Utc;
use fluxbee_sdk::identity::list_ilks_from_hive_config;
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    connect, try_handle_default_node_status, NodeConfig, NodeReceiver, NodeSender, NodeUuidMode,
    VaultMetadata, MSG_VAULT_GET, MSG_VAULT_GET_METADATA, MSG_VAULT_GET_METADATA_RESPONSE,
    MSG_VAULT_GET_RESPONSE, MSG_VAULT_PUT, MSG_VAULT_PUT_RESPONSE,
};
use nix::libc::{flock, LOCK_EX, LOCK_NB};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use tokio::time;
use tracing_subscriber::EnvFilter;

type VaultResult<T> = Result<T, VaultError>;

const VAULT_NODE_BASE_NAME: &str = "SY.vault";
const VAULT_NODE_VERSION: &str = "0.1";
const DEFAULT_DB_PATH: &str = "/var/lib/fluxbee/vault.db";
const DEFAULT_MASTER_KEY_PATH: &str = "/etc/fluxbee/vault.master.key";
const DEFAULT_LOCK_PATH: &str = "/var/run/fluxbee/sy-vault.lock";
const MAX_SECRET_VALUE_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
enum VaultError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("node error: {0}")]
    Node(#[from] fluxbee_sdk::NodeError),
    #[error("identity SHM error: {0}")]
    Identity(#[from] fluxbee_sdk::IdentityShmError),
    #[error("invalid master key: {0}")]
    InvalidMasterKey(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("key not found")]
    KeyNotFound,
    #[error("storage error: {0}")]
    Storage(String),
    #[error("encryption error")]
    Encryption,
}

impl VaultError {
    fn code(&self) -> &'static str {
        match self {
            VaultError::InvalidMasterKey(_) => "MASTER_KEY_NOT_AVAILABLE",
            VaultError::InvalidRequest(_) => "INVALID_REQUEST",
            VaultError::Unauthorized => "UNAUTHORIZED",
            VaultError::KeyNotFound => "KEY_NOT_FOUND",
            VaultError::Sqlite(_) | VaultError::Storage(_) => "STORAGE_ERROR",
            VaultError::Encryption => "ENCRYPTION_ERROR",
            VaultError::Identity(_) => "IDENTITY_UNAVAILABLE",
            VaultError::Io(_) => "IO_ERROR",
            VaultError::Json(_) => "INVALID_VALUE",
            VaultError::Node(_) => "NODE_ERROR",
        }
    }
}

#[derive(Debug, Clone)]
struct Caller {
    l2_name: Option<String>,
    ilk_id: Option<String>,
    tenant_id: Option<String>,
    ilk_type: Option<String>,
}

impl Caller {
    fn is_admin(&self) -> bool {
        self.l2_name
            .as_deref()
            .map(|name| {
                let base = name.split('@').next().unwrap_or(name);
                matches!(base, "SY.admin" | "SY.architect")
            })
            .unwrap_or(false)
    }
}

#[derive(Debug)]
struct LockGuard {
    path: PathBuf,
    _file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[allow(dead_code)]
#[derive(Debug)]
struct SecretRecord {
    key: String,
    metadata: VaultMetadata,
    version: i64,
    current_nonce: Vec<u8>,
    current_ciphertext: Vec<u8>,
    previous_nonce: Option<Vec<u8>>,
    previous_ciphertext: Option<Vec<u8>>,
    previous_version: Option<i64>,
    access_count: i64,
    last_accessed_at: Option<String>,
}

struct VaultStore {
    conn: Connection,
    cipher: Aes256Gcm,
}

#[tokio::main]
async fn main() -> Result<(), VaultError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_vault supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let hive_id = fluxbee_sdk::load_hive_id(&config_dir)?;
    let node_base_name = VAULT_NODE_BASE_NAME.to_string();
    let node_name = ensure_l2_name(&node_base_name, &hive_id);

    let _lock = acquire_lock(Path::new(DEFAULT_LOCK_PATH))?;
    let key = load_or_create_master_key(Path::new(DEFAULT_MASTER_KEY_PATH))?;
    let mut store = VaultStore::open(Path::new(DEFAULT_DB_PATH), &key)?;

    let node_config = NodeConfig {
        name: node_base_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: VAULT_NODE_VERSION.to_string(),
    };
    let (sender, mut receiver) = connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!(node_name = %node_name, "sy.vault started");

    loop {
        let msg = match receiver.recv().await {
            Ok(msg) => msg,
            Err(err) => {
                tracing::warn!(error = %err, "sy.vault connection interrupted; reconnect handled internally");
                continue;
            }
        };
        if try_handle_default_node_status(&sender, &msg).await? {
            continue;
        }
        if msg.meta.msg_type != SYSTEM_KIND {
            continue;
        }
        let action = msg.meta.msg.as_deref().unwrap_or_default();
        if !matches!(
            action,
            MSG_VAULT_PUT | MSG_VAULT_GET | MSG_VAULT_GET_METADATA
        ) {
            continue;
        }
        if let Err(err) = handle_vault_message(&sender, &mut store, &config_dir, &msg).await {
            tracing::warn!(
                error = %err,
                action,
                trace_id = %msg.routing.trace_id,
                "vault message failed before response"
            );
            let _ = send_error_response(&sender, &msg, response_action_for(action), &err).await;
        }
    }
}

async fn handle_vault_message(
    sender: &NodeSender,
    store: &mut VaultStore,
    config_dir: &Path,
    msg: &Message,
) -> VaultResult<()> {
    let action = msg.meta.msg.as_deref().unwrap_or_default();
    let caller = resolve_caller(config_dir, msg)?;
    let response_action = response_action_for(action);
    let result = match action {
        MSG_VAULT_PUT => handle_put(store, msg, &caller),
        MSG_VAULT_GET => handle_get(store, msg, &caller, true),
        MSG_VAULT_GET_METADATA => handle_get(store, msg, &caller, false),
        _ => Err(VaultError::InvalidRequest(
            "unsupported vault action".to_string(),
        )),
    };

    let payload = match result {
        Ok(payload) => payload,
        Err(err) => {
            let key = request_key(&msg.payload);
            let _ = store.audit(action, key.as_deref(), &caller, "error", Some(err.code()));
            error_payload(&err)
        }
    };
    send_system_response(sender, msg, response_action, payload).await?;
    Ok(())
}

fn handle_put(store: &mut VaultStore, msg: &Message, caller: &Caller) -> VaultResult<Value> {
    if !caller.is_admin() {
        return Err(VaultError::Unauthorized);
    }
    let request: fluxbee_sdk::VaultPutRequest = serde_json::from_value(msg.payload.clone())?;
    validate_key(&request.key)?;
    validate_metadata(&request.metadata)?;
    let value_bytes = serde_json::to_vec(&request.value)?;
    if value_bytes.len() > MAX_SECRET_VALUE_BYTES {
        return Err(VaultError::InvalidRequest(
            "secret value exceeds 1 MiB".to_string(),
        ));
    }
    let (version, changed) = store.put(&request.key, request.value, request.metadata, caller)?;
    Ok(json!({
        "status": "ok",
        "key": request.key,
        "version": version,
        "changed": changed,
    }))
}

fn handle_get(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    include_value: bool,
) -> VaultResult<Value> {
    let key = request_key_required(&msg.payload)?;
    validate_key(&key)?;
    let record = store.get_record(&key)?.ok_or(VaultError::KeyNotFound)?;
    authorize_read(caller, &record.metadata, include_value)?;
    let mut payload = json!({
        "status": "ok",
        "key": record.key,
        "metadata": record.metadata,
        "version": record.version,
        "last_accessed_at": record.last_accessed_at,
        "access_count": record.access_count,
    });
    if include_value {
        let value = store.decrypt_value(&record.current_nonce, &record.current_ciphertext)?;
        payload["value"] = value;
        store.mark_accessed(&key)?;
    }
    store.audit(
        if include_value {
            MSG_VAULT_GET
        } else {
            MSG_VAULT_GET_METADATA
        },
        Some(&key),
        caller,
        "success",
        None,
    )?;
    Ok(payload)
}

impl VaultStore {
    fn open(db_path: &Path, key: &[u8; 32]) -> VaultResult<Self> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            );
            INSERT INTO schema_version (version)
                SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version);

            CREATE TABLE IF NOT EXISTS secrets (
                key TEXT PRIMARY KEY,
                metadata_json TEXT NOT NULL,
                version INTEGER NOT NULL,
                current_nonce BLOB NOT NULL,
                current_ciphertext BLOB NOT NULL,
                previous_nonce BLOB,
                previous_ciphertext BLOB,
                previous_version INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_accessed_at TEXT,
                access_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                operation TEXT NOT NULL,
                key TEXT,
                caller_l2_name TEXT,
                caller_ilk TEXT,
                caller_tenant_id TEXT,
                result TEXT NOT NULL,
                error_code TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_audit_key ON audit_log(key);
            CREATE INDEX IF NOT EXISTS idx_audit_caller ON audit_log(caller_ilk);
            CREATE INDEX IF NOT EXISTS idx_audit_operation ON audit_log(operation);
            "#,
        )?;
        Ok(Self {
            conn,
            cipher: Aes256Gcm::new_from_slice(key)
                .map_err(|_| VaultError::InvalidMasterKey("invalid AES key".to_string()))?,
        })
    }

    fn put(
        &mut self,
        key: &str,
        value: Value,
        mut metadata: VaultMetadata,
        caller: &Caller,
    ) -> VaultResult<(i64, bool)> {
        let now = Utc::now().to_rfc3339();
        metadata.created_by = caller.ilk_id.clone();
        metadata.updated_at = Some(now.clone());

        let existing = self.get_record(key)?;
        if let Some(existing) = existing {
            let current_value =
                self.decrypt_value(&existing.current_nonce, &existing.current_ciphertext)?;
            if current_value == value {
                self.audit(MSG_VAULT_PUT, Some(key), caller, "noop", None)?;
                return Ok((existing.version, false));
            }
            metadata.created_at = existing.metadata.created_at.clone().or(Some(now.clone()));
            let (nonce, ciphertext) = self.encrypt_value(&value)?;
            let metadata_json = serde_json::to_string(&metadata)?;
            let version = existing.version.saturating_add(1);
            let tx = self.conn.transaction()?;
            tx.execute(
                r#"
                UPDATE secrets
                   SET metadata_json = ?2,
                       version = ?3,
                       current_nonce = ?4,
                       current_ciphertext = ?5,
                       previous_nonce = ?6,
                       previous_ciphertext = ?7,
                       previous_version = ?8,
                       updated_at = ?9
                 WHERE key = ?1
                "#,
                params![
                    key,
                    metadata_json,
                    version,
                    nonce,
                    ciphertext,
                    existing.current_nonce,
                    existing.current_ciphertext,
                    existing.version,
                    now
                ],
            )?;
            audit_with_tx(&tx, MSG_VAULT_PUT, Some(key), caller, "success", None)?;
            tx.commit()?;
            return Ok((version, true));
        }

        metadata.created_at = Some(now.clone());
        let (nonce, ciphertext) = self.encrypt_value(&value)?;
        let metadata_json = serde_json::to_string(&metadata)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO secrets (
                key, metadata_json, version, current_nonce, current_ciphertext,
                created_at, updated_at, access_count
            ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5, 0)
            "#,
            params![key, metadata_json, nonce, ciphertext, now],
        )?;
        audit_with_tx(&tx, MSG_VAULT_PUT, Some(key), caller, "success", None)?;
        tx.commit()?;
        Ok((1, true))
    }

    fn get_record(&self, key: &str) -> VaultResult<Option<SecretRecord>> {
        self.conn
            .query_row(
                r#"
                SELECT key, metadata_json, version, current_nonce, current_ciphertext,
                       previous_nonce, previous_ciphertext, previous_version,
                       access_count, last_accessed_at
                  FROM secrets
                 WHERE key = ?1
                "#,
                params![key],
                |row| {
                    let metadata_json: String = row.get(1)?;
                    let metadata =
                        serde_json::from_str::<VaultMetadata>(&metadata_json).map_err(|err| {
                            rusqlite::Error::FromSqlConversionFailure(
                                1,
                                rusqlite::types::Type::Text,
                                Box::new(err),
                            )
                        })?;
                    Ok(SecretRecord {
                        key: row.get(0)?,
                        metadata,
                        version: row.get(2)?,
                        current_nonce: row.get(3)?,
                        current_ciphertext: row.get(4)?,
                        previous_nonce: row.get(5)?,
                        previous_ciphertext: row.get(6)?,
                        previous_version: row.get(7)?,
                        access_count: row.get(8)?,
                        last_accessed_at: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(VaultError::from)
    }

    fn mark_accessed(&self, key: &str) -> VaultResult<()> {
        self.conn.execute(
            "UPDATE secrets SET last_accessed_at = ?2, access_count = access_count + 1 WHERE key = ?1",
            params![key, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn audit(
        &self,
        operation: &str,
        key: Option<&str>,
        caller: &Caller,
        result: &str,
        error_code: Option<&str>,
    ) -> VaultResult<()> {
        audit_with_conn(&self.conn, operation, key, caller, result, error_code)
    }

    fn encrypt_value(&self, value: &Value) -> VaultResult<(Vec<u8>, Vec<u8>)> {
        let plaintext = serde_json::to_vec(value)?;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_slice())
            .map_err(|_| VaultError::Encryption)?;
        Ok((nonce.to_vec(), ciphertext))
    }

    fn decrypt_value(&self, nonce: &[u8], ciphertext: &[u8]) -> VaultResult<Value> {
        if nonce.len() != 12 {
            return Err(VaultError::Encryption);
        }
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| VaultError::Encryption)?;
        Ok(serde_json::from_slice(&plaintext)?)
    }
}

fn audit_with_conn(
    conn: &Connection,
    operation: &str,
    key: Option<&str>,
    caller: &Caller,
    result: &str,
    error_code: Option<&str>,
) -> VaultResult<()> {
    conn.execute(
        r#"
        INSERT INTO audit_log (
            timestamp, operation, key, caller_l2_name, caller_ilk,
            caller_tenant_id, result, error_code
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
        params![
            Utc::now().to_rfc3339(),
            operation,
            key,
            caller.l2_name.as_deref(),
            caller.ilk_id.as_deref(),
            caller.tenant_id.as_deref(),
            result,
            error_code,
        ],
    )?;
    Ok(())
}

fn audit_with_tx(
    tx: &rusqlite::Transaction<'_>,
    operation: &str,
    key: Option<&str>,
    caller: &Caller,
    result: &str,
    error_code: Option<&str>,
) -> VaultResult<()> {
    tx.execute(
        r#"
        INSERT INTO audit_log (
            timestamp, operation, key, caller_l2_name, caller_ilk,
            caller_tenant_id, result, error_code
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
        params![
            Utc::now().to_rfc3339(),
            operation,
            key,
            caller.l2_name.as_deref(),
            caller.ilk_id.as_deref(),
            caller.tenant_id.as_deref(),
            result,
            error_code,
        ],
    )?;
    Ok(())
}

fn resolve_caller(config_dir: &Path, msg: &Message) -> VaultResult<Caller> {
    let l2_name = msg.source_l2_name().map(ToString::to_string);
    let ilk_id = msg.meta.src_ilk.clone();
    let mut caller = Caller {
        l2_name,
        ilk_id: ilk_id.clone(),
        tenant_id: None,
        ilk_type: None,
    };
    if caller.is_admin() {
        return Ok(caller);
    }
    let Some(ilk_id) = ilk_id else {
        return Err(VaultError::Unauthorized);
    };
    let snapshot = list_ilks_from_hive_config(config_dir)?;
    let Some(ilk) = snapshot.ilks.into_iter().find(|ilk| ilk.ilk_id == ilk_id) else {
        return Err(VaultError::Unauthorized);
    };
    caller.tenant_id = Some(ilk.tenant_id);
    caller.ilk_type = Some(ilk.ilk_type);
    Ok(caller)
}

fn authorize_read(
    caller: &Caller,
    metadata: &VaultMetadata,
    metadata_only: bool,
) -> VaultResult<()> {
    if caller.is_admin() {
        return Ok(());
    }
    if metadata.tenant_id == "sys" {
        return Err(VaultError::Unauthorized);
    }
    if caller.ilk_id.as_deref() == Some(metadata.owner_ilk.as_str()) {
        return Ok(());
    }
    let same_tenant = caller.tenant_id.as_deref() == Some(metadata.tenant_id.as_str());
    if metadata_only && same_tenant {
        return Ok(());
    }
    if same_tenant && caller.ilk_type.as_deref() == Some("system") {
        return Ok(());
    }
    Err(VaultError::Unauthorized)
}

fn validate_key(key: &str) -> VaultResult<()> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 256 {
        return Err(VaultError::InvalidRequest("invalid key length".to_string()));
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(VaultError::InvalidRequest("invalid key prefix".to_string()));
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(*b, b':' | b'_' | b'-'))
    {
        return Err(VaultError::InvalidRequest("invalid key format".to_string()));
    }
    Ok(())
}

fn validate_metadata(metadata: &VaultMetadata) -> VaultResult<()> {
    let tenant = metadata.tenant_id.trim();
    if tenant != "sys" && !tenant.starts_with("tnt:") {
        return Err(VaultError::InvalidRequest(
            "metadata.tenant_id must be sys or tnt:<uuid>".to_string(),
        ));
    }
    if !metadata.owner_ilk.starts_with("ilk:") {
        return Err(VaultError::InvalidRequest(
            "metadata.owner_ilk must be ilk:<uuid>".to_string(),
        ));
    }
    Ok(())
}

fn request_key(payload: &Value) -> Option<String> {
    payload
        .get("key")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
}

fn request_key_required(payload: &Value) -> VaultResult<String> {
    request_key(payload)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| VaultError::InvalidRequest("missing key".to_string()))
}

fn error_payload(err: &VaultError) -> Value {
    json!({
        "status": "error",
        "error_code": err.code(),
        "message": err.to_string(),
    })
}

async fn send_error_response(
    sender: &NodeSender,
    request: &Message,
    msg_name: &str,
    err: &VaultError,
) -> VaultResult<()> {
    send_system_response(sender, request, msg_name, error_payload(err)).await
}

async fn send_system_response(
    sender: &NodeSender,
    request: &Message,
    msg_name: &str,
    payload: Value,
) -> VaultResult<()> {
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: 16,
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg_name.to_string()),
            ..Meta::default()
        },
        payload,
    };
    sender.send(reply).await?;
    Ok(())
}

fn response_action_for(action: &str) -> &'static str {
    match action {
        MSG_VAULT_PUT => MSG_VAULT_PUT_RESPONSE,
        MSG_VAULT_GET_METADATA => MSG_VAULT_GET_METADATA_RESPONSE,
        MSG_VAULT_GET => MSG_VAULT_GET_RESPONSE,
        _ => MSG_VAULT_GET_RESPONSE,
    }
}

fn load_or_create_master_key(path: &Path) -> VaultResult<[u8; 32]> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&key)?;
        return Ok(key);
    }
    let metadata = fs::metadata(path)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(VaultError::InvalidMasterKey(
            "master key permissions must be 0600 or stricter".to_string(),
        ));
    }
    let data = fs::read(path)?;
    if data.len() != 32 {
        return Err(VaultError::InvalidMasterKey(
            "master key must be exactly 32 bytes".to_string(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&data);
    Ok(key)
}

fn acquire_lock(path: &Path) -> VaultResult<LockGuard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(VaultError::from)?;
    // libc flock is process-scoped and released when the file descriptor closes.
    let lock_result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if lock_result != 0 {
        return Err(VaultError::Storage(format!(
            "lock unavailable: {}",
            std::io::Error::last_os_error()
        )));
    }
    writeln!(file, "{}", std::process::id())?;
    Ok(LockGuard {
        path: path.to_path_buf(),
        _file: file,
    })
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{name}@{hive_id}")
    }
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!(error = %err, "connect failed; retrying");
                time::sleep(delay).await;
            }
        }
    }
}
