#![forbid(unsafe_code)]

//! `IO.blob` is the motherbee-local curator for public artifacts.
//!
//! It is intentionally not an authority and has no public channel. `SY.admin` authenticates the
//! producer, resolves publication ownership, and then sends one of the bounded worker commands
//! handled here. The worker validates a `BlobRef`, copies its bytes to `public/<full-sha256>`, and
//! maintains an idempotent publication/refcount ledger.
//!
//! Configuration model: io.blob is a FIRST-CLASS MANAGED IO NODE like io.api/io.slack. Its tunables
//! (`max_bytes`, `admin_hive`) come from the managed CONFIG_SET/GET control plane, NOT from ENV. The
//! blob roots and ledger path are FIXED design paths (node defaults, dirs created at install). A
//! curator with no operator config is VALID and runs on defaults (it is not `FAILED_CONFIG`).

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::blob::{BlobConfig, BlobError, BlobRef, BlobToolkit, BLOB_DEFAULT_MAX_BYTES};
use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_BLOB_CURATE, MSG_BLOB_CURATE_RESPONSE,
    MSG_BLOB_RELEASE, MSG_BLOB_RELEASE_RESPONSE, MSG_BLOB_STATUS_GET, MSG_BLOB_STATUS_GET_RESPONSE,
    MSG_CONFIG_GET, MSG_CONFIG_SET, MSG_NODE_STATUS_GET, SYSTEM_KIND,
};
use fluxbee_sdk::{
    managed_node_config_path, try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, FLUXBEE_NODE_NAME_ENV,
};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_blob_adapter_config::{
    configured_admin_hive, configured_max_bytes, IoBlobAdapterConfigContract,
};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload,
    ensure_config_version_advances, parse_and_validate_io_control_plane_request, IoConfigSource,
    IoControlPlaneErrorInfo, IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_persist_error,
    log_config_set_stale_rejected, log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::persist_io_control_plane_state;
use nix::fcntl::{open, OFlag};
use nix::sys::stat::{fstat, Mode, SFlag};
use nix::unistd::{close, read};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn std::error::Error + Send + Sync>;

const RPC_CH_CONTROL: &str = "control";
const LEDGER_SCHEMA_VERSION: u32 = 1;
// Rutas FIJAS de diseño del sistema: NO son config de operador ni ENV. Los dirs se crean al
// instalar; el nodo solo usa estas rutas (con `ensure_dir` como red de seguridad en boot).
const DEFAULT_BLOB_ROOT: &str = "/var/lib/fluxbee/blob";
const DEFAULT_PUBLIC_ROOT: &str = "/var/lib/fluxbee/blob/public";
const DEFAULT_LEDGER_PATH: &str = "/var/lib/fluxbee/state/io-blob/publications.json";

/// Infra de arranque leida del managed spawn (env inyectado por el runtime / `_system` de
/// `config.json`), NO los tunables de operador. Los tunables (`max_bytes`, `admin_hive`) viven en el
/// plano de control (`RuntimeState.control_plane`).
#[derive(Debug, Clone)]
struct Config {
    node_name: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    orchestrator_target: String,
}

