use std::collections::HashSet;
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
use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, VaultSecretChangedPayload, VaultSecretOp,
    is_system_kind, MSG_VAULT_SECRET_CHANGED, SCOPE_GLOBAL, SYSTEM_KIND, VAULT_BOOTSTRAP_ACTION,
};
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, VaultFilter, VaultListRequest, VaultMetadata,
    VaultRotateRequest, VaultScopedKeyRequest, VaultSecretSummary, MSG_VAULT_ASSIGN,
    MSG_VAULT_ASSIGN_RESPONSE, MSG_VAULT_DELETE, MSG_VAULT_DELETE_RESPONSE, MSG_VAULT_GET,
    MSG_VAULT_GET_METADATA, MSG_VAULT_GET_METADATA_RESPONSE, MSG_VAULT_GET_RESPONSE,
    MSG_VAULT_LIST, MSG_VAULT_LIST_RESPONSE, MSG_VAULT_PUT, MSG_VAULT_PUT_RESPONSE,
    MSG_VAULT_ROLLBACK, MSG_VAULT_ROLLBACK_RESPONSE, MSG_VAULT_ROTATE, MSG_VAULT_ROTATE_RESPONSE,
};
use nix::libc::{flock, LOCK_EX, LOCK_NB};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type VaultResult<T> = Result<T, VaultError>;

const VAULT_NODE_BASE_NAME: &str = "SY.vault";
const VAULT_NODE_VERSION: &str = "0.1";
const RPC_CH_SYSTEM: &str = "system";

fn build_vault_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}
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
    /// The key exists, but in another tenant than the request names: answered exactly like a key
    /// that does not exist (same code, same text), so naming another tenant's key reveals nothing.
    /// Only the audit row tells them apart ("denied").
    #[error("key not found")]
    KeyNotFoundInTenant,
    /// A put names a key that already belongs to another tenant. The namespace is global, so the
    /// name is taken; the put never takes it over.
    #[error("key is taken by another tenant")]
    KeyTaken,
    /// The secret's tenant is not active (docs/ilk-tenant-model-v1.md §2.2): the one thing the vault
    /// checks about a tenant. Answered as UNAUTHORIZED, with the reason, so every consumer stops
    /// using the secret as it does for a refusal.
    #[error("the secret's tenant {0}")]
    TenantNotActive(String),
    /// The secret's tenant is not in the identity SHM: deleted, or identity has not loaded its data
    /// yet (a degraded start writes only the root tenant). The value is not handed out, but the
    /// answer is retryable (IDENTITY_UNAVAILABLE), so a consumer keeps what it holds instead of
    /// dropping it during an identity outage. A deleted tenant has no node left to ask.
    #[error("the secret's tenant {0}")]
    TenantUnknown(String),
    /// The caller's `meta.src_ilk` is not the ILK registered for the node the router says sent the
    /// request (`routing.src_l2_name`, which the node cannot choose). Without this check any node
    /// could claim another's ILK and read its secrets, or claim SY.admin's and write any.
    #[error("caller {0}")]
    CallerMismatch(String),
    /// The caller's ILK is not in the identity SHM, or the SHM cannot be read: retryable, since a
    /// degraded identity start writes only the system's ILKs.
    #[error("caller {0}")]
    CallerUnknown(String),
    #[error("no previous version available")]
    NoPreviousVersion,
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
            VaultError::Unauthorized
            | VaultError::TenantNotActive(_)
            | VaultError::CallerMismatch(_) => "UNAUTHORIZED",
            VaultError::KeyNotFound | VaultError::KeyNotFoundInTenant => "KEY_NOT_FOUND",
            VaultError::KeyTaken => "KEY_TAKEN",
            VaultError::NoPreviousVersion => "NO_PREVIOUS_VERSION",
            VaultError::Sqlite(_) | VaultError::Storage(_) => "STORAGE_ERROR",
            VaultError::Encryption => "ENCRYPTION_ERROR",
            VaultError::Identity(_)
            | VaultError::TenantUnknown(_)
            | VaultError::CallerUnknown(_) => "IDENTITY_UNAVAILABLE",
            VaultError::Io(_) => "IO_ERROR",
            VaultError::Json(_) => "INVALID_VALUE",
            VaultError::Node(_) => "NODE_ERROR",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Caller {
    l2_name: Option<String>,
    ilk_id: Option<String>,
    tenant_id: Option<String>,
    ilk_type: Option<String>,
}