impl Config {
    /// Carga la infra al estilo io.api: env primero, luego el `node`/`_system` del spawn `config.json`,
    /// luego el default. A diferencia de io.api, la lectura del spawn doc es TOLERANTE: un io.blob sin
    /// `config.json` (o con uno ilegible) es un curador valido con defaults, no un arranque abortado —
    /// el plano de control clasifica por separado una config corrupta como FAILED_CONFIG.
    fn load() -> Result<Self, DynError> {
        let node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            format!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let hive_id = node_name
            .split_once('@')
            .map(|(_, hive)| hive.trim().to_string())
            .filter(|hive| !hive.is_empty())
            .ok_or_else(|| {
                format!("invalid {FLUXBEE_NODE_NAME_ENV}='{node_name}': expected <name>@<hive>")
            })?;
        let spawn_doc = load_spawn_doc(&node_name);
        Ok(Self {
            node_name,
            node_version: env("NODE_VERSION")
                .or_else(|| json_get_string(&spawn_doc, "_system.runtime_version"))
                .unwrap_or_else(|| "0.1.0".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| json_get_string(&spawn_doc, "node.router_socket"))
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| json_get_string(&spawn_doc, "node.uuid_persistence_dir"))
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string(&spawn_doc, "node.config_dir"))
                    .unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            // El orchestrator es una segunda autoridad de CONFIG (ademas de SY.admin). No es un
            // tunable de operador, asi que sale del spawn doc o del default del hive, nunca de un
            // env IO_BLOB_*.
            orchestrator_target: json_get_string(&spawn_doc, "node.orchestrator_target")
                .unwrap_or_else(|| format!("SY.orchestrator@{hive_id}")),
        })
    }
}

#[derive(Debug, Clone)]
struct BlobCurator {
    toolkit: BlobToolkit,
    blob_root: PathBuf,
    public_root: PathBuf,
    ledger_path: PathBuf,
    max_bytes: u64,
}

/// Estado de runtime del nodo, al estilo `io.api::RuntimeState`. El curador vive detras de un `Mutex`
/// porque un CONFIG_SET de `max_bytes` lo reconstruye en caliente (rutas fijas, sin reiniciar el
/// proceso).
struct RuntimeState {
    config: Config,
    local_hive: String,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    dispatcher: Arc<RouterDispatcher>,
    control_plane: RwLock<IoControlPlaneState>,
    control_metrics: IoControlPlaneMetrics,
    curator: Mutex<BlobCurator>,
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
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,io_blob=debug,io_common=info,fluxbee_sdk=info")
        }))
        .init();

    let config = Config::load()?;

    // Plano de control: boot lee SOLO el `config.json` del dir del nodo, validado por el contrato del
    // adapter via el SDK (single-config, sin archivo dinamico que ensombrezca un respawn — BUG-4).
    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoBlobAdapterConfigContract);
    let mut boot_state =
        bootstrap_io_control_plane_state(&config.node_name, adapter_contract.as_ref())
            .unwrap_or_else(|err| IoControlPlaneState {
                current_state: IoNodeLifecycleState::FailedConfig,
                config_source: IoConfigSource::None,
                schema_version: 1,
                config_version: 0,
                effective_config: None,
                last_error: Some(IoControlPlaneErrorInfo {
                    code: "config_bootstrap_failed".to_string(),
                    message: err.to_string(),
                }),
            });
    validate_boot_effective_config(&mut boot_state, adapter_contract.as_ref());

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
        // Plano de control managed: CONFIG_GET/SET entran por el mismo canal.
        .post_pending_rule(
            RouteMatch::one_of(SYSTEM_KIND, [MSG_CONFIG_GET, MSG_CONFIG_SET]),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        .build()?;

    let node_config = NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    };
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    let full_name = sender.full_name().to_string();
    let local_hive = full_name
        .rsplit_once('@')
        .map(|(_, hive)| hive.to_string())
        .unwrap_or_else(|| "motherbee".to_string());

    // Red de seguridad fail-closed: io.blob es SOLO-motherbee. Un runtime managed ya no tiene el
    // ExecCondition de systemd que antes vetaba spokes, asi que el chequeo se hace aca.
    if local_hive != "motherbee" {
        tracing::error!(
            node = %full_name,
            hive = %local_hive,
            "IO.blob is motherbee-only; refusing to run on a non-motherbee hive"
        );
        return Err(format!("IO.blob must run on motherbee, not '{local_hive}'").into());
    }

    // El tope inicial sale del plano de control vivo (default baked si no hay config).
    let initial_max_bytes =
        configured_max_bytes(boot_state.effective_config.as_ref()).unwrap_or(BLOB_DEFAULT_MAX_BYTES);
    let curator = build_curator(initial_max_bytes)?;

    let control_metrics = IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    );

    tracing::info!(
        node = %full_name,
        hive = %local_hive,
        lifecycle = %boot_state.current_state.as_str(),
        config_version = boot_state.config_version,
        max_bytes = initial_max_bytes,
        public_root = %curator.public_root.display(),
        ledger_path = %curator.ledger_path.display(),
        orchestrator_target = %config.orchestrator_target,
        "IO.blob curator ready (managed control plane)"
    );

    let state = RuntimeState {
        config,
        local_hive,
        adapter_contract,
        dispatcher: dispatcher.clone(),
        control_plane: RwLock::new(boot_state),
        control_metrics,
        curator: Mutex::new(curator),
    };

    let mut incoming = dispatcher.take_command_receiver(RPC_CH_CONTROL).await?;

    while let Some(message) = incoming.recv().await {
        if try_handle_default_node_status(&sender, &message).await? {
            continue;
        }

        // Plano de control ANTES de la compuerta de admin del curador: CONFIG_GET/SET los autoriza el
        // admin O el orchestrator (no la compuerta `expected_admin` del curador).
        if is_config_command(&message) {
            let response = handle_control_message(&state, &message).await;
            sender.send(response).await?;
            continue;
        }

        let response_msg = response_message_for(message.meta.msg.as_deref());
        // `expected_admin` se lee del plano de control VIVO, asi que un CONFIG_SET de `admin_hive`
        // tiene efecto sin reiniciar.
        let expected_admin = current_expected_admin(&state).await;
        let result = match authorize_admin_message(&message, &expected_admin) {
            Err(err) => Err(err),
            Ok(()) => {
                let curator = state.curator.lock().await.clone();
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

// --- Plano de control (espejo de io.api) --------------------------------------------------------

fn validate_boot_effective_config(
    state: &mut IoControlPlaneState,
    contract: &dyn IoAdapterConfigContract,
) {
    let Some(candidate) = state.effective_config.as_ref() else {
        return;
    };
    match contract.validate_and_materialize(candidate) {
        Ok(effective) => {
            state.effective_config = Some(effective);
            state.current_state = IoNodeLifecycleState::Configured;
            state.last_error = None;
        }
        Err(err) => {
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: err.code().to_string(),
                message: err.to_string(),
            });
        }
    }
}

fn is_config_command(message: &Message) -> bool {
    message.meta.msg_type.eq_ignore_ascii_case(SYSTEM_KIND)
        && matches!(
            message.meta.msg.as_deref(),
            Some(command)
                if command.eq_ignore_ascii_case("CONFIG_GET")
                    || command.eq_ignore_ascii_case("CONFIG_SET")
        )
}

/// `SY.admin@{admin_hive}` derivado del plano de control VIVO (default = hive local). io.blob NO
/// tiene un `admin_target` fijo como io.api: su autoridad de admin es config-driven (`admin_hive`).
async fn current_expected_admin(state: &RuntimeState) -> String {
    let effective = state.control_plane.read().await;
    let hive = configured_admin_hive(effective.effective_config.as_ref())
        .unwrap_or_else(|| state.local_hive.clone());
    format!("SY.admin@{hive}")
}

fn control_caller_authorized(state: &RuntimeState, message: &Message, expected_admin: &str) -> bool {
    let caller = message.routing.src_l2_name.as_deref().map(str::trim);
    caller == Some(expected_admin) || caller == Some(state.config.orchestrator_target.as_str())
}

async fn handle_control_message(state: &RuntimeState, message: &Message) -> Message {
    let expected_admin = current_expected_admin(state).await;
    let payload = if !control_caller_authorized(state, message, &expected_admin) {
        tracing::warn!(
            trace_id = %message.routing.trace_id,
            source = ?message.routing.src_l2_name,
            command = ?message.meta.msg,
            "IO.blob rejected configuration command from non-authority"
        );
        let snapshot = state.control_plane.read().await.clone();
        build_io_config_set_error_payload(
            &state.config.node_name,
            &snapshot,
            "unauthorized",
            "CONFIG_GET/SET requires the configured Admin or Orchestrator origin",
        )
    } else {
        match parse_and_validate_io_control_plane_request(message, &state.config.node_name) {
            Ok(IoControlPlaneRequest::Get(_)) => build_config_get_payload(state, message).await,
            Ok(IoControlPlaneRequest::Set(set)) => apply_config_set(state, &set).await,
            Err(err) => {
                log_control_plane_request_rejected(
                    &message.routing.trace_id,
                    &state.config.node_name,
                    err.code(),
                    &err.to_string(),
                );
                let snapshot = state.control_plane.read().await.clone();
                build_io_config_set_error_payload(
                    &state.config.node_name,
                    &snapshot,
                    err.code(),
                    err.to_string(),
                )
            }
        }
    };
    let mut response = build_io_config_response_message(message, payload);
    response.routing.src = state.dispatcher.sender_snapshot().uuid().to_string();
    response
}

async fn build_config_get_payload(state: &RuntimeState, message: &Message) -> Value {
    let snapshot = state.control_plane.read().await.clone();
    log_config_get_served(&message.routing.trace_id, &state.config.node_name, &snapshot);
    let mut payload = build_io_config_get_response_payload(
        &state.config.node_name,
        &snapshot,
        build_io_adapter_contract_payload(
            state.adapter_contract.as_ref(),
            snapshot.effective_config.as_ref(),
        ),
    );
    inject_runtime_status(&mut payload, state).await;
    payload
}

async fn apply_config_set(
    state: &RuntimeState,
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
) -> Value {
    let current = state.control_plane.read().await.clone();
    if let Err(err) = ensure_config_version_advances(payload.config_version, current.config_version)
    {
        log_config_set_stale_rejected(
            &state.config.node_name,
            payload.config_version,
            current.config_version,
        );
        state.control_metrics.record_config_set_error(
            current.current_state.as_str(),
            current.config_version,
            err.code(),
        );
        return build_io_config_set_error_payload(
            &state.config.node_name,
            &current,
            err.code(),
            err.to_string(),
        );
    }
    let effective =
        match apply_adapter_config_replace(state.adapter_contract.as_ref(), &payload.config) {
            Ok(value) => value,
            Err(err) => {
                state.control_metrics.record_config_set_error(
                    current.current_state.as_str(),
                    current.config_version,
                    err.code(),
                );
                return build_io_config_set_error_payload(
                    &state.config.node_name,
                    &current,
                    err.code(),
                    err.to_string(),
                );
            }
        };
    let next = IoControlPlaneState {
        current_state: IoNodeLifecycleState::Configured,
        config_source: IoConfigSource::Dynamic,
        schema_version: payload.schema_version,
        config_version: payload.config_version,
        effective_config: Some(effective.clone()),
        last_error: None,
    };
    if let Err(err) = persist_io_control_plane_state(&state.config.node_name, &next) {
        log_config_set_persist_error(
            &state.config.node_name,
            payload.schema_version,
            payload.config_version,
            &err.to_string(),
        );
        return build_io_config_set_error_payload(
            &state.config.node_name,
            &current,
            "config_persist_error",
            err.to_string(),
        );
    }

    // Diferencias contra la config previa para decidir el hot-apply.
    let old_max =
        configured_max_bytes(current.effective_config.as_ref()).unwrap_or(BLOB_DEFAULT_MAX_BYTES);
    let new_max = configured_max_bytes(Some(&effective)).unwrap_or(BLOB_DEFAULT_MAX_BYTES);
    let admin_hive_changed = configured_admin_hive(current.effective_config.as_ref())
        != configured_admin_hive(Some(&effective));

    *state.control_plane.write().await = next.clone();
    state
        .control_metrics
        .record_config_set_ok(next.current_state.as_str(), next.config_version);

    let mut hot_applied = Vec::new();
    if new_max != old_max {
        // El curador guarda `max_bytes` por valor. Las rutas son FIJAS, asi que un cambio de tope se
        // aplica en caliente reconstruyendo el curador en proceso (relee el ledger, barato) — nunca
        // hace falta reiniciar el nodo.
        match build_curator(new_max) {
            Ok(rebuilt) => {
                *state.curator.lock().await = rebuilt;
                hot_applied.push("io.max_bytes".to_string());
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "validated IO.blob max_bytes failed to hot-apply; keeping previous curator"
                );
            }
        }
    }
    if admin_hive_changed {
        // `expected_admin` se lee del plano de control vivo en cada comando, asi que ya quedo aplicado.
        hot_applied.push("io.admin_hive".to_string());
    }
    log_config_set_applied(
        &state.config.node_name,
        payload.schema_version,
        payload.config_version,
        &hot_applied,
        &[],
        &[],
    );

    let mut response = build_io_config_set_ok_payload(&state.config.node_name, &next);
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "apply".to_string(),
            json!({
                "mode":"hot_reload",
                "hot_applied":hot_applied,
                "reinit_performed":[],
                "restart_required":[],
            }),
        );
    }
    inject_runtime_status(&mut response, state).await;
    response
}