impl Caller {
    /// True when the caller's ILK matches one of the well-known administrative
    /// ILKs computed deterministically at boot (admin + architect).
    ///
    /// In Model D' this is the only fast-path: there's no string-based
    /// "trust whoever claims SY.admin in their L2 name" — we compare the
    /// deterministic ILK exactly. Identity is NOT in this set anymore; its
    /// boot-time chicken/egg is resolved by direct ILK-equality match
    /// (`caller.ilk == secret.ilk`) which doesn't need SHM resolution.
    fn is_well_known_admin(&self, well_known: &HashSet<String>) -> bool {
        self.ilk_id
            .as_deref()
            .map(|ilk| well_known.contains(ilk))
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

struct VaultRotateResult {
    rotated_at: String,
    current_version: i64,
    previous_version: i64,
}

struct VaultRollbackResult {
    current_version: i64,
    previous_version: i64,
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
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let hive_id = fluxbee_sdk::load_hive_id(&config_dir)?;
    let node_base_name = VAULT_NODE_BASE_NAME.to_string();
    let node_name = ensure_l2_name(&node_base_name, &hive_id);
    // Model D' / Phase J'-13: vault is self-contained and does NOT wait
    // for identity SHM. It computes every system ILK it needs locally with
    // the same deterministic formula identity uses to seed SHM. This
    // eliminates the legacy chicken-and-egg even though identity needs vault
    // to resolve its postgres secret. The lifecycle still starts vault last so
    // its bootstrap broadcasts reach consumers already registered in router.
    let self_ilk_id = fluxbee_sdk::deterministic_system_ilk_id(&node_name);
    tracing::info!(self_ilk_id = %self_ilk_id, "self system ILK computed deterministically (no SHM wait)");

    // Phase J' / Model D' — well-known administrative ILKs (admin /
    // architect): only these can write to vault.
    let well_known_admin_ilks: HashSet<String> = {
        let admin_ilk = fluxbee_sdk::deterministic_system_ilk_id(&format!("SY.admin@{}", hive_id));
        let architect_ilk =
            fluxbee_sdk::deterministic_system_ilk_id(&format!("SY.architect@{}", hive_id));
        tracing::info!(
            admin_ilk = %admin_ilk,
            architect_ilk = %architect_ilk,
            "well-known administrative ILKs computed"
        );
        let mut set = HashSet::with_capacity(2);
        set.insert(admin_ilk);
        set.insert(architect_ilk);
        set
    };

    // Well-known SY system ILKs — the SY.* entries from `system_nodes` in
    // hive.yaml plus the admin/architect override set. Used by
    // `authorize_read` to grant root-tenant pool reads to any SY system
    // caller WITHOUT requiring identity SHM to be populated. This is the
    // piece that closes the boot chicken/egg: vault can authorize reads
    // for identity (and any other SY) at boot even if identity hasn't
    // written its SHM yet.
    let well_known_system_ilks: HashSet<String> =
        compute_well_known_system_ilks(&config_dir, &hive_id, &self_ilk_id, &well_known_admin_ilks);
    tracing::info!(
        count = well_known_system_ilks.len(),
        "well-known SY system ILKs computed from hive.yaml (no SHM dependency)"
    );

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
    let profile = build_vault_rpc_profile()
        .map_err(|err| VaultError::Storage(format!("sy.vault rpc profile invalid: {err}")))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    tracing::info!(node_name = %node_name, "sy.vault started");

    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| VaultError::Storage(format!("sy.vault system receiver: {err}")))?;

    // Model D' / Phase J'-13c — bootstrap broadcast. Emit one
    // VAULT_SECRET_CHANGED with `op=put` for each secret already in
    // vault.db so consumers that arrived before us and got
    // VAULT_UNAVAILABLE can react now. Without this, vault.db with
    // pre-existing secrets + a boot race leaves consumers stuck in
    // `secret_source = Missing` because no future mutation will fire to
    // wake them up.
    //
    // The OUTCOME is idempotent for consumers already configured — they end up on the same
    // value — but the mechanism is not free: their handler exits(0) unconditionally once origin
    // and interest match, so systemd restarts them. Measured on a cold boot: SY.storage and
    // SY.identity each resolve via their own retry, then restart ~7 s later when this broadcast
    // lands. Harmless (Restart=always, back in ~5 s) and still REQUIRED for a real rotation,
    // where the pool must be rebuilt. Do not read this comment as "nothing happens".
    {
        let sender = dispatcher.sender_snapshot();
        if let Err(err) = emit_bootstrap_secret_broadcasts(&sender, &store, &hive_id).await {
            tracing::warn!(
                error = %err,
                "failed to emit bootstrap secret broadcasts; consumers may stay degraded until next mutation"
            );
        }
    }

    loop {
        let Some(msg) = system_rx.recv().await else {
            tracing::warn!("sy.vault system channel closed; exiting main loop");
            return Ok(());
        };
        let sender = dispatcher.sender_snapshot();
        if try_handle_default_node_status(&sender, &msg).await? {
            continue;
        }
        if !is_system_kind(&msg.meta.msg_type) {
            continue;
        }
        let action = msg.meta.msg.as_deref().unwrap_or_default();
        if !matches!(
            action,
            MSG_VAULT_PUT
                | MSG_VAULT_GET
                | MSG_VAULT_GET_METADATA
                | MSG_VAULT_LIST
                | MSG_VAULT_DELETE
                | MSG_VAULT_ROTATE
                | MSG_VAULT_ROLLBACK
                | MSG_VAULT_ASSIGN
        ) {
            continue;
        }
        if let Err(err) = handle_vault_message(
            &sender,
            &mut store,
            &config_dir,
            &hive_id,
            &well_known_admin_ilks,
            &well_known_system_ilks,
            &msg,
        )
        .await
        {
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

/// Output of a vault handler. Carries the response payload sent back to
/// the caller, plus (for mutating actions) an optional broadcast payload
/// emitted to all hive nodes via `VAULT_SECRET_CHANGED`.
struct HandlerOutcome {
    response: Value,
    broadcast: Option<VaultSecretChangedPayload>,
}

impl HandlerOutcome {
    fn response_only(response: Value) -> Self {
        Self {
            response,
            broadcast: None,
        }
    }
}

/// Compute the set of well-known SY system ILKs from `hive.yaml`. Includes
/// `self_ilk` (vault), `well_known_admin_ilks` (admin + architect), and
/// every SY.* entry in `system_nodes.<role>.nodes` mapped via
/// `deterministic_system_ilk_id`. Reads `hive.yaml` directly from disk;
/// does NOT depend on identity SHM being populated. This is what closes
/// the boot chicken-and-egg in Model D'.
fn compute_well_known_system_ilks(
    config_dir: &Path,
    hive_id: &str,
    self_ilk_id: &str,
    admin_set: &HashSet<String>,
) -> HashSet<String> {
    let mut set: HashSet<String> = HashSet::new();
    set.insert(self_ilk_id.to_string());
    set.extend(admin_set.iter().cloned());
    let hive_yaml_path = config_dir.join("hive.yaml");
    let Ok(yaml_str) = std::fs::read_to_string(&hive_yaml_path) else {
        tracing::warn!(
            path = %hive_yaml_path.display(),
            "could not read hive.yaml; SY system ILK set will be admin+architect+self only"
        );
        return set;
    };
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&yaml_str) else {
        tracing::warn!(
            path = %hive_yaml_path.display(),
            "could not parse hive.yaml; SY system ILK set will be admin+architect+self only"
        );
        return set;
    };
    let role = value
        .get("role")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "motherbee".to_string());
    if let Some(nodes) = value
        .get("system_nodes")
        .and_then(|sn| sn.get(role.as_str()))
        .and_then(|r| r.get("nodes"))
        .and_then(|n| n.as_sequence())
    {
        for entry in nodes {
            if let Some(name) = entry.as_str() {
                let name = name.trim();
                if !name.starts_with("SY.") {
                    tracing::debug!(
                        node = name,
                        "skipping non-SY lifecycle node in well-known system ILK set"
                    );
                    continue;
                }
                let l2 = if name.contains('@') {
                    name.to_string()
                } else {
                    format!("{}@{}", name, hive_id)
                };
                set.insert(fluxbee_sdk::deterministic_system_ilk_id(&l2));
            }
        }
    }
    set
}

#[allow(clippy::too_many_arguments)]
async fn handle_vault_message(
    sender: &NodeSender,
    store: &mut VaultStore,
    config_dir: &Path,
    hive_id: &str,
    well_known_admin_ilks: &HashSet<String>,
    well_known_system_ilks: &HashSet<String>,
    msg: &Message,
) -> VaultResult<()> {
    let action = msg.meta.msg.as_deref().unwrap_or_default();
    let response_action = response_action_for(action);
    let caller = match resolve_caller(config_dir, msg) {
        Ok(caller) => caller,
        Err(err) => {
            // Answered and audited like any refusal, never dropped: the caller must not wait.
            let claimed = Caller {
                l2_name: msg.source_l2_name().map(ToString::to_string),
                ilk_id: msg.meta.src_ilk.clone(),
                ..Caller::default()
            };
            tracing::warn!(
                action,
                caller_l2_name = ?claimed.l2_name,
                caller_ilk = ?claimed.ilk_id,
                error = %err,
                "vault request refused: the caller's ILK is not bound to the sending node"
            );
            let key = request_key(&msg.payload);
            let _ = store.audit(
                action,
                key.as_deref(),
                &claimed,
                audit_result_for_error(&err),
                Some(err.code()),
            );
            send_system_response(sender, msg, response_action, error_payload(&err)).await?;
            return Ok(());
        }
    };
    let result = match action {
        MSG_VAULT_PUT => handle_put(store, msg, &caller, well_known_admin_ilks, hive_id),
        MSG_VAULT_GET => handle_get(
            store,
            msg,
            &caller,
            well_known_system_ilks,
            true,
            &|tenant: &str| ensure_tenant_active(config_dir, tenant),
        )
        .map(HandlerOutcome::response_only),
        MSG_VAULT_GET_METADATA => handle_get(
            store,
            msg,
            &caller,
            well_known_system_ilks,
            false,
            &|tenant: &str| ensure_tenant_active(config_dir, tenant),
        )
        .map(HandlerOutcome::response_only),
        MSG_VAULT_LIST => handle_list(store, msg, &caller).map(HandlerOutcome::response_only),
        MSG_VAULT_DELETE => handle_delete(store, msg, &caller, well_known_admin_ilks, hive_id),
        MSG_VAULT_ROTATE => handle_rotate(store, msg, &caller, well_known_admin_ilks, hive_id),
        MSG_VAULT_ROLLBACK => handle_rollback(store, msg, &caller, well_known_admin_ilks, hive_id),
        MSG_VAULT_ASSIGN => handle_assign(store, msg, &caller, well_known_admin_ilks, hive_id),
        _ => Err(VaultError::InvalidRequest(
            "unsupported vault action".to_string(),
        )),
    };

    let outcome = match result {
        Ok(outcome) => outcome,
        Err(err) => {
            let key = request_key(&msg.payload);
            let _ = store.audit(
                action,
                key.as_deref(),
                &caller,
                audit_result_for_error(&err),
                Some(err.code()),
            );
            HandlerOutcome::response_only(error_payload(&err))
        }
    };
    send_system_response(sender, msg, response_action, outcome.response).await?;

    // Broadcast event-driven notification of the change to all hive nodes.
    // Mirrors the CONFIG_CHANGED pattern: Destination::Broadcast over the
    // router socket, scope=global, no auth required (metadata only — the
    // plaintext stays in vault and requires an authorized vault_get).
    if let Some(payload) = outcome.broadcast {
        if let Err(err) = send_broadcast_secret_changed(sender, msg, payload).await {
            tracing::warn!(
                error = %err,
                action,
                trace_id = %msg.routing.trace_id,
                "failed to emit VAULT_SECRET_CHANGED broadcast (consumers will retry on next poll/refresh)"
            );
        }
    }
    Ok(())
}

/// Send a `VAULT_SECRET_CHANGED` broadcast message over the router socket.
/// `src_trace_id` is the originating mutation's trace_id so observers can
/// correlate the broadcast with the put/rotate/delete/rollback request.
async fn send_broadcast_secret_changed(
    sender: &NodeSender,
    src_msg: &Message,
    payload: VaultSecretChangedPayload,
) -> VaultResult<()> {
    let broadcast = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Broadcast,
            ttl: 16,
            // New trace_id (this is a fan-out event, not a reply). The
            // payload carries no back-pointer to the mutation trace_id
            // because consumers don't need it — they react to the resource
            // change, not to the specific RPC that caused it. If
            // correlation is needed later, the originating trace_id is
            // already in vault's audit log under the same `at_ms`.
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_VAULT_SECRET_CHANGED.to_string()),
            src_ilk: src_msg.meta.dst_ilk.clone(),
            scope: Some(SCOPE_GLOBAL.to_string()),
            target: None,
            action: None,
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload: serde_json::to_value(&payload)?,
    };
    sender.send(broadcast).await?;
    tracing::info!(
        op = %payload.op.as_str(),
        resource_type = %payload.resource_type,
        tenant_id = %payload.tenant_id,
        ilk = %payload.ilk.as_deref().unwrap_or(""),
        version = payload.version,
        key = %payload.key,
        "vault secret changed broadcast sent"
    );
    Ok(())
}

/// Emit one `VAULT_SECRET_CHANGED` (op=put) for each secret currently
/// stored in vault.db. Called once at boot, after the router connection
/// is established and right before entering the receive loop.
///
/// Why: the `VAULT_SECRET_CHANGED` broadcast is the consumers' wake-up
/// signal. It normally fires on put/rotate/delete/rollback. If vault.db
/// already has secrets at boot (reinstall without cleanall, or simple
/// restart), no mutation will fire — and any consumer that arrived
/// before vault was routable saw `VAULT_UNAVAILABLE` and stayed
/// degraded. The bootstrap broadcast rescues them: each receives the
/// event, filters by interest, and reacts (exit/refresh).
///
/// Idempotent for consumers already configured: they re-resolve the
/// same value and continue. The cost is N small messages at boot, where
/// N = number of secrets in vault.db — bounded by operator intent.
async fn emit_bootstrap_secret_broadcasts(
    sender: &NodeSender,
    store: &VaultStore,
    hive_id: &str,
) -> VaultResult<()> {
    let summaries = store.list(VaultFilter::default(), &Caller::default())?;
    if summaries.is_empty() {
        tracing::info!("sy.vault bootstrap broadcast: vault.db is empty, nothing to announce");
        return Ok(());
    }
    let mut emitted = 0usize;
    let mut skipped = 0usize;
    for summary in summaries {
        let Some(payload) = build_secret_changed_payload(
            VaultSecretOp::Put,
            &summary.key,
            &summary.metadata,
            None,
            summary.version,
            hive_id,
        ) else {
            // Secret has no resource_type — Model D' rejects this on put,
            // but legacy records could still exist. Skip and log.
            skipped += 1;
            tracing::warn!(
                key = %summary.key,
                "bootstrap broadcast skipped: secret has no resource_type (legacy record?)"
            );
            continue;
        };
        // Build the broadcast Message directly. We don't have an
        // originating msg here (this is boot, not a mutation), so we
        // mint a fresh trace_id and synthesize the routing.
        let broadcast = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Broadcast,
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_VAULT_SECRET_CHANGED.to_string()),
                src_ilk: None,
                scope: Some(SCOPE_GLOBAL.to_string()),
                target: None,
                // Marks this as a boot re-announcement so destructive consumers can tell it
                // apart from a real change (SY.edge restarts on a TLS change — see PB-9).
                action: Some(VAULT_BOOTSTRAP_ACTION.to_string()),
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload: serde_json::to_value(&payload)?,
        };
        if let Err(err) = sender.send(broadcast).await {
            tracing::warn!(
                error = %err,
                key = %summary.key,
                "bootstrap broadcast failed for one secret; continuing with the rest"
            );
            continue;
        }
        emitted += 1;
    }
    tracing::info!(
        emitted = emitted,
        skipped = skipped,
        "sy.vault bootstrap broadcast complete (rescues consumers that raced ahead at boot)"
    );
    Ok(())
}

fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn handle_list(store: &mut VaultStore, msg: &Message, caller: &Caller) -> VaultResult<Value> {
    let request: VaultListRequest = serde_json::from_value(msg.payload.clone())?;
    let secrets = store.list(request.filter.unwrap_or_default(), caller)?;
    store.audit(MSG_VAULT_LIST, None, caller, "success", None)?;
    Ok(json!({
        "status": "ok",
        "count": secrets.len(),
        "secrets": secrets,
    }))
}

fn handle_delete(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    well_known: &HashSet<String>,
    hive_id: &str,
) -> VaultResult<HandlerOutcome> {
    let request: VaultScopedKeyRequest = serde_json::from_value(msg.payload.clone())?;
    validate_key(&request.key)?;
    let tenant = validate_request_tenant(&request.tenant_id)?;
    let pre_metadata = store.get_record(&request.key)?.map(|r| r.metadata);
    ensure_in_tenant(pre_metadata.as_ref(), tenant)?;
    // Auth: admin/architect can delete anything; non-admin can only delete
    // secrets dedicated to their own ILK.
    if !caller.is_well_known_admin(well_known) {
        let Some(existing_metadata) = pre_metadata.as_ref() else {
            return Err(VaultError::Unauthorized);
        };
        let owner_ilk = secret_owner_ilk(existing_metadata);
        match (owner_ilk, caller.ilk_id.as_deref()) {
            (Some(owner), Some(self_ilk)) if owner == self_ilk => {}
            _ => return Err(VaultError::Unauthorized),
        }
    }
    store.delete(&request.key, caller)?;
    let broadcast = pre_metadata.as_ref().and_then(|metadata| {
        build_secret_changed_payload(
            VaultSecretOp::Delete,
            &request.key,
            metadata,
            None,
            // After delete the version is gone; report 0 to signal absence.
            0,
            hive_id,
        )
    });
    Ok(HandlerOutcome {
        response: json!({
            "status": "ok",
            "key": request.key,
            "deleted": true,
        }),
        broadcast,
    })
}

fn handle_rotate(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    well_known: &HashSet<String>,
    hive_id: &str,
) -> VaultResult<HandlerOutcome> {
    let request: VaultRotateRequest = serde_json::from_value(msg.payload.clone())?;
    validate_key(&request.key)?;
    let tenant = validate_request_tenant(&request.tenant_id)?;
    let pre_metadata = store.get_record(&request.key)?.map(|r| r.metadata);
    ensure_in_tenant(pre_metadata.as_ref(), tenant)?;
    if !caller.is_well_known_admin(well_known) {
        let Some(existing_metadata) = pre_metadata.as_ref() else {
            return Err(VaultError::Unauthorized);
        };
        let owner_ilk = secret_owner_ilk(existing_metadata);
        match (owner_ilk, caller.ilk_id.as_deref()) {
            (Some(owner), Some(self_ilk)) if owner == self_ilk => {}
            _ => return Err(VaultError::Unauthorized),
        }
    }
    let value_bytes = serde_json::to_vec(&request.value)?;
    if value_bytes.len() > MAX_SECRET_VALUE_BYTES {
        return Err(VaultError::InvalidRequest(
            "secret value exceeds 1 MiB".to_string(),
        ));
    }
    let result = store.rotate(&request.key, request.value, caller)?;
    let broadcast = pre_metadata.as_ref().and_then(|metadata| {
        build_secret_changed_payload(
            VaultSecretOp::Rotate,
            &request.key,
            metadata,
            None,
            result.current_version,
            hive_id,
        )
    });
    Ok(HandlerOutcome {
        response: json!({
            "status": "ok",
            "key": request.key,
            "rotated_at": result.rotated_at,
            "current_version": result.current_version,
            "previous_version": result.previous_version,
        }),
        broadcast,
    })
}

fn handle_rollback(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    well_known: &HashSet<String>,
    hive_id: &str,
) -> VaultResult<HandlerOutcome> {
    let request: VaultScopedKeyRequest = serde_json::from_value(msg.payload.clone())?;
    validate_key(&request.key)?;
    let tenant = validate_request_tenant(&request.tenant_id)?;
    let pre_metadata = store.get_record(&request.key)?.map(|r| r.metadata);
    ensure_in_tenant(pre_metadata.as_ref(), tenant)?;
    if !caller.is_well_known_admin(well_known) {
        let Some(existing_metadata) = pre_metadata.as_ref() else {
            return Err(VaultError::Unauthorized);
        };
        let owner_ilk = secret_owner_ilk(existing_metadata);
        match (owner_ilk, caller.ilk_id.as_deref()) {
            (Some(owner), Some(self_ilk)) if owner == self_ilk => {}
            _ => return Err(VaultError::Unauthorized),
        }
    }
    let result = store.rollback(&request.key, caller)?;
    let broadcast = pre_metadata.as_ref().and_then(|metadata| {
        build_secret_changed_payload(
            VaultSecretOp::Rollback,
            &request.key,
            metadata,
            None,
            result.current_version,
            hive_id,
        )
    });
    Ok(HandlerOutcome {
        response: json!({
            "status": "ok",
            "key": request.key,
            "current_version": result.current_version,
            "previous_version": result.previous_version,
        }),
        broadcast,
    })
}

fn handle_put(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    well_known: &HashSet<String>,
    hive_id: &str,
) -> VaultResult<HandlerOutcome> {
    // Model D': only well-known admin ILKs (SY.admin, SY.architect) can
    // write to vault. Other nodes consume secrets via resolve_resource.
    if !caller.is_well_known_admin(well_known) {
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
    let previous = store
        .get_record(&request.key)?
        .map(|record| record.metadata);
    let mut metadata = request.metadata;
    // An update keeps who uses the secret unless the put names it (docs/ilk-tenant-model-v1.md
    // §2.2): a new value never turns a restricted secret into a general one. Otherwise only
    // VAULT_ASSIGN changes the nodes.
    if let Some(previous) = previous.as_ref() {
        let names_users =
            secret_owner_ilk(&metadata).is_some() || !metadata.assigned_ilks.is_empty();
        if !names_users && previous.tenant_id.trim() == metadata.tenant_id.trim() {
            metadata.ilk = previous.ilk.clone();
            metadata.owner_ilk = previous.owner_ilk.clone();
            metadata.assigned_ilks = previous.assigned_ilks.clone();
        }
    }
    let metadata_for_broadcast = metadata.clone();
    let (version, changed) = store.put(&request.key, request.value, metadata, caller)?;
    // Only emit a broadcast when something actually changed. Idempotent
    // re-puts (same value) return changed=false and version unchanged —
    // emitting an event for those would spam consumers with no-ops.
    let broadcast = if changed {
        build_secret_changed_payload(
            VaultSecretOp::Put,
            &request.key,
            &metadata_for_broadcast,
            previous.as_ref(),
            version,
            hive_id,
        )
    } else {
        None
    };
    Ok(HandlerOutcome {
        response: json!({
            "status": "ok",
            "key": request.key,
            "version": version,
            "changed": changed,
        }),
        broadcast,
    })
}

/// VAULT_ASSIGN: replace the nodes a secret is assigned to, in the tenant the request names; the
/// value, its version and the owner stay. Administrative, like a put. Everyone who had it or has
/// it now is told (docs/ilk-tenant-model-v1.md §2.2).
fn handle_assign(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    well_known: &HashSet<String>,
    hive_id: &str,
) -> VaultResult<HandlerOutcome> {
    if !caller.is_well_known_admin(well_known) {
        return Err(VaultError::Unauthorized);
    }
    let request: fluxbee_sdk::VaultAssignRequest = serde_json::from_value(msg.payload.clone())?;
    validate_key(&request.key)?;
    let tenant = validate_request_tenant(&request.tenant_id)?;
    let previous = store
        .get_record(&request.key)?
        .map(|record| record.metadata);
    ensure_in_tenant(previous.as_ref(), tenant)?;
    let Some(previous) = previous else {
        return Err(VaultError::KeyNotFound);
    };
    let mut assigned: Vec<String> = request
        .assigned_ilks
        .iter()
        .map(|ilk| ilk.trim().to_string())
        .filter(|ilk| !ilk.is_empty())
        .collect();
    assigned.sort_unstable();
    assigned.dedup();
    let mut metadata = previous.clone();
    metadata.assigned_ilks = assigned;
    validate_metadata(&metadata)?;
    let (version, changed) = store.set_assigned(&request.key, metadata.clone(), caller)?;
    let broadcast = if changed {
        build_secret_changed_payload(
            VaultSecretOp::Assign,
            &request.key,
            &metadata,
            Some(&previous),
            version,
            hive_id,
        )
    } else {
        None
    };
    Ok(HandlerOutcome {
        response: json!({
            "status": "ok",
            "key": request.key,
            "assigned_ilks": metadata.assigned_ilks,
            "general": is_general_secret(&metadata),
            "changed": changed,
        }),
        broadcast,
    })
}

/// Build the broadcast payload from the secret's metadata. Returns `None`
/// when metadata is missing `resource_type` (legacy/incomplete records),
/// since downstream consumers filter by `resource_type`. `previous` is the metadata before this
/// change, when it changed who reads the secret: the nodes it had and the ones it has now are all
/// told, and so is every node of the tenant when it was or is general.
fn build_secret_changed_payload(
    op: VaultSecretOp,
    key: &str,
    metadata: &VaultMetadata,
    previous: Option<&VaultMetadata>,
    version: i64,
    hive_id: &str,
) -> Option<VaultSecretChangedPayload> {
    let resource_type = metadata.resource_type.clone()?;
    // The previous owner, when the change took it away, is told like a node that lost it.
    let previous_owner = previous
        .and_then(secret_owner_ilk)
        .filter(|owner| Some(*owner) != secret_owner_ilk(metadata));
    let mut assigned_ilks: Vec<String> = metadata
        .assigned_ilks
        .iter()
        .chain(
            previous
                .into_iter()
                .flat_map(|previous| previous.assigned_ilks.iter()),
        )
        .map(String::as_str)
        .chain(previous_owner)
        .map(|ilk| ilk.trim().to_string())
        .filter(|ilk| !ilk.is_empty())
        .collect();
    assigned_ilks.sort_unstable();
    assigned_ilks.dedup();
    Some(VaultSecretChangedPayload {
        op,
        resource_type,
        tenant_id: metadata.tenant_id.clone(),
        ilk: metadata.ilk.clone().filter(|v| !v.is_empty()),
        assigned_ilks,
        general: is_general_secret(metadata) || previous.is_some_and(is_general_secret),
        version,
        key: key.to_string(),
        hive_id: hive_id.to_string(),
        at_ms: now_epoch_ms(),
    })
}

/// Extract the canonical owner ILK from a metadata record, preferring the
/// new `ilk` field (Model D') and falling back to the legacy `owner_ilk`
/// string for transitional reads of secrets written by the old schema.
fn secret_owner_ilk(metadata: &VaultMetadata) -> Option<&str> {
    if let Some(ilk) = metadata
        .ilk
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(ilk);
    }
    let legacy = metadata.owner_ilk.trim();
    if legacy.is_empty() {
        None
    } else {
        Some(legacy)
    }
}