async fn inject_runtime_status(payload: &mut Value, state: &RuntimeState) {
    let (public_root, ledger_path, max_bytes) = {
        let curator = state.curator.lock().await;
        (
            curator.public_root.display().to_string(),
            curator.ledger_path.display().to_string(),
            curator.max_bytes,
        )
    };
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "runtime".to_string(),
            json!({
                "transport":"router_socket",
                "role":"public-artifact-curator",
                "public_root":public_root,
                "ledger_path":ledger_path,
                "max_bytes":max_bytes,
                "control_plane_metrics":state.control_metrics.snapshot(),
            }),
        );
    }
}

/// Construye un curador con las rutas FIJAS de diseño y el tope dado. Unico punto de creacion en
/// produccion (boot + hot-apply de `max_bytes`).
fn build_curator(max_bytes: u64) -> Result<BlobCurator, CuratorError> {
    BlobCurator::new(
        PathBuf::from(DEFAULT_BLOB_ROOT),
        PathBuf::from(DEFAULT_PUBLIC_ROOT),
        PathBuf::from(DEFAULT_LEDGER_PATH),
        max_bytes,
    )
}

impl BlobCurator {
    fn new(
        blob_root: PathBuf,
        public_root: PathBuf,
        ledger_path: PathBuf,
        max_bytes: u64,
    ) -> Result<Self, CuratorError> {
        ensure_dir(&public_root, 0o750)?;
        if let Some(parent) = ledger_path.parent() {
            ensure_dir(parent, 0o750)?;
        }
        let toolkit = BlobToolkit::new(BlobConfig {
            blob_root: blob_root.clone(),
            max_blob_bytes: Some(max_bytes),
            ..BlobConfig::default()
        })?;
        let curator = Self {
            toolkit,
            blob_root,
            public_root,
            ledger_path,
            max_bytes,
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

/// Lee (tolerante) el spawn `config.json` del dir del nodo para la infra de arranque. Un archivo
/// ausente o ilegible NO aborta el boot: cae a env/defaults (io.blob sin config es valido).
fn load_spawn_doc(node_name: &str) -> Value {
    let Ok(path) = managed_node_config_path(node_name) else {
        return Value::Null;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Value::Null;
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::debug!(
                path = %path.display(),
                error = %err,
                "io.blob spawn config.json is not valid JSON; using env/defaults for infra"
            );
            Value::Null
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn json_get_string(root: &Value, dotted_path: &str) -> Option<String> {
    json_get_path(root, dotted_path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
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
        let blob_root = root.join("blob");
        ensure_dir(&blob_root.join("active"), 0o750).expect("active root");
        BlobCurator::new(
            blob_root.clone(),
            root.join("blob/public"),
            root.join("state/io-blob/publications.json"),
            1024 * 1024,
        )
        .expect("curator")
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