/// `tenant_active` is the tenant check (`ensure_tenant_active`), passed in so the handler stays
/// testable without an identity SHM.
fn handle_get(
    store: &mut VaultStore,
    msg: &Message,
    caller: &Caller,
    well_known_system: &HashSet<String>,
    include_value: bool,
    tenant_active: &dyn Fn(&str) -> VaultResult<()>,
) -> VaultResult<Value> {
    let key = request_key_required(&msg.payload)?;
    validate_key(&key)?;
    let record = store.get_record(&key)?.ok_or(VaultError::KeyNotFound)?;
    authorize_read(caller, &record.metadata, well_known_system, include_value)?;
    // Who reads it is settled; the value goes out only while its tenant is active.
    if include_value {
        tenant_active(&record.metadata.tenant_id)?;
    }
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
        // VA-F9b: lock the at-rest DB to 0600 (SQLite creates it 0644 by default). The rows
        // are AES-256-GCM ciphertext so 0644 leaks no plaintext without the (already-0600)
        // master key, but defense-in-depth keeps the encrypted blobs owner-only too. The WAL
        // journal_mode above has already materialized the -wal/-shm sidecars (which carry the
        // same ciphertext), so tighten all three; best-effort — a chmod failure must not stop
        // the vault from serving.
        for suffix in ["", "-wal", "-shm"] {
            let path = if suffix.is_empty() {
                db_path.to_path_buf()
            } else {
                let mut name = db_path.as_os_str().to_os_string();
                name.push(suffix);
                std::path::PathBuf::from(name)
            };
            if path.exists() {
                if let Err(err) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                    tracing::warn!(path = %path.display(), error = %err, "sy-vault could not tighten DB file permissions to 0600");
                }
            }
        }
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
            // Checked before the no-op shortcut: a put of the same value under another tenant
            // must not answer ok as if the caller held the key.
            if existing.metadata.tenant_id.trim() != metadata.tenant_id.trim() {
                return Err(VaultError::KeyTaken);
            }
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

    fn list(&self, filter: VaultFilter, caller: &Caller) -> VaultResult<Vec<VaultSecretSummary>> {
        let limit = filter.limit.unwrap_or(200).clamp(1, 1000) as usize;
        // Order most-recently-WRITTEN first so the consumer-resolution path (VaultClient
        // ::resolve_resource -> list_then_get_first, which takes the FIRST match) selects
        // the freshest secret when several share the same (resource_type, tenant, ilk) — the
        // VA-J'-2e "most-recent-pool-wins" contract (resolve_resource's doc-comment promises
        // "most recent"). The prior `ORDER BY key` returned the alphabetically-first key.
        // We sort by `updated_at` (not `created_at`): `created_at` is frozen at INSERT and is
        // NOT bumped by a same-key re-put/rotate, so ordering by it would let a just-rewritten
        // key lose to an older-created sibling; `updated_at` is set on every write, giving true
        // last-write-wins. `created_at` then `key` are deterministic tiebreaks. Both columns are
        // NOT NULL and RFC3339, so lexical DESC == chronological DESC.
        let mut stmt = self.conn.prepare(
            r#"
            SELECT key, metadata_json, version, access_count, last_accessed_at
              FROM secrets
             ORDER BY updated_at DESC, created_at DESC, key ASC
            "#,
        )?;
        let mut rows = stmt.query([])?;
        let mut summaries = Vec::new();
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            if let Some(prefix) = filter.prefix.as_deref() {
                if !key.starts_with(prefix) {
                    continue;
                }
            }
            let metadata_json: String = row.get(1)?;
            let metadata: VaultMetadata = serde_json::from_str(&metadata_json)?;
            if let Some(tenant_id) = filter.tenant_id.as_deref() {
                if metadata.tenant_id != tenant_id {
                    continue;
                }
            }
            // Model D' filter: resource_type
            if let Some(resource_type) = filter.resource_type.as_deref() {
                let resource_matches = metadata
                    .resource_type
                    .as_deref()
                    .map(|v| v == resource_type)
                    .unwrap_or(false);
                if !resource_matches {
                    continue;
                }
            }
            // Model D' filter: ilk — `Some("")` means "general only" (no owner and no assigned
            // node), `Some("ilk:...")` means "dedicated to that ILK", `None` means "don't filter".
            if let Some(ilk_filter) = filter.ilk.as_deref() {
                if ilk_filter.is_empty() {
                    if !is_general_secret(&metadata) {
                        continue;
                    }
                } else if secret_owner_ilk(&metadata) != Some(ilk_filter) {
                    continue;
                }
            }
            if let Some(assigned) = filter.assigned_ilk.as_deref() {
                if !metadata
                    .assigned_ilks
                    .iter()
                    .any(|ilk| ilk.trim() == assigned.trim())
                {
                    continue;
                }
            }
            if !filter
                .tags
                .iter()
                .all(|tag| metadata.tags.iter().any(|candidate| candidate == tag))
            {
                continue;
            }
            // Model D': list is open. Anyone in the hive can see metadata of
            // any secret. The protection is on `vault_get` plaintext, not on
            // the list of keys.
            let _ = caller; // silence unused warning in this loop scope
            summaries.push(VaultSecretSummary {
                key,
                metadata,
                version: row.get(2)?,
                access_count: row.get(3)?,
                last_accessed_at: row.get(4)?,
            });
            if summaries.len() >= limit {
                break;
            }
        }
        Ok(summaries)
    }

    /// Rewrite a secret's metadata for VAULT_ASSIGN: the value and its version stay. Answers the
    /// version and whether the assignment changed.
    fn set_assigned(
        &mut self,
        key: &str,
        mut metadata: VaultMetadata,
        caller: &Caller,
    ) -> VaultResult<(i64, bool)> {
        let existing = self.get_record(key)?.ok_or(VaultError::KeyNotFound)?;
        if existing.metadata.assigned_ilks == metadata.assigned_ilks {
            self.audit(MSG_VAULT_ASSIGN, Some(key), caller, "noop", None)?;
            return Ok((existing.version, false));
        }
        let now = Utc::now().to_rfc3339();
        metadata.updated_at = Some(now.clone());
        let metadata_json = serde_json::to_string(&metadata)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE secrets SET metadata_json = ?2, updated_at = ?3 WHERE key = ?1",
            params![key, metadata_json, now],
        )?;
        audit_with_tx(&tx, MSG_VAULT_ASSIGN, Some(key), caller, "success", None)?;
        tx.commit()?;
        Ok((existing.version, true))
    }

    fn delete(&mut self, key: &str, caller: &Caller) -> VaultResult<()> {
        let tx = self.conn.transaction()?;
        let deleted = tx.execute("DELETE FROM secrets WHERE key = ?1", params![key])?;
        if deleted == 0 {
            return Err(VaultError::KeyNotFound);
        }
        audit_with_tx(&tx, MSG_VAULT_DELETE, Some(key), caller, "success", None)?;
        tx.commit()?;
        Ok(())
    }

    fn rotate(
        &mut self,
        key: &str,
        value: Value,
        caller: &Caller,
    ) -> VaultResult<VaultRotateResult> {
        let existing = self.get_record(key)?.ok_or(VaultError::KeyNotFound)?;
        let mut metadata = existing.metadata;
        let now = Utc::now().to_rfc3339();
        metadata.updated_at = Some(now.clone());
        let metadata_json = serde_json::to_string(&metadata)?;
        let (nonce, ciphertext) = self.encrypt_value(&value)?;
        let previous_version = existing.version;
        let current_version = existing.version.saturating_add(1);
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
                current_version,
                nonce,
                ciphertext,
                existing.current_nonce,
                existing.current_ciphertext,
                previous_version,
                now
            ],
        )?;
        audit_with_tx(&tx, MSG_VAULT_ROTATE, Some(key), caller, "success", None)?;
        tx.commit()?;
        Ok(VaultRotateResult {
            rotated_at: Utc::now().to_rfc3339(),
            current_version,
            previous_version,
        })
    }

    fn rollback(&mut self, key: &str, caller: &Caller) -> VaultResult<VaultRollbackResult> {
        let existing = self.get_record(key)?.ok_or(VaultError::KeyNotFound)?;
        let previous_nonce = existing
            .previous_nonce
            .ok_or(VaultError::NoPreviousVersion)?;
        let previous_ciphertext = existing
            .previous_ciphertext
            .ok_or(VaultError::NoPreviousVersion)?;
        let previous_version = existing
            .previous_version
            .ok_or(VaultError::NoPreviousVersion)?;
        let mut metadata = existing.metadata;
        metadata.updated_at = Some(Utc::now().to_rfc3339());
        let metadata_json = serde_json::to_string(&metadata)?;
        let old_current_version = existing.version;
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
                previous_version,
                previous_nonce,
                previous_ciphertext,
                existing.current_nonce,
                existing.current_ciphertext,
                old_current_version,
                Utc::now().to_rfc3339()
            ],
        )?;
        audit_with_tx(&tx, MSG_VAULT_ROLLBACK, Some(key), caller, "success", None)?;
        tx.commit()?;
        Ok(VaultRollbackResult {
            current_version: previous_version,
            previous_version: old_current_version,
        })
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

/// Resolve the caller's identity from the incoming message.
///
/// In Model D' vault never trusts `routing.src_l2_name` for authorization
/// — it's kept here only for audit logging. The real key is `meta.src_ilk`,
/// which the caller pre-computed (deterministic for SY, from env var for
/// dynamic AI/IO/WF) and which vault may also enrich with `tenant_id` /
/// `ilk_type` by reading identity SHM. If the caller's ILK is not yet in
/// SHM (identity at boot, fresh node), tenant/ilk_type stay `None` and
/// downstream auth falls back to direct ILK-equality match.
/// Who is asking: the router-stamped node name and the ILK the request claims, which must be the
/// one registered for that node (`check_caller_binding`), enriched with its tenant and type from the
/// identity SHM. A request with no ILK goes on unresolved: it reads nothing but what is open.
fn resolve_caller(config_dir: &Path, msg: &Message) -> VaultResult<Caller> {
    let l2_name = msg
        .source_l2_name()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let ilk_id = msg
        .meta
        .src_ilk
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    let mut caller = Caller {
        l2_name: l2_name.clone(),
        ilk_id: ilk_id.clone(),
        tenant_id: None,
        ilk_type: None,
    };
    let Some(ilk_id) = ilk_id else {
        return Ok(caller);
    };
    let snapshot = list_ilks_from_hive_config(config_dir).ok();
    let registered = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.ilks.iter().find(|ilk| ilk.ilk_id == ilk_id));
    check_caller_binding(l2_name.as_deref(), &ilk_id, registered)?;
    if let Some(ilk) = registered {
        caller.tenant_id = Some(ilk.tenant_id.clone());
        caller.ilk_type = Some(ilk.ilk_type.clone());
    }
    Ok(caller)
}

/// The claimed ILK must be the sending node's (io.web spec §21, phase 0): a system node's is derived
/// from its name, so it proves itself without the SHM (the core reads its keys at boot); any other
/// node's is the one identity registered for its name. `registered` is the SHM's entry for the
/// claimed ILK, if any.
fn check_caller_binding(
    l2_name: Option<&str>,
    ilk_id: &str,
    registered: Option<&fluxbee_sdk::identity::IdentityIlkOption>,
) -> VaultResult<()> {
    let Some(l2_name) = l2_name else {
        return Err(VaultError::CallerMismatch(format!(
            "claims {ilk_id} without a router-stamped node name"
        )));
    };
    if l2_name.starts_with("SY.") && fluxbee_sdk::deterministic_system_ilk_id(l2_name) == ilk_id {
        return Ok(());
    }
    match registered {
        Some(ilk)
            if ilk
                .handler_node
                .as_deref()
                .is_some_and(|node| node.trim().eq_ignore_ascii_case(l2_name)) =>
        {
            Ok(())
        }
        Some(_) => Err(VaultError::CallerMismatch(format!(
            "{l2_name} claims {ilk_id}, which identity registered for another node"
        ))),
        None => Err(VaultError::CallerUnknown(format!(
            "{l2_name} claims {ilk_id}, which the identity SHM does not hold (yet)"
        ))),
    }
}

/// Model D' read authorization (vault_get plaintext + vault_get_metadata
/// when `include_value=false`).
///
/// Rules:
/// - Dedicated secret (`secret.ilk` set): only that ILK reads. **No admin
///   bypass** — the operator's "read my own secret" path is `vault_get`
///   from a node that owns it, not admin reading on the operator's behalf.
/// - Pool secret (`secret.ilk` empty/null) in the hive's fixed root tenant:
///   readable by any caller whose ILK is in `well_known_system_ilks`. This
///   set is computed locally from `hive.yaml` at boot and does NOT depend
///   on identity SHM, so consumers can resolve their boot secrets even
///   when identity hasn't written SHM yet.
/// - Pool secret with `tenant_id == "tnt:<uuid>"`: readable by any caller
///   in the same tenant (resolved from SHM). If SHM isn't populated yet,
///   only the dedicated path applies.
fn authorize_read(
    caller: &Caller,
    metadata: &VaultMetadata,
    well_known_system_ilks: &HashSet<String>,
    _include_value: bool,
) -> VaultResult<()> {
    let secret_owner = secret_owner_ilk(metadata);
    let caller_ilk = caller.ilk_id.as_deref().filter(|v| !v.is_empty());

    // (1) Dedicated match (works without SHM resolution).
    if let (Some(owner), Some(self_ilk)) = (secret_owner, caller_ilk) {
        if owner == self_ilk {
            return Ok(());
        }
    }
    // (1b) Assigned to the caller (works without SHM resolution).
    if let Some(self_ilk) = caller_ilk {
        if metadata
            .assigned_ilks
            .iter()
            .any(|ilk| ilk.trim() == self_ilk)
        {
            return Ok(());
        }
    }
    // (2) General: no owner and no assigned node.
    if is_general_secret(metadata) {
        // (2a) Root-tenant pool universal for SY system callers (no SHM
        // needed). This is the path that closes the boot chicken/egg:
        // any node whose ILK matches a deterministic SY system ILK from
        // hive.yaml can read root-tenant pool secrets, even before identity has
        // written SHM.
        if metadata.tenant_id == fluxbee_sdk::DEFAULT_ROOT_TENANT_ID {
            if let Some(self_ilk) = caller_ilk {
                if well_known_system_ilks.contains(self_ilk) {
                    return Ok(());
                }
            }
            // Fallback: if SHM is populated, accept callers tagged as
            // system there (covers non-SY system callers we haven't
            // anticipated).
            if caller.ilk_type.as_deref() == Some("system") {
                return Ok(());
            }
        }
        // (2b) The caller's tenant's general secret: every node charged to the tenant reads it
        // (requires the caller's tenant from the SHM). Cloud makes a secret general by saving it
        // with no owner and no node.
        if let Some(tenant) = caller.tenant_id.as_deref() {
            if tenant == metadata.tenant_id {
                return Ok(());
            }
        }
    }
    Err(VaultError::Unauthorized)
}

/// A general secret: no owner and no assigned node. Every node of its tenant reads it
/// (docs/ilk-tenant-model-v1.md §2.2).
fn is_general_secret(metadata: &VaultMetadata) -> bool {
    secret_owner_ilk(metadata).is_none()
        && metadata
            .assigned_ilks
            .iter()
            .all(|ilk| ilk.trim().is_empty())
}

/// The one thing the vault checks about a tenant (docs/ilk-tenant-model-v1.md §2.2): a secret is
/// handed out only while its tenant exists and is active, read from the identity SHM (which leaves
/// deleted tenants out). The genesis tenant always counts as active: the core reads its keys at
/// boot, before identity has written the SHM.
fn ensure_tenant_active(config_dir: &Path, tenant_id: &str) -> VaultResult<()> {
    if fluxbee_sdk::is_root_tenant(tenant_id) {
        return Ok(());
    }
    tenant_check(tenant_id, || {
        fluxbee_sdk::identity::tenant_status_in_hive_config(config_dir, tenant_id)
    })
}

/// The tenant decision, apart from the SHM read: a malformed tenant id and a tenant that is not
/// active are refused (UNAUTHORIZED); one the SHM does not hold, or an unreadable SHM, is
/// retryable (IDENTITY_UNAVAILABLE).
fn tenant_check(
    tenant_id: &str,
    status: impl FnOnce() -> Result<Option<String>, fluxbee_sdk::IdentityShmError>,
) -> VaultResult<()> {
    let valid = tenant_id
        .trim()
        .strip_prefix("tnt:")
        .is_some_and(|raw| Uuid::parse_str(raw).is_ok());
    if !valid {
        return Err(VaultError::TenantNotActive(format!(
            "{tenant_id} is not a valid tenant id"
        )));
    }
    match status()? {
        Some(status) if status == "active" => Ok(()),
        Some(status) => Err(VaultError::TenantNotActive(format!(
            "{tenant_id} is not active ({status})"
        ))),
        None => Err(VaultError::TenantUnknown(format!(
            "{tenant_id} is not in the identity SHM (deleted, or identity has not loaded yet)"
        ))),
    }
}

fn audit_result_for_error(err: &VaultError) -> &'static str {
    match err {
        // Reaching for another tenant's key is a refusal, even when the caller is told "not found".
        VaultError::Unauthorized
        | VaultError::TenantNotActive(_)
        | VaultError::TenantUnknown(_)
        | VaultError::CallerMismatch(_)
        | VaultError::CallerUnknown(_)
        | VaultError::KeyNotFoundInTenant
        | VaultError::KeyTaken => "denied",
        _ => "error",
    }
}

fn validate_key(key: &str) -> VaultResult<()> {
    // The charset itself lives in the SDK (`vault_key_is_valid`) so the consumers that have to
    // NAME a key in their config can check it too — they ship in a different cargo workspace and
    // used to carry example keys this function rejects. The specific messages stay here: they are
    // what an operator sees, and "prefix" vs "format" tells them which rule they broke.
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 256 {
        return Err(VaultError::InvalidRequest("invalid key length".to_string()));
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(VaultError::InvalidRequest("invalid key prefix".to_string()));
    }
    if !fluxbee_sdk::vault_key_is_valid(key) {
        return Err(VaultError::InvalidRequest("invalid key format".to_string()));
    }
    Ok(())
}

fn validate_metadata(metadata: &VaultMetadata) -> VaultResult<()> {
    let tenant = metadata.tenant_id.trim();
    if !tenant.starts_with("tnt:") {
        return Err(VaultError::InvalidRequest(
            "metadata.tenant_id must be tnt:<uuid> (use the hive's root tenant for infrastructure secrets — admin defaults to it when tenant_id is omitted)".to_string(),
        ));
    }
    // Model D': resource_type is mandatory on every PUT. We accept either
    // a known canonical string (e.g. "openai") or a custom one — but it
    // must be already normalized by the caller (admin's HTTP path does it
    // before forwarding). Reject empty.
    let Some(resource_type) = metadata.resource_type.as_deref().map(str::trim) else {
        return Err(VaultError::InvalidRequest(
            "metadata.resource_type is required (e.g. 'openai', 'postgres')".to_string(),
        ));
    };
    if resource_type.is_empty() {
        return Err(VaultError::InvalidRequest(
            "metadata.resource_type must not be empty".to_string(),
        ));
    }
    // Optional: if ilk is set, it must be a well-formed ilk: id.
    if let Some(ilk) = metadata
        .ilk
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if !ilk.starts_with("ilk:") {
            return Err(VaultError::InvalidRequest(
                "metadata.ilk must be ilk:<uuid> (or omit for pool)".to_string(),
            ));
        }
    }
    // The assigned nodes are ILKs.
    if metadata
        .assigned_ilks
        .iter()
        .map(|ilk| ilk.trim())
        .any(|ilk| !ilk.starts_with("ilk:"))
    {
        return Err(VaultError::InvalidRequest(
            "metadata.assigned_ilks must hold ilk:<uuid> values".to_string(),
        ));
    }
    // owner_ilk (legacy field) is no longer required. If present, validate;
    // if empty, fine — Model D' uses metadata.ilk instead.
    if !metadata.owner_ilk.is_empty() && !metadata.owner_ilk.starts_with("ilk:") {
        return Err(VaultError::InvalidRequest(
            "metadata.owner_ilk if provided must be ilk:<uuid>; prefer metadata.ilk in Model D'"
                .to_string(),
        ));
    }
    Ok(())
}

/// The tenant a delete, rotate or rollback acts for. Every write names one (a put through
/// `metadata.tenant_id`); infrastructure keys live in the root tenant.
fn validate_request_tenant(tenant_id: &str) -> VaultResult<&str> {
    let tenant = tenant_id.trim();
    if !tenant.starts_with("tnt:") {
        return Err(VaultError::InvalidRequest(
            "tenant_id must be tnt:<uuid> (the tenant the key belongs to; the hive's root tenant for infrastructure keys)".to_string(),
        ));
    }
    Ok(tenant)
}

/// A key of another tenant is answered like a missing one. A missing key passes: each operation
/// reports it as before.
fn ensure_in_tenant(metadata: Option<&VaultMetadata>, tenant: &str) -> VaultResult<()> {
    match metadata {
        Some(metadata) if metadata.tenant_id.trim() != tenant => {
            Err(VaultError::KeyNotFoundInTenant)
        }
        _ => Ok(()),
    }
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
        MSG_VAULT_LIST => MSG_VAULT_LIST_RESPONSE,
        MSG_VAULT_DELETE => MSG_VAULT_DELETE_RESPONSE,
        MSG_VAULT_ROTATE => MSG_VAULT_ROTATE_RESPONSE,
        MSG_VAULT_ROLLBACK => MSG_VAULT_ROLLBACK_RESPONSE,
        MSG_VAULT_ASSIGN => MSG_VAULT_ASSIGN_RESPONSE,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(tenant_id: &str, ilk: Option<&str>) -> VaultMetadata {
        VaultMetadata {
            tenant_id: tenant_id.to_string(),
            owner_ilk: String::new(),
            resource_type: Some("postgres".to_string()),
            ilk: ilk.map(str::to_string),
            assigned_ilks: Vec::new(),
            description: None,
            created_by: None,
            created_at: None,
            updated_at: None,
            tags: Vec::new(),
        }
    }

    fn vault_request(action: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: String::new(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.vault@motherbee".to_string()),
                ttl: 16,
                trace_id: String::new(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(action.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    fn caller(ilk_id: Option<&str>, tenant_id: Option<&str>, ilk_type: Option<&str>) -> Caller {
        Caller {
            l2_name: Some("SY.test@motherbee".to_string()),
            ilk_id: ilk_id.map(str::to_string),
            tenant_id: tenant_id.map(str::to_string),
            ilk_type: ilk_type.map(str::to_string),
        }
    }

    fn assert_unauthorized(result: VaultResult<()>) {
        assert!(matches!(result, Err(VaultError::Unauthorized)));
    }

    #[test]
    fn authorize_read_allows_dedicated_owner_without_shm() {
        let caller = caller(Some("ilk:owner"), None, None);
        let metadata = metadata("tnt:client", Some("ilk:owner"));

        authorize_read(&caller, &metadata, &HashSet::new(), true).expect("owner reads own secret");
    }

    #[test]
    fn authorize_read_denies_dedicated_secret_to_different_ilk() {
        let caller = caller(Some("ilk:other"), Some("tnt:client"), Some("system"));
        let metadata = metadata("tnt:client", Some("ilk:owner"));

        assert_unauthorized(authorize_read(&caller, &metadata, &HashSet::new(), true));
    }

    #[test]
    fn authorize_read_allows_root_pool_for_well_known_system_ilk_without_shm() {
        let caller = caller(Some("ilk:sy-storage"), None, None);
        let metadata = metadata(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID, None);
        let well_known_system_ilks = HashSet::from(["ilk:sy-storage".to_string()]);

        authorize_read(&caller, &metadata, &well_known_system_ilks, true)
            .expect("well-known system caller reads root pool");
    }

    #[test]
    fn well_known_system_ilks_exclude_packaged_io_lifecycle_nodes() {
        let config_dir = std::env::temp_dir().join(format!(
            "fluxbee-vault-system-ilks-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&config_dir).expect("create config dir");
        fs::write(
            config_dir.join("hive.yaml"),
            "hive_id: motherbee\nrole: motherbee\nsystem_nodes:\n  motherbee:\n    nodes:\n      - SY.identity\n      - IO.blob\n",
        )
        .expect("write hive config");

        let self_ilk = fluxbee_sdk::deterministic_system_ilk_id("SY.vault@motherbee");
        let admin_ilk = fluxbee_sdk::deterministic_system_ilk_id("SY.admin@motherbee");
        let admin_set = HashSet::from([admin_ilk.clone()]);
        let system_ilks =
            compute_well_known_system_ilks(&config_dir, "motherbee", &self_ilk, &admin_set);

        assert!(system_ilks.contains(&self_ilk));
        assert!(system_ilks.contains(&admin_ilk));
        assert!(
            system_ilks.contains(&fluxbee_sdk::deterministic_system_ilk_id(
                "SY.identity@motherbee"
            ))
        );
        assert!(
            !system_ilks.contains(&fluxbee_sdk::deterministic_system_ilk_id(
                "IO.blob@motherbee"
            ))
        );

        fs::remove_dir_all(config_dir).expect("remove config dir");
    }

    #[test]
    fn authorize_read_denies_root_pool_to_unknown_unresolved_caller() {
        let caller = caller(Some("ilk:unknown"), None, None);
        let metadata = metadata(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID, None);

        assert_unauthorized(authorize_read(&caller, &metadata, &HashSet::new(), true));
    }

    #[test]
    fn authorize_read_allows_same_tenant_pool_from_shm() {
        let caller = caller(Some("ilk:client-node"), Some("tnt:client"), Some("ai"));
        let metadata = metadata("tnt:client", None);

        authorize_read(&caller, &metadata, &HashSet::new(), true)
            .expect("same tenant reads tenant pool");
    }

    #[test]
    fn authorize_read_denies_cross_tenant_pool() {
        let caller = caller(Some("ilk:client-node"), Some("tnt:client-a"), Some("ai"));
        let metadata = metadata("tnt:client-b", None);

        assert_unauthorized(authorize_read(&caller, &metadata, &HashSet::new(), true));
    }

    #[test]
    fn well_known_admin_authorization_uses_ilk_not_l2_name() {
        let admin_ilks = HashSet::from(["ilk:admin".to_string()]);

        assert!(caller(Some("ilk:admin"), None, None).is_well_known_admin(&admin_ilks));
        assert!(!caller(Some("ilk:other"), None, None).is_well_known_admin(&admin_ilks));
        assert!(!caller(None, None, Some("system")).is_well_known_admin(&admin_ilks));
    }

    #[test]
    fn validate_metadata_rejects_sys_tenant_alias() {
        let metadata = metadata("sys", None);

        let err = validate_metadata(&metadata).expect_err("sys is not a tenant id");
        assert!(matches!(err, VaultError::InvalidRequest(_)));
    }

    // ---- VA-H storage/crypto suite (VA-H1..H7 + VA-J'-10c) ------------------
    // Deterministic unit coverage for the on-disk VaultStore: key validation,
    // encrypt/decrypt, wrong-key isolation, put/get, rotation, rollback, audit
    // rows, and the most-recent-pool-wins ordering. (VA-H8 is a diag binary/script,
    // not a unit test.) Each test uses a fresh temp SQLite DB and a fixed 32-byte
    // master key so results never depend on the machine key or wall clock.

    fn open_test_store(master: [u8; 32]) -> (VaultStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fluxbee-vault-h-{}", Uuid::new_v4().simple()));
        let db = dir.join("vault.db");
        let store = VaultStore::open(&db, &master).expect("open vault store");
        (store, dir)
    }

    fn cleanup(dir: PathBuf) {
        let _ = fs::remove_dir_all(dir);
    }

    // ---- Secrets, stage 2 (docs/ilk-tenant-model-v1.md §2.2) ----

    #[test]
    fn assigned_nodes_read_a_secret_and_other_nodes_of_its_tenant_do_not() {
        let none = HashSet::new();
        let node = |ilk: &str| caller(Some(ilk), Some("tnt:client-a"), Some("agent"));
        let mut slack = metadata("tnt:client-a", None);
        slack.assigned_ilks = vec!["ilk:sales".to_string(), "ilk:support".to_string()];
        authorize_read(&node("ilk:sales"), &slack, &none, true).expect("assigned");
        assert_unauthorized(authorize_read(&node("ilk:billing"), &slack, &none, true));
        // General (no owner, no assigned node): every node of its tenant, nobody else.
        slack.assigned_ilks.clear();
        authorize_read(&node("ilk:billing"), &slack, &none, true).expect("general");
        assert_unauthorized(authorize_read(
            &caller(Some("ilk:x"), Some("tnt:client-b"), Some("agent")),
            &slack,
            &none,
            true,
        ));
        // A person's secret: its assigned node reads it, the rest of the tenant does not.
        let mut erp = metadata("tnt:client-a", Some("ilk:person"));
        erp.assigned_ilks = vec!["ilk:agent".to_string()];
        authorize_read(&node("ilk:agent"), &erp, &none, true)
            .expect("assigned to a person's secret");
        assert_unauthorized(authorize_read(&node("ilk:billing"), &erp, &none, true));
    }

    #[test]
    fn the_value_goes_out_only_while_its_tenant_is_active() {
        let (mut store, dir) = open_test_store([3u8; 32]);
        store
            .put(
                "pg:client-a:pool",
                json!({"url": "x"}),
                metadata("tnt:client-a", None),
                &admin_caller(),
            )
            .expect("put");
        let reader = caller(Some("ilk:node"), Some("tnt:client-a"), Some("agent"));
        let request = vault_request(MSG_VAULT_GET, json!({"key": "pg:client-a:pool"}));
        let suspended = |tenant: &str| -> VaultResult<()> {
            Err(VaultError::TenantNotActive(format!(
                "{tenant} is not active (suspended)"
            )))
        };
        let err = handle_get(
            &mut store,
            &request,
            &reader,
            &HashSet::new(),
            true,
            &suspended,
        )
        .expect_err("suspended");
        assert_eq!(err.code(), "UNAUTHORIZED");
        assert_eq!(audit_result_for_error(&err), "denied");
        // Its metadata still reads; the value goes out once the tenant is active.
        handle_get(
            &mut store,
            &request,
            &reader,
            &HashSet::new(),
            false,
            &suspended,
        )
        .expect("metadata");
        let active = |_: &str| -> VaultResult<()> { Ok(()) };
        let got = handle_get(
            &mut store,
            &request,
            &reader,
            &HashSet::new(),
            true,
            &active,
        )
        .expect("value");
        assert_eq!(got["value"]["url"], "x");
        // The genesis tenant needs no SHM; another one does, and an unreadable one is retried.
        let nowhere = Path::new("/nonexistent/fluxbee");
        ensure_tenant_active(nowhere, fluxbee_sdk::DEFAULT_ROOT_TENANT_ID).expect("genesis");
        let err = ensure_tenant_active(nowhere, "tnt:11111111-1111-4111-8111-111111111111")
            .expect_err("no SHM");
        assert_eq!(err.code(), "IDENTITY_UNAVAILABLE");
        cleanup(dir);
    }

    #[test]
    fn assign_replaces_the_nodes_and_tells_who_had_it() {
        let (mut store, dir) = open_test_store([4u8; 32]);
        let admin = admin_caller();
        let admins: HashSet<String> = HashSet::from(["ilk:admin".to_string()]);
        store
            .put(
                "slack:client-a:bot",
                json!({"t": 1}),
                metadata("tnt:client-a", None),
                &admin,
            )
            .expect("put a general secret");
        let assign = |tenant: &str, nodes: &[&str]| {
            vault_request(
                MSG_VAULT_ASSIGN,
                json!({"key": "slack:client-a:bot", "tenant_id": tenant, "assigned_ilks": nodes}),
            )
        };
        let a = "tnt:client-a";
        // Administrative, like a put.
        let node = caller(Some("ilk:sales"), Some(a), Some("agent"));
        let refused = handle_assign(
            &mut store,
            &assign(a, &["ilk:sales"]),
            &node,
            &admins,
            "motherbee",
        )
        .map(|_| ())
        .expect_err("not an admin");
        assert_eq!(refused.code(), "UNAUTHORIZED");
        // General -> two nodes: every node of the tenant is told, since it was general.
        let out = handle_assign(
            &mut store,
            &assign(a, &["ilk:support", "ilk:sales", "ilk:sales"]),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("assign");
        assert_eq!(
            out.response["assigned_ilks"],
            json!(["ilk:sales", "ilk:support"])
        );
        assert_eq!(out.response["general"], false);
        let event = out.broadcast.expect("an event");
        assert_eq!(event.op, VaultSecretOp::Assign);
        assert!(event.general);
        assert_eq!(event.assigned_ilks, vec!["ilk:sales", "ilk:support"]);
        // The same list again changes nothing and tells nobody.
        let again = handle_assign(
            &mut store,
            &assign(a, &["ilk:sales", "ilk:support"]),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("again");
        assert_eq!(again.response["changed"], false);
        assert!(again.broadcast.is_none());
        // Down to one: the node removed is told too; the value and its version stay.
        let out = handle_assign(
            &mut store,
            &assign(a, &["ilk:sales"]),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("unassign one");
        let event = out.broadcast.expect("an event");
        assert!(!event.general);
        assert_eq!(event.assigned_ilks, vec!["ilk:sales", "ilk:support"]);
        let record = store.get_record("slack:client-a:bot").unwrap().unwrap();
        assert_eq!(record.version, 1);
        assert_eq!(record.metadata.assigned_ilks, vec!["ilk:sales"]);
        // Another tenant's key reads like a missing one; a node id must be an ILK.
        let err = handle_assign(
            &mut store,
            &assign("tnt:client-b", &[]),
            &admin,
            &admins,
            "motherbee",
        )
        .map(|_| ())
        .expect_err("another tenant");
        assert_eq!(err.code(), "KEY_NOT_FOUND");
        let err = handle_assign(
            &mut store,
            &assign(a, &["IO.slack@motherbee"]),
            &admin,
            &admins,
            "motherbee",
        )
        .map(|_| ())
        .expect_err("not an ILK");
        assert_eq!(err.code(), "INVALID_REQUEST");
        cleanup(dir);
    }

    #[test]
    fn a_new_value_keeps_who_uses_the_secret() {
        let (mut store, dir) = open_test_store([6u8; 32]);
        let admin = admin_caller();
        let admins: HashSet<String> = HashSet::from(["ilk:admin".to_string()]);
        let put = |value: Value, metadata: Value| {
            vault_request(
                MSG_VAULT_PUT,
                json!({"key": "openai:a:vip", "value": value, "metadata": metadata}),
            )
        };
        let base = json!({"tenant_id": "tnt:client-a", "resource_type": "openai"});
        let mut assigned = base.clone();
        assigned["assigned_ilks"] = json!(["ilk:vip"]);
        handle_put(
            &mut store,
            &put(json!({"k": 1}), assigned),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("put assigned");
        // Refreshing the value without naming who uses it keeps the assignment: it does not
        // become the tenant's general secret, and the event says so.
        let out = handle_put(
            &mut store,
            &put(json!({"k": 2}), base.clone()),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("refresh the value");
        let event = out.broadcast.expect("an event");
        assert!(!event.general);
        assert_eq!(event.assigned_ilks, vec!["ilk:vip"]);
        let record = store.get_record("openai:a:vip").unwrap().unwrap();
        assert_eq!(record.metadata.assigned_ilks, vec!["ilk:vip"]);
        assert_eq!(record.version, 2);
        // A put that names an owner replaces who uses it; the nodes that lost it are told.
        let mut owned = base.clone();
        owned["ilk"] = json!("ilk:owner");
        let out = handle_put(
            &mut store,
            &put(json!({"k": 3}), owned),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("put owned");
        let event = out.broadcast.expect("an event");
        assert_eq!(event.ilk.as_deref(), Some("ilk:owner"));
        assert_eq!(event.assigned_ilks, vec!["ilk:vip"]);
        // A new owner: the previous one is told too.
        let mut other = base;
        other["ilk"] = json!("ilk:other");
        let out = handle_put(
            &mut store,
            &put(json!({"k": 4}), other),
            &admin,
            &admins,
            "motherbee",
        )
        .expect("another owner");
        assert_eq!(
            out.broadcast.expect("an event").assigned_ilks,
            vec!["ilk:owner"]
        );
        cleanup(dir);
    }

    #[test]
    fn the_tenant_decision_refuses_inactive_and_retries_unknown() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let with = |status: Option<&str>| {
            let status = status.map(str::to_string);
            tenant_check(tenant, move || Ok(status))
        };
        with(Some("active")).expect("active");
        for status in ["suspended", "pending"] {
            let err = with(Some(status)).expect_err(status);
            assert_eq!(err.code(), "UNAUTHORIZED", "{status}");
            assert!(err.to_string().contains(status));
        }
        // Not in the SHM: deleted, or identity not loaded yet; retryable, still not handed out.
        let err = with(None).expect_err("unknown");
        assert_eq!(err.code(), "IDENTITY_UNAVAILABLE");
        assert_eq!(audit_result_for_error(&err), "denied");
        // A malformed tenant id is refused, never retried forever.
        let err = tenant_check("tnt:client-a", || Ok(Some("active".to_string())))
            .expect_err("not a uuid");
        assert_eq!(err.code(), "UNAUTHORIZED");
    }

    #[test]
    fn a_caller_proves_its_ilk_belongs_to_the_node_that_sent_the_request() {
        let registered = |ilk: &str, node: &str| fluxbee_sdk::identity::IdentityIlkOption {
            ilk_id: ilk.to_string(),
            tenant_id: "tnt:11111111-1111-4111-8111-111111111111".to_string(),
            display_name: None,
            handler_node: Some(node.to_string()),
            registration_status: "complete".to_string(),
            ilk_type: "agent".to_string(),
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
        };
        let admin = fluxbee_sdk::deterministic_system_ilk_id("SY.admin@motherbee");
        // A system node proves its derived ILK by its name alone: no SHM at boot.
        check_caller_binding(Some("SY.admin@motherbee"), &admin, None).expect("SY.admin at boot");
        // A node's ILK is the one identity registered for its name (any case of the name).
        let slack = registered("ilk:slack", "IO.slack.sales@motherbee");
        check_caller_binding(Some("IO.slack.sales@motherbee"), "ilk:slack", Some(&slack))
            .expect("its own");
        check_caller_binding(Some("io.SLACK.sales@motherbee"), "ilk:slack", Some(&slack))
            .expect("case");
        // Claiming another node's ILK, SY.admin's included, is refused.
        let err = check_caller_binding(Some("IO.slack.evil@motherbee"), "ilk:slack", Some(&slack))
            .expect_err("another node's");
        assert_eq!(err.code(), "UNAUTHORIZED");
        assert_eq!(audit_result_for_error(&err), "denied");
        let admin_entry = registered(&admin, "SY.admin@motherbee");
        let err = check_caller_binding(Some("IO.slack.evil@motherbee"), &admin, Some(&admin_entry))
            .expect_err("SY.admin's");
        assert_eq!(err.code(), "UNAUTHORIZED");
        let err = check_caller_binding(Some("SY.cognition@motherbee"), &admin, Some(&admin_entry))
            .expect_err("another system node's");
        assert_eq!(err.code(), "UNAUTHORIZED");
        // An ILK the SHM does not hold: retryable, still nothing read.
        let err = check_caller_binding(Some("IO.slack.new@motherbee"), "ilk:unknown", None)
            .expect_err("unknown");
        assert_eq!(err.code(), "IDENTITY_UNAVAILABLE");
        // No router-stamped name: refused.
        let err = check_caller_binding(None, "ilk:slack", Some(&slack)).expect_err("no name");
        assert_eq!(err.code(), "UNAUTHORIZED");
    }

    #[test]
    fn the_general_filter_leaves_out_assigned_secrets() {
        let (mut store, dir) = open_test_store([5u8; 32]);
        let admin = admin_caller();
        let mut assigned = metadata("tnt:client-a", None);
        assigned.assigned_ilks = vec!["ilk:sales".to_string()];
        store
            .put("pg:a:assigned", json!({"u": 1}), assigned, &admin)
            .expect("put assigned");
        store
            .put(
                "pg:a:general",
                json!({"u": 2}),
                metadata("tnt:client-a", None),
                &admin,
            )
            .expect("put general");
        let keys = |filter: VaultFilter| -> Vec<String> {
            let mut keys: Vec<String> = store
                .list(filter, &admin)
                .expect("list")
                .into_iter()
                .map(|summary| summary.key)
                .collect();
            keys.sort();
            keys
        };
        let general = VaultFilter {
            tenant_id: Some("tnt:client-a".to_string()),
            ilk: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(keys(general), vec!["pg:a:general"]);
        let mine = VaultFilter {
            tenant_id: Some("tnt:client-a".to_string()),
            assigned_ilk: Some("ilk:sales".to_string()),
            ..Default::default()
        };
        assert_eq!(keys(mine), vec!["pg:a:assigned"]);
        cleanup(dir);
    }

    fn admin_caller() -> Caller {
        caller(Some("ilk:admin"), Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID), Some("system"))
    }

    #[test]
    fn va_h1_validate_key_accepts_valid_rejects_invalid() {
        for k in [
            "pg:root:pool",
            "openai:tnt-1:pool",
            "ssh:worker-1",
            "a",
            "abc_123-x:y",
        ] {
            assert!(validate_key(k).is_ok(), "{k} should be valid");
        }
        for bad in [
            "",            // empty
            "Key",         // uppercase prefix
            ":leading",    // symbol prefix
            "-leading",    // symbol prefix
            "has space",   // space
            "UPPER",       // uppercase
            "slash/here",  // '/' not allowed (this is why ssh:<id> is used, not ssh/<id>)
            "dot.here",    // '.' not allowed
        ] {
            assert!(
                matches!(validate_key(bad), Err(VaultError::InvalidRequest(_))),
                "{bad:?} should be rejected"
            );
        }
        assert!(validate_key(&"a".repeat(257)).is_err(), "over-256 key rejected");
    }

    #[test]
    fn va_h2_encrypt_decrypt_roundtrip() {
        let (store, dir) = open_test_store([7u8; 32]);
        let value = json!({"url":"postgres://u:p@h:5432","n":42,"nested":{"a":[1,2,3]}});
        let (nonce, ciphertext) = store.encrypt_value(&value).expect("encrypt");
        assert_eq!(nonce.len(), 12, "GCM nonce is 12 bytes");
        assert_ne!(
            ciphertext,
            serde_json::to_vec(&value).unwrap(),
            "ciphertext must not equal plaintext"
        );
        let back = store.decrypt_value(&nonce, &ciphertext).expect("decrypt");
        assert_eq!(back, value);
        cleanup(dir);
    }

    #[test]
    fn va_h3_wrong_master_key_cannot_decrypt() {
        let (store_a, dir_a) = open_test_store([1u8; 32]);
        let value = json!({"secret":"do-not-leak"});
        let (nonce, ciphertext) = store_a.encrypt_value(&value).expect("encrypt with key A");
        let (store_b, dir_b) = open_test_store([2u8; 32]);
        assert!(
            matches!(
                store_b.decrypt_value(&nonce, &ciphertext),
                Err(VaultError::Encryption)
            ),
            "a store with a different master key must NOT decrypt (AEAD auth fails)"
        );
        cleanup(dir_a);
        cleanup(dir_b);
    }

    #[test]
    fn va_h4_put_get_roundtrip_and_idempotent_noop() {
        let (mut store, dir) = open_test_store([9u8; 32]);
        let c = admin_caller();
        let value = json!({"api_key":"sk-test"});
        let (version, changed) = store
            .put("pg:root:pool", value.clone(), metadata("tnt:root", None), &c)
            .expect("put");
        assert_eq!(version, 1);
        assert!(changed, "first put is a change");
        let rec = store.get_record("pg:root:pool").expect("get").expect("row");
        let stored = store
            .decrypt_value(&rec.current_nonce, &rec.current_ciphertext)
            .expect("decrypt stored");
        assert_eq!(stored, value);
        // Identical re-put is a no-op: version unchanged, changed=false.
        let (v2, changed2) = store
            .put("pg:root:pool", value, metadata("tnt:root", None), &c)
            .expect("re-put");
        assert_eq!(v2, 1);
        assert!(!changed2, "identical re-put must be a no-op");
        cleanup(dir);
    }

    #[test]
    fn va_h5_rotate_keeps_previous_and_bumps_version() {
        let (mut store, dir) = open_test_store([3u8; 32]);
        let c = admin_caller();
        store
            .put("pg:root:pool", json!({"v":1}), metadata("tnt:root", None), &c)
            .expect("put v1");
        let res = store
            .rotate("pg:root:pool", json!({"v":2}), &c)
            .expect("rotate");
        assert_eq!(res.current_version, 2);
        assert_eq!(res.previous_version, 1);
        let rec = store.get_record("pg:root:pool").unwrap().unwrap();
        assert_eq!(
            store
                .decrypt_value(&rec.current_nonce, &rec.current_ciphertext)
                .unwrap(),
            json!({"v":2}),
            "current is the rotated value"
        );
        let pn = rec.previous_nonce.expect("previous nonce retained");
        let pc = rec.previous_ciphertext.expect("previous ciphertext retained");
        assert_eq!(
            store.decrypt_value(&pn, &pc).unwrap(),
            json!({"v":1}),
            "previous version is recoverable"
        );
        cleanup(dir);
    }

    #[test]
    fn va_h6_rollback_restores_previous_and_errors_without_previous() {
        let (mut store, dir) = open_test_store([4u8; 32]);
        let c = admin_caller();
        store
            .put("pg:root:pool", json!({"v":1}), metadata("tnt:root", None), &c)
            .expect("put v1");
        // No previous version yet -> rollback is rejected, not a silent no-op.
        assert!(matches!(
            store.rollback("pg:root:pool", &c),
            Err(VaultError::NoPreviousVersion)
        ));
        store
            .rotate("pg:root:pool", json!({"v":2}), &c)
            .expect("rotate to v2");
        let rb = store.rollback("pg:root:pool", &c).expect("rollback");
        assert_eq!(rb.current_version, 1, "rollback restores the previous version number");
        let rec = store.get_record("pg:root:pool").unwrap().unwrap();
        assert_eq!(
            store
                .decrypt_value(&rec.current_nonce, &rec.current_ciphertext)
                .unwrap(),
            json!({"v":1}),
            "rollback restores the previous value"
        );
        cleanup(dir);
    }

    #[test]
    fn a_put_never_takes_over_another_tenants_key() {
        let (mut store, dir) = open_test_store([11u8; 32]);
        let c = admin_caller();
        let tenant_a = "tnt:11111111-1111-4111-8111-111111111111";
        let tenant_b = "tnt:22222222-2222-4222-8222-222222222222";
        store
            .put(
                "google:acme:sales",
                json!({"t":"a"}),
                metadata(tenant_a, None),
                &c,
            )
            .expect("first put");
        // Another tenant: refused, with a new value and with the very same one (no ok no-op).
        for value in [json!({"t":"b"}), json!({"t":"a"})] {
            let err = store
                .put("google:acme:sales", value, metadata(tenant_b, None), &c)
                .expect_err("another tenant's key is taken");
            assert!(matches!(err, VaultError::KeyTaken));
            assert_eq!(err.code(), "KEY_TAKEN");
        }
        let rec = store.get_record("google:acme:sales").unwrap().unwrap();
        assert_eq!(rec.metadata.tenant_id, tenant_a);
        assert_eq!(rec.version, 1);
        // Its own tenant still writes it.
        let (version, changed) = store
            .put(
                "google:acme:sales",
                json!({"t":"a2"}),
                metadata(tenant_a, None),
                &c,
            )
            .expect("own tenant rewrites");
        assert_eq!((version, changed), (2, true));
        cleanup(dir);
    }

    #[test]
    fn another_tenants_key_is_answered_as_missing() {
        let tenant_a = "tnt:11111111-1111-4111-8111-111111111111";
        let tenant_b = "tnt:22222222-2222-4222-8222-222222222222";
        let md = metadata(tenant_a, None);
        assert!(ensure_in_tenant(Some(&md), tenant_a).is_ok());
        // A missing key passes: delete/rotate/rollback then report it as before.
        assert!(ensure_in_tenant(None, tenant_b).is_ok());
        let err = ensure_in_tenant(Some(&md), tenant_b).expect_err("another tenant");
        // Indistinguishable from a missing key on the wire: same code, same text.
        assert_eq!(err.code(), VaultError::KeyNotFound.code());
        assert_eq!(err.to_string(), VaultError::KeyNotFound.to_string());
        // Only the audit row says it was a refusal.
        assert_eq!(audit_result_for_error(&err), "denied");
        assert_eq!(audit_result_for_error(&VaultError::KeyNotFound), "error");
    }

    #[test]
    fn every_scoped_request_names_a_canonical_tenant() {
        assert!(validate_request_tenant(" tnt:11111111-1111-4111-8111-111111111111 ").is_ok());
        for bad in ["", "  ", "sys", "11111111-1111-4111-8111-111111111111"] {
            assert!(validate_request_tenant(bad).is_err(), "{bad:?}");
        }
        // The wire refuses a delete, rotate or rollback without a tenant.
        assert!(serde_json::from_value::<VaultScopedKeyRequest>(json!({"key":"k"})).is_err());
        assert!(
            serde_json::from_value::<VaultRotateRequest>(json!({"key":"k","value":1})).is_err()
        );
    }

    #[test]
    fn va_h7_audit_log_records_operations_and_caller() {
        let (mut store, dir) = open_test_store([5u8; 32]);
        let c = admin_caller();
        store
            .put("pg:root:pool", json!({"v":1}), metadata("tnt:root", None), &c)
            .expect("put");
        store
            .rotate("pg:root:pool", json!({"v":2}), &c)
            .expect("rotate");
        let success_rows: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE operation IN (?1, ?2) AND result = 'success'",
                params![MSG_VAULT_PUT, MSG_VAULT_ROTATE],
                |row| row.get(0),
            )
            .expect("count audit rows");
        assert_eq!(success_rows, 2, "put + rotate each write a success audit row");
        let recorded_ilk: Option<String> = store
            .conn
            .query_row(
                "SELECT caller_ilk FROM audit_log WHERE operation = ?1 LIMIT 1",
                params![MSG_VAULT_PUT],
                |row| row.get(0),
            )
            .expect("read audit caller");
        assert_eq!(recorded_ilk.as_deref(), Some("ilk:admin"));
        cleanup(dir);
    }

    #[test]
    fn va_f9b_db_file_is_owner_only_0600() {
        // The at-rest vault DB (and its WAL sidecars) must be 0600, not SQLite's default
        // 0644 — even though the rows are ciphertext, the encrypted blobs stay owner-only.
        let (mut store, dir) = open_test_store([8u8; 32]);
        // Force a write so the -wal/-shm sidecars exist and get tightened too.
        store
            .put("pg:root:pool", json!({"v":1}), metadata("tnt:root", None), &admin_caller())
            .expect("put");
        for suffix in ["", "-wal", "-shm"] {
            let path = dir.join(format!("vault.db{suffix}"));
            if path.exists() {
                let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{} must be 0600, got {:o}", path.display(), mode);
            }
        }
        cleanup(dir);
    }

    #[test]
    fn va_j10c_list_returns_most_recently_written_pool_secret_first() {
        // The consumer-resolution contract (VaultClient::resolve_resource takes the FIRST
        // list match) requires list() to return the most-recently-WRITTEN secret first when
        // several share (resource_type, tenant, ilk=null). Guards ORDER BY updated_at DESC:
        // ordering by created_at alone would let a re-put/rotated key lose to an older-created
        // sibling (created_at is frozen at INSERT); ordering by key returns the wrong one too.
        let (mut store, dir) = open_test_store([6u8; 32]);
        let c = admin_caller();
        let set_times = |store: &VaultStore, key: &str, created: &str, updated: &str| {
            store
                .conn
                .execute(
                    "UPDATE secrets SET created_at = ?2, updated_at = ?3 WHERE key = ?1",
                    params![key, created, updated],
                )
                .unwrap();
        };
        let pool_filter = || VaultFilter {
            resource_type: Some("postgres".to_string()),
            tenant_id: Some("tnt:root".to_string()),
            ilk: Some(String::new()), // pool only (no owner ILK)
            ..Default::default()
        };
        let first_key = |store: &VaultStore| {
            store
                .list(pool_filter(), &c)
                .expect("list")
                .first()
                .map(|s| s.key.clone())
        };

        store
            .put("pg:root:pool-a", json!({"which":"a"}), metadata("tnt:root", None), &c)
            .expect("put a");
        store
            .put("pg:root:pool-b", json!({"which":"b"}), metadata("tnt:root", None), &c)
            .expect("put b");
        // pool-a created+written older, pool-b newer. pool-b is alphabetically AFTER pool-a,
        // so ORDER BY key would wrongly return pool-a.
        set_times(&store, "pg:root:pool-a", "2020-01-01T00:00:00+00:00", "2020-01-01T00:00:00+00:00");
        set_times(&store, "pg:root:pool-b", "2026-07-21T00:00:00+00:00", "2026-07-21T00:00:00+00:00");
        assert_eq!(
            first_key(&store).as_deref(),
            Some("pg:root:pool-b"),
            "the newer sibling must win"
        );

        // Re-put pool-a with a NEW value: created_at stays 2020 but the write bumps updated_at.
        // Pin it explicitly to the newest time; most-recently-WRITTEN (pool-a) must now win even
        // though it was created FIRST — this is the created_at-vs-updated_at distinction.
        store
            .put("pg:root:pool-a", json!({"which":"a2"}), metadata("tnt:root", None), &c)
            .expect("re-put a");
        set_times(&store, "pg:root:pool-a", "2020-01-01T00:00:00+00:00", "2027-01-01T00:00:00+00:00");
        assert_eq!(
            first_key(&store).as_deref(),
            Some("pg:root:pool-a"),
            "the most-recently-WRITTEN secret must win, not the most-recently-created"
        );
        cleanup(dir);
    }
}
