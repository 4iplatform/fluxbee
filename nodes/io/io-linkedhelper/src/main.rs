#![forbid(unsafe_code)]

mod auth;
mod schema;
mod state_store;

use anyhow::Result;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    compute_thread_id, resolve_identity_option_from_hive_id, try_handle_default_node_status,
    NodeConfig, NodeUuidMode, OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher,
    ThreadIdInput, VaultCallerOwned, VaultClient, FLUXBEE_NODE_NAME_ENV,
};
use io_common::identity::{IdentityError, ResolveOrCreateInput};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
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
use io_common::io_linkedhelper_adapter_config::IoLinkedHelperAdapterConfigContract;
use io_common::io_context::{
    wrap_in_meta_context, ConversationRef, IoContext, MessageRef, PartyRef, ReplyTarget,
};
use io_common::provision::{
    ensure_own_ich, strict_provision_ilk, EnsureOwnIchResult, IdentityProvisionConfig,
};
use io_common::router_message::{build_user_message, new_trace_id, DEFAULT_TTL};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};

use crate::auth::{AdapterAuthValidator, AuthRejection, AuthStatus, InboundAuthRequest};
use crate::schema::{build_configured_schema, build_unconfigured_schema};
use crate::state_store::{
    load_linkedhelper_state, persist_linkedhelper_state, AdapterSnapshot, LinkedHelperDurableState,
    ProfileStateRecord, StoredResponseItem,
};

#[derive(Clone)]
struct Config {
    node_name: String,
    node_version: String,
    listen_addr: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    state_dir: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        Self {
            node_name: env(FLUXBEE_NODE_NAME_ENV)
                .unwrap_or_else(|| "IO.linkedhelper.local".to_string()),
            node_version: env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string()),
            listen_addr: env("LISTEN_ADDR").unwrap_or_else(|| "127.0.0.1:19091".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET").unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            state_dir: PathBuf::from(
                env("STATE_DIR")
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/io-nodes".to_string()),
            ),
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

#[derive(Debug, Clone)]
struct AdapterAuthConfig {
    resource_type: String,
    key: String,
}

#[derive(Debug, Clone)]
struct AdapterRuntime {
    managed_instance_id: String,
    adapter_id: String,
    tenant_id: String,
    local_instance_id: Option<String>,
    auth: AdapterAuthConfig,
    adapter_secret: Option<String>,
    label: Option<String>,
    dst_node: Option<String>,
    mode: String,
    listen_addr: String,
}

#[derive(Debug, Clone, Default)]
struct LinkedHelperRuntimeRegistry {
    binding: Option<AdapterRuntime>,
    max_request_bytes: usize,
    dedup_max_entries: usize,
    identity_target: String,
    identity_timeout_ms: u64,
    /// The node's own managed-instance ICH (`ICH.linkedhelper.<managed_instance_id>`),
    /// set once `ensure_own_ich` succeeds. `None` means the node has not yet
    /// registered its own ICH and must not be treated as operational.
    own_ich_id: Option<String>,
}

#[derive(Clone)]
struct HttpState {
    node_name: String,
    hive_id: String,
    state_dir: PathBuf,
    dispatcher: Arc<RouterDispatcher>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
    /// Serializes the durable-state mutation section of `/v1/poll`. The handler
    /// reads-clones-mutates-writes the durable state, which would race under
    /// concurrent polls (a poll would clobber another's processed_events /
    /// pending_deliveries / profile promotions). One managed instance = one
    /// adapter, so serializing polls here is correct and not a throughput issue.
    poll_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PollRequest {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    adapter_id: Option<String>,
    #[serde(default)]
    managed_instance_id: Option<String>,
    #[serde(default)]
    local_instance_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default, alias = "items")]
    items: Vec<PollItem>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PollItem {
    id: String,
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    payload: Value,
}

#[derive(Debug, Deserialize)]
struct ProfileCreatePayload {
    external_profile_id: String,
    display_name: String,
    #[serde(default)]
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ConversationMessagePayload {
    profile_ilk: String,
    contact_name: String,
    contact_external_composite_id: String,
    #[serde(default)]
    contact_lh_person_id: Option<String>,
    conversation_external_id: String,
    content: Value,
}

/// Operational-enablement values carried in the `/v1/poll` control block. This
/// is a node/managed-instance-level switch (1 LinkedHelper account = 1 node), not
/// a per-profile one — sourced from the node's own managed-instance ICH `enabled`
/// flag (SY.identity), so enable/disable is enforced by Fluxbee, never by a
/// permanent adapter↔Cloud heartbeat.
const OPERATIONAL_ENABLED: &str = "enabled";
const OPERATIONAL_DISABLED: &str = "disabled";

/// Directives the node hands back to the adapter so the adapter — not a
/// permanent Cloud heartbeat — can decide what to do next. See the LinkedHelper
/// replanteo: the adapter contacts Fluxbee Cloud only when the node tells it its
/// situation changed.
mod poll_directive {
    /// Everything is fine; keep operating (send heartbeats/events).
    pub const CONTINUE: &str = "continue";
    /// The managed instance is administratively disabled; stop emitting events
    /// and keep polling status until it flips back to enabled.
    pub const PAUSE: &str = "pause";
    /// The adapter's credentials are no longer valid; re-contact Fluxbee Cloud
    /// to recover administrative credentials (re-enrollment).
    pub const REENROLL: &str = "reenroll";
    /// The adapter's instance→node mapping is stale (this node isn't bound to
    /// it); re-contact Fluxbee Cloud to obtain a fresh runtime destination.
    pub const REPROVISION: &str = "reprovision";
    /// A transient node-side condition; just retry the runtime poll later.
    pub const RETRY: &str = "retry";
}

/// Suggested backoff (seconds) for `retry` directives (transient node states).
const POLL_RETRY_BACKOFF_SECONDS: u64 = 15;
/// Suggested backoff (seconds) before the adapter re-contacts Cloud on a
/// `reprovision`/`reenroll` directive, so a stale mapping can't hammer Cloud.
const POLL_ADMIN_BACKOFF_SECONDS: u64 = 30;

/// Machine-actionable control block attached to EVERY `/v1/poll` response
/// (success and reject) so the adapter always learns its current situation from
/// the runtime plane instead of a permanent Cloud heartbeat.
#[derive(Debug, Clone, Serialize)]
struct PollControl {
    operational_state: &'static str,
    directive: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<u64>,
}

impl PollControl {
    /// Control block for a healthy response: keep operating, unless the managed
    /// instance is administratively disabled (then `pause`).
    fn operating(operational_state: &'static str) -> Self {
        if operational_state == OPERATIONAL_DISABLED {
            Self {
                operational_state,
                directive: poll_directive::PAUSE,
                reason: Some("instance_disabled".to_string()),
                retry_after_seconds: Some(POLL_RETRY_BACKOFF_SECONDS),
            }
        } else {
            Self {
                operational_state,
                directive: poll_directive::CONTINUE,
                reason: None,
                retry_after_seconds: None,
            }
        }
    }

    /// Control block for a rejection, derived from the stable `error_code`.
    fn for_reject(error_code: &str, operational_state: &'static str) -> Self {
        let (directive, retry_after_seconds) = match error_code {
            // The adapter secret no longer matches what the node resolved from
            // Vault (rotated/revoked) → recover credentials from Cloud.
            "invalid_adapter_secret" => (poll_directive::REENROLL, Some(POLL_ADMIN_BACKOFF_SECONDS)),
            // The adapter is targeting a node that isn't bound to it, or its
            // managed/local-instance mapping is stale → re-ask Cloud where this
            // instance must report.
            "adapter_not_allowed"
            | "managed_instance_id_mismatch"
            | "local_instance_id_mismatch" => {
                (poll_directive::REPROVISION, Some(POLL_ADMIN_BACKOFF_SECONDS))
            }
            // Transient node-side conditions → retry the runtime poll later.
            "node_not_ready"
            | "node_binding_unavailable"
            | "auth_secret_unavailable"
            | "durable_state_unavailable" => {
                (poll_directive::RETRY, Some(POLL_RETRY_BACKOFF_SECONDS))
            }
            // Administrative disable surfaced as a reject while sending events.
            "instance_disabled" => (poll_directive::PAUSE, Some(POLL_RETRY_BACKOFF_SECONDS)),
            // Malformed adapter requests: nothing administratively changed, the
            // adapter should fix the request and keep operating.
            _ => (poll_directive::CONTINUE, None),
        };
        Self {
            operational_state,
            directive,
            reason: Some(error_code.to_string()),
            retry_after_seconds,
        }
    }
}

/// Build a `/v1/poll` reject carrying the control block, so every non-2xx
/// response tells the adapter what to do next (retry / reprovision / reenroll /
/// pause) rather than being a mute HTTP status.
fn poll_reject(
    status: StatusCode,
    error_code: &str,
    error_message: impl Into<String>,
    operational_state: &'static str,
) -> Response {
    let control = PollControl::for_reject(error_code, operational_state);
    (
        status,
        Json(serde_json::json!({
            "error_code": error_code,
            "error_message": error_message.into(),
            "control": control,
        })),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct PollResponse {
    ok: bool,
    accepted_at: String,
    response_id: String,
    adapter_id: String,
    actions: Vec<Value>,
    control: PollControl,
    items: Vec<ResponseItem>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ResponseItem {
    #[serde(rename = "ack")]
    Ack {
        response_id: String,
        adapter_id: String,
        event_id: String,
    },
    #[serde(rename = "result")]
    Result {
        response_id: String,
        adapter_id: String,
        event_id: String,
        status: String,
        result_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        retryable: Option<bool>,
    },
    #[serde(rename = "heartbeat")]
    Heartbeat {
        response_id: String,
        adapter_id: String,
        timestamp: String,
    },
}

impl From<StoredResponseItem> for ResponseItem {
    fn from(value: StoredResponseItem) -> Self {
        match value {
            StoredResponseItem::Ack {
                response_id,
                adapter_id,
                event_id,
            } => Self::Ack {
                response_id,
                adapter_id,
                event_id,
            },
            StoredResponseItem::Result {
                response_id,
                adapter_id,
                event_id,
                status,
                result_type,
                payload,
                error_code,
                error_message,
                retryable,
            } => Self::Result {
                response_id,
                adapter_id,
                event_id,
                status,
                result_type,
                payload,
                error_code,
                error_message,
                retryable,
            },
            StoredResponseItem::Heartbeat {
                response_id,
                adapter_id,
                timestamp,
            } => Self::Heartbeat {
                response_id,
                adapter_id,
                timestamp,
            },
        }
    }
}

impl ResponseItem {
    /// A response is retryable when it is an error `Result` the adapter should
    /// re-send. Such outcomes must NOT be recorded in the idempotency ledger,
    /// so the adapter can retry them on the next poll.
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            ResponseItem::Result {
                retryable: Some(true),
                ..
            }
        )
    }

    /// Durable-state projection used to persist a terminal response for
    /// idempotent replay.
    fn to_stored(&self) -> StoredResponseItem {
        match self {
            ResponseItem::Ack {
                response_id,
                adapter_id,
                event_id,
            } => StoredResponseItem::Ack {
                response_id: response_id.clone(),
                adapter_id: adapter_id.clone(),
                event_id: event_id.clone(),
            },
            ResponseItem::Result {
                response_id,
                adapter_id,
                event_id,
                status,
                result_type,
                payload,
                error_code,
                error_message,
                retryable,
            } => StoredResponseItem::Result {
                response_id: response_id.clone(),
                adapter_id: adapter_id.clone(),
                event_id: event_id.clone(),
                status: status.clone(),
                result_type: result_type.clone(),
                payload: payload.clone(),
                error_code: error_code.clone(),
                error_message: error_message.clone(),
                retryable: *retryable,
            },
            ResponseItem::Heartbeat {
                response_id,
                adapter_id,
                timestamp,
            } => StoredResponseItem::Heartbeat {
                response_id: response_id.clone(),
                adapter_id: adapter_id.clone(),
                timestamp: timestamp.clone(),
            },
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::from_env();
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,io_linkedhelper=info,fluxbee_sdk=info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Phase J'-0a: self ILK + tenant from orchestrator-injected env vars.
    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        state_dir = %config.state_dir.display(),
        boot_listen_addr = %config.listen_addr,
        self_ilk_id = ?self_ilk_id,
        self_tenant_id = ?self_tenant_id,
        "io-linkedhelper starting"
    );

    let node_config = NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    };
    let profile = build_io_linkedhelper_rpc_profile()
        .map_err(|err| anyhow::anyhow!("io-linkedhelper rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender_for_log = dispatcher.sender_snapshot();

    tracing::info!(
        full_name = %sender_for_log.full_name(),
        "connected to router"
    );

    let vault_client = match (
        self_ilk_id.as_deref().filter(|value| !value.is_empty()),
        config
            .node_name
            .split('@')
            .nth(1)
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) {
        (Some(self_ilk), Some(hive_id)) => Some(Arc::new(VaultClient::new(
            dispatcher.clone(),
            hive_id.to_string(),
            VaultCallerOwned::new(self_ilk.to_string(), config.node_name.clone()),
        ))),
        _ => None,
    };

    let adapter_contract: Arc<dyn IoAdapterConfigContract> =
        Arc::new(IoLinkedHelperAdapterConfigContract);
    let mut boot_state = bootstrap_io_control_plane_state(&config.state_dir, &config.node_name)
        .unwrap_or_else(|err| {
            tracing::warn!(
                error = %err,
                state_dir = %config.state_dir.display(),
                node_name = %config.node_name,
                "failed to bootstrap IO control-plane state; using UNCONFIGURED"
            );
            IoControlPlaneState::default()
        });

    let boot_registry = load_runtime_registry(boot_state.effective_config.as_ref())
        .unwrap_or_else(|err| {
            if boot_state.effective_config.is_some() {
                boot_state.current_state = IoNodeLifecycleState::FailedConfig;
                boot_state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err.to_string(),
                });
                tracing::warn!(
                    node_name = %config.node_name,
                    error = %err,
                    "boot effective config is invalid for IO.linkedhelper; starting in FAILED_CONFIG"
                );
            }
            LinkedHelperRuntimeRegistry::default()
        });
    let runtime_registry = Arc::new(RwLock::new(boot_registry));
    {
        let mut registry = runtime_registry.write().await;
        if let Some(listen_addr) = registry
            .binding
            .as_ref()
            .map(|binding| binding.listen_addr.trim())
            .filter(|value| !value.is_empty())
        {
            config.listen_addr = listen_addr.to_string();
        }
        if registry.binding.is_some() {
            match resolve_runtime_registry_auth(
                &mut registry,
                vault_client.as_deref(),
                Duration::from_secs(5),
            )
            .await
            {
                Err(err) => {
                    boot_state.current_state = IoNodeLifecycleState::FailedConfig;
                    boot_state.last_error = Some(IoControlPlaneErrorInfo {
                        code: "auth_secret_unavailable".to_string(),
                        message: err.to_string(),
                    });
                    tracing::warn!(
                        node_name = %config.node_name,
                        error = %err,
                        "boot effective config could not resolve linkedhelper adapter secret; starting in FAILED_CONFIG"
                    );
                }
                Ok(()) => {
                    let managed_instance_id = registry
                        .binding
                        .as_ref()
                        .map(|binding| binding.managed_instance_id.clone());
                    if let Some(managed_instance_id) = managed_instance_id {
                        let identity_cfg = build_identity_provision_config(
                            &registry,
                            &hive_id_from_node_name(&config.node_name),
                        );
                        match ensure_linkedhelper_own_ich(
                            &dispatcher,
                            &identity_cfg,
                            self_ilk_id.as_deref(),
                            self_tenant_id.as_deref(),
                            &managed_instance_id,
                        )
                        .await
                        {
                            Ok(result) => {
                                tracing::info!(
                                    node_name = %config.node_name,
                                    managed_instance_id = %managed_instance_id,
                                    self_ilk_id = %result.ilk_id,
                                    own_ich_id = %result.ich_id,
                                    enabled = result.enabled,
                                    "io-linkedhelper own managed-instance ICH ensured at boot"
                                );
                                registry.own_ich_id = Some(result.ich_id);
                            }
                            Err(err) => {
                                boot_state.current_state = IoNodeLifecycleState::FailedConfig;
                                boot_state.last_error = Some(IoControlPlaneErrorInfo {
                                    code: "own_ich_registration_failed".to_string(),
                                    message: err.to_string(),
                                });
                                tracing::warn!(
                                    node_name = %config.node_name,
                                    managed_instance_id = %managed_instance_id,
                                    error = %err,
                                    "failed to ensure own managed-instance ICH at boot; starting in FAILED_CONFIG"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    let runtime_snapshots = {
        let registry = runtime_registry.read().await;
        runtime_registry_snapshots(&registry)
    };
    let durable_state = Arc::new(RwLock::new(
        load_linkedhelper_state(&config.state_dir, &config.node_name)
            .ok()
            .and_then(|loaded| loaded.map(|file| file.state))
            .unwrap_or_default(),
    ));
    {
        let mut state = durable_state.write().await;
        state.sync_adapters(&runtime_snapshots);
        if let Err(err) = persist_linkedhelper_state(&config.state_dir, &config.node_name, &state) {
            tracing::warn!(
                node_name = %config.node_name,
                error = %err,
                "failed to persist linkedhelper durable state during bootstrap"
            );
        }
    }

    let control_plane = Arc::new(RwLock::new(boot_state.clone()));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    ));
    let http_state = Arc::new(HttpState {
        node_name: config.node_name.clone(),
        hive_id: hive_id_from_node_name(&config.node_name),
        state_dir: config.state_dir.clone(),
        dispatcher: dispatcher.clone(),
        control_plane: control_plane.clone(),
        adapter_contract: adapter_contract.clone(),
        runtime_registry: runtime_registry.clone(),
        durable_state: durable_state.clone(),
        poll_lock: Arc::new(Mutex::new(())),
    });

    let http_task = match try_bind_http_listener(
        &config.node_name,
        &config.listen_addr,
        control_plane.clone(),
        control_metrics.clone(),
    )
    .await
    {
        Some(listener) => Some(tokio::spawn(run_http_server(listener, http_state))),
        None => None,
    };

    let control_task = tokio::spawn(run_router_control_loop(
        dispatcher,
        config.node_name.clone(),
        config.state_dir.clone(),
        control_plane,
        control_metrics,
        adapter_contract,
        runtime_registry,
        durable_state,
        vault_client,
        self_ilk_id.clone(),
        self_tenant_id.clone(),
    ));

    if let Some(http_task) = http_task {
        let _ = tokio::join!(http_task, control_task);
    } else {
        let _ = tokio::join!(control_task);
    }

    Ok(())
}

async fn run_http_server(listener: TcpListener, state: Arc<HttpState>) -> Result<()> {
    let app = Router::new()
        .route("/", get(get_schema))
        .route("/schema", get(get_schema))
        .route("/v1/poll", post(post_poll))
        .with_state(state);
    tracing::info!(addr = %listener.local_addr()?, "io-linkedhelper http listener ready");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn try_bind_http_listener(
    node_name: &str,
    listen_addr: &str,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
) -> Option<TcpListener> {
    match TcpListener::bind(listen_addr).await {
        Ok(listener) => Some(listener),
        Err(err) => {
            let err_text = format!("failed to bind HTTP listener {listen_addr}: {err}");
            {
                let mut state = control_plane.write().await;
                state.current_state = IoNodeLifecycleState::FailedConfig;
                state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "listener_bind_failed".to_string(),
                    message: err_text.clone(),
                });
            }
            control_metrics.record_config_set_error(
                IoNodeLifecycleState::FailedConfig.as_str(),
                0,
                "listener_bind_failed",
            );
            tracing::warn!(
                node_name = %node_name,
                listen_addr = %listen_addr,
                error = %err,
                "io-linkedhelper http listener bind failed; node stays alive for control-plane recovery"
            );
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_router_control_loop(
    dispatcher: Arc<RouterDispatcher>,
    node_name: String,
    state_dir: PathBuf,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
    vault_client: Option<Arc<VaultClient>>,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
) -> Result<()> {
    let control_src = dispatcher.sender_snapshot().uuid().to_string();
    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| anyhow::anyhow!("io-linkedhelper system receiver: {err}"))?;
    loop {
        let Some(msg) = system_rx.recv().await else {
            tracing::warn!("io-linkedhelper system channel closed; exiting control loop");
            return Ok(());
        };
        let sender = dispatcher.sender_snapshot();

        if try_handle_default_node_status(&sender, &msg).await? {
            continue;
        }

        if let Some(response) = handle_io_control_plane_message(
            &msg,
            &node_name,
            &control_src,
            &state_dir,
            &dispatcher,
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            runtime_registry.clone(),
            durable_state.clone(),
            vault_client.clone(),
            self_ilk_id.as_deref(),
            self_tenant_id.as_deref(),
        )
        .await
        {
            sender.send(response).await?;
        }
    }
}

const RPC_CH_SYSTEM: &str = "system";

fn build_io_linkedhelper_rpc_profile(
) -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}

#[allow(clippy::too_many_arguments)]
async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    state_dir: &Path,
    dispatcher: &Arc<RouterDispatcher>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
    vault_client: Option<Arc<VaultClient>>,
    self_ilk_id: Option<&str>,
    self_tenant_id: Option<&str>,
) -> Option<WireMessage> {
    let command = msg.meta.msg.as_deref().unwrap_or_default();
    if msg.meta.msg_type != SYSTEM_KIND {
        return None;
    }

    if command.eq_ignore_ascii_case("PING") {
        let state = control_plane.read().await.clone();
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
        });
        return Some(build_system_reply(msg, control_src, "PONG", payload));
    }

    if command.eq_ignore_ascii_case("STATUS") {
        let state = control_plane.read().await.clone();
        let metrics = control_metrics.snapshot();
        let (adapter_count, own_ich_id) = {
            let registry = runtime_registry.read().await;
            (usize::from(registry.binding.is_some()), registry.own_ich_id.clone())
        };
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
            "config_source": state.config_source.as_str(),
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": state.last_error,
            "metrics": { "control_plane": metrics },
            "runtime": {
                "active_adapter_count": adapter_count,
                "own_ich_id": own_ich_id
            }
        });
        return Some(build_system_reply(msg, control_src, "STATUS_RESPONSE", payload));
    }

    if !command.eq_ignore_ascii_case("CONFIG_GET") && !command.eq_ignore_ascii_case("CONFIG_SET") {
        return None;
    }

    let payload = match parse_and_validate_io_control_plane_request(msg, node_name) {
        Ok(IoControlPlaneRequest::Get(_)) => {
            let state = control_plane.read().await.clone();
            let redacted = redact_state(&state, adapter_contract);
            log_config_get_served(msg.routing.trace_id.as_str(), node_name, &redacted);
            let mut payload = build_io_config_get_response_payload(
                node_name,
                &redacted,
                build_io_adapter_contract_payload(adapter_contract, state.effective_config.as_ref()),
            );
            if let Some(obj) = payload.as_object_mut() {
                let (adapter_count, own_ich_id) = {
                    let registry = runtime_registry.read().await;
                    (usize::from(registry.binding.is_some()), registry.own_ich_id.clone())
                };
                obj.insert(
                    "metrics".to_string(),
                    serde_json::json!({
                        "control_plane": control_metrics.snapshot(),
                        "runtime": {
                            "active_adapter_count": adapter_count,
                            "own_ich_id": own_ich_id
                        }
                    }),
                );
            }
            payload
        }
        Ok(IoControlPlaneRequest::Set(set_payload)) => {
            apply_linkedhelper_config_set(
                &set_payload,
                node_name,
                state_dir,
                dispatcher,
                control_plane.clone(),
                control_metrics,
                adapter_contract,
                runtime_registry,
                durable_state,
                vault_client,
                self_ilk_id,
                self_tenant_id,
            )
            .await
        }
        Err(err) => {
            let state = control_plane.read().await.clone();
            let redacted = redact_state(&state, adapter_contract);
            let err_text = err.to_string();
            log_control_plane_request_rejected(
                msg.routing.trace_id.as_str(),
                node_name,
                err.code(),
                err_text.as_str(),
            );
            build_io_config_set_error_payload(node_name, &redacted, err.code(), err_text)
        }
    };

    let mut response = build_io_config_response_message(msg, payload);
    response.routing.src = control_src.to_string();
    Some(response)
}

#[allow(clippy::too_many_arguments)]
async fn apply_linkedhelper_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    state_dir: &Path,
    dispatcher: &Arc<RouterDispatcher>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
    vault_client: Option<Arc<VaultClient>>,
    self_ilk_id: Option<&str>,
    self_tenant_id: Option<&str>,
) -> Value {
    let mut state = control_plane.write().await;

    if let Err(err) = ensure_config_version_advances(payload.config_version, state.config_version) {
        log_config_set_stale_rejected(node_name, payload.config_version, state.config_version);
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            state.config_version,
            err.code(),
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            err.code(),
            err.to_string(),
        );
    }

    let candidate = match apply_adapter_config_replace(adapter_contract, &payload.config) {
        Ok(cfg) => cfg,
        Err(err) => {
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: err.code().to_string(),
                message: err.to_string(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                state.config_version,
                err.code(),
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                err.code(),
                err.to_string(),
            );
        }
    };

    let registry = match load_runtime_registry(Some(&candidate)) {
        Ok(registry) => registry,
        Err(err) => {
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: err.to_string(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                state.config_version,
                "invalid_config",
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                "invalid_config",
                err.to_string(),
            );
        }
    };
    let previous_listen_addr = state
        .effective_config
        .as_ref()
        .and_then(|cfg| extract_runtime_listen_addr(cfg).ok());
    let next_listen_addr = extract_runtime_listen_addr(&candidate).ok();
    let mut registry = registry;
    if let Err(err) = resolve_runtime_registry_auth(
        &mut registry,
        vault_client.as_deref(),
        Duration::from_secs(5),
    )
    .await
    {
        state.current_state = IoNodeLifecycleState::FailedConfig;
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: "auth_secret_unavailable".to_string(),
            message: err.to_string(),
        });
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            state.config_version,
            "auth_secret_unavailable",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            "auth_secret_unavailable",
            err.to_string(),
        );
    }

    // Ensure the node owns its managed-instance ICH before going operational
    // (§3.8 / acceptance criteria: no own ICH => not operational).
    if let Some(managed_instance_id) = registry
        .binding
        .as_ref()
        .map(|binding| binding.managed_instance_id.clone())
    {
        let identity_cfg =
            build_identity_provision_config(&registry, &hive_id_from_node_name(node_name));
        match ensure_linkedhelper_own_ich(
            dispatcher,
            &identity_cfg,
            self_ilk_id,
            self_tenant_id,
            &managed_instance_id,
        )
        .await
        {
            Ok(result) => {
                tracing::info!(
                    node_name = %node_name,
                    managed_instance_id = %managed_instance_id,
                    self_ilk_id = %result.ilk_id,
                    own_ich_id = %result.ich_id,
                    enabled = result.enabled,
                    "io-linkedhelper own managed-instance ICH ensured on CONFIG_SET"
                );
                registry.own_ich_id = Some(result.ich_id);
            }
            Err(err) => {
                state.current_state = IoNodeLifecycleState::FailedConfig;
                state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "own_ich_registration_failed".to_string(),
                    message: err.to_string(),
                });
                control_metrics.record_config_set_error(
                    state.current_state.as_str(),
                    state.config_version,
                    "own_ich_registration_failed",
                );
                let redacted = redact_state(&state, adapter_contract);
                return build_io_config_set_error_payload(
                    node_name,
                    &redacted,
                    "own_ich_registration_failed",
                    err.to_string(),
                );
            }
        }
    }

    let previous_version = state.config_version;
    state.current_state = IoNodeLifecycleState::Configured;
    state.config_source = IoConfigSource::Dynamic;
    state.schema_version = payload.schema_version;
    state.config_version = payload.config_version;
    state.effective_config = Some(candidate);
    state.last_error = None;

    if let Err(err) = persist_io_control_plane_state(state_dir, node_name, &state) {
        let code = "persist_failed";
        let message = err.to_string();
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: code.to_string(),
            message: message.clone(),
        });
        log_config_set_persist_error(
            node_name,
            payload.schema_version,
            payload.config_version,
            &message,
        );
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            previous_version,
            code,
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(node_name, &redacted, code, message);
    }

    let snapshots = runtime_registry_snapshots(&registry);
    *runtime_registry.write().await = registry;
    {
        let current = durable_state.read().await.clone();
        let mut updated = current.clone();
        updated.sync_adapters(&snapshots);
        if let Err(err) = persist_linkedhelper_state(state_dir, node_name, &updated) {
            let code = "linkedhelper_state_persist_failed";
            let message = err.to_string();
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: code.to_string(),
                message: message.clone(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                previous_version,
                code,
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(node_name, &redacted, code, message);
        }
        *durable_state.write().await = updated;
    }
    let apply_hot = vec![
        "runtime_registry".to_string(),
        "vault_auth_cache".to_string(),
    ];
    let apply_reinit: Vec<String> = Vec::new();
    let apply_restart_required = if previous_listen_addr != next_listen_addr {
        vec!["http_listener".to_string()]
    } else {
        Vec::new()
    };
    log_config_set_applied(
        node_name,
        payload.schema_version,
        payload.config_version,
        &apply_hot,
        &apply_reinit,
        &apply_restart_required,
    );
    control_metrics.record_config_set_ok(state.current_state.as_str(), state.config_version);
    let redacted = redact_state(&state, adapter_contract);
    let mut response = build_io_config_set_ok_payload(node_name, &redacted);
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "apply".to_string(),
            serde_json::json!({
                "mode": "hot_reload",
                "hot_applied": apply_hot,
                "reinit_performed": apply_reinit,
                "restart_required": apply_restart_required
            }),
        );
    }
    response
}

fn redact_state(
    state: &IoControlPlaneState,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> IoControlPlaneState {
    let mut redacted = state.clone();
    redacted.effective_config = redacted
        .effective_config
        .as_ref()
        .map(|effective| adapter_contract.redact_effective_config(effective));
    redacted
}

fn load_runtime_registry(effective_config: Option<&Value>) -> Result<LinkedHelperRuntimeRegistry> {
    let max_request_bytes = effective_config
        .and_then(|cfg| cfg.get("http"))
        .and_then(|http| http.get("max_request_bytes"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(256 * 1024);
    let dedup_max_entries = effective_config
        .and_then(|cfg| cfg.get("http"))
        .and_then(|http| http.get("dedup_max_entries"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(10_000);
    let identity_target = effective_config
        .and_then(|cfg| cfg.get("identity"))
        .and_then(|identity| identity.get("target"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
        .to_string();
    let identity_timeout_ms = effective_config
        .and_then(|cfg| cfg.get("identity"))
        .and_then(|identity| identity.get("timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(10_000);

    let binding = if let Some(cfg) = effective_config {
        let managed_instance_id = cfg
            .get("managed_instance_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("managed_instance_id is required"))?
            .to_string();
        let tenant_id = cfg
            .get("tenant_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("tenant_id is required"))?
            .to_string();
        let adapter = cfg
            .get("adapter")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("config.adapter must be an object"))?;
        let adapter_id = adapter
            .get("adapter_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("adapter.adapter_id is required"))?
            .to_string();
        let local_instance_id = adapter
            .get("local_instance_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let auth = adapter
            .get("auth")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("adapter.auth must be an object"))?;
        let auth_type = auth
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("adapter.auth.type is required"))?;
        if auth_type != "vault_ref" {
            return Err(anyhow::anyhow!("adapter.auth.type must be 'vault_ref'"));
        }
        let resource_type = auth
            .get("resource_type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("adapter.auth.resource_type is required"))?
            .to_string();
        let key = auth
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("adapter.auth.key is required"))?
            .to_string();
        let label = adapter
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let dst_node = adapter
            .get("dst_node")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let mode = cfg
            .get("mode")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("direct_http_intermediate")
            .to_string();
        let listen_addr = extract_runtime_listen_addr(cfg)?;
        Some(AdapterRuntime {
            managed_instance_id,
            adapter_id,
            tenant_id,
            local_instance_id,
            auth: AdapterAuthConfig { resource_type, key },
            adapter_secret: None,
            label,
            dst_node,
            mode,
            listen_addr,
        })
    } else {
        None
    };

    Ok(LinkedHelperRuntimeRegistry {
        binding,
        max_request_bytes,
        dedup_max_entries,
        identity_target,
        identity_timeout_ms,
        own_ich_id: None,
    })
}

fn runtime_registry_snapshots(registry: &LinkedHelperRuntimeRegistry) -> Vec<AdapterSnapshot> {
    registry
        .binding
        .iter()
        .map(|adapter| AdapterSnapshot {
            adapter_id: adapter.adapter_id.clone(),
            tenant_id: adapter.tenant_id.clone(),
            label: adapter.label.clone(),
            dst_node: adapter.dst_node.clone(),
            auth_key_ref: adapter.auth.key.clone(),
        })
        .collect()
}

async fn resolve_runtime_registry_auth(
    registry: &mut LinkedHelperRuntimeRegistry,
    vault_client: Option<&VaultClient>,
    timeout: Duration,
) -> Result<()> {
    let Some(binding) = registry.binding.as_mut() else {
        return Ok(());
    };
    let Some(vault_client) = vault_client else {
        return Err(anyhow::anyhow!(
            "vault client unavailable for linkedhelper adapter secret resolution"
        ));
    };
    let resource_type = binding.auth.resource_type.trim();
    if resource_type != "linkedhelper_adapter" {
        return Err(anyhow::anyhow!(
            "unsupported adapter auth resource_type '{}'",
            binding.auth.resource_type
        ));
    }
    let response = vault_client
        .get(binding.auth.key.as_str(), timeout)
        .await
        .map_err(|err| anyhow::anyhow!("vault get {} failed: {err}", binding.auth.key))?;
    let secret_value = response
        .value
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("vault key {} returned no value", binding.auth.key))?;
    let secret = extract_adapter_secret_from_vault_value(secret_value)?;
    if secret.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "vault key {} returned an empty adapter secret",
            binding.auth.key
        ));
    }
    binding.adapter_secret = Some(secret);
    Ok(())
}

fn extract_adapter_secret_from_vault_value(value: &Value) -> Result<String> {
    if let Some(secret) = value.as_str().map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(secret.to_string());
    }
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("vault secret value must be a string or object"))?;
    for field in ["adapter_secret", "secret", "token", "bearer_token"] {
        if let Some(secret) = obj
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(secret.to_string());
        }
    }
    Err(anyhow::anyhow!(
        "vault secret value must expose adapter_secret, secret, token, or bearer_token"
    ))
}

fn extract_runtime_listen_addr(effective_config: &Value) -> Result<String> {
    let listen = effective_config
        .get("listen")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("listen is required"))?;
    let address = listen
        .get("address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("listen.address is required"))?;
    let port = listen
        .get("port")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("listen.port is required"))?;
    Ok(format!("{address}:{port}"))
}

fn hive_id_from_node_name(node_name: &str) -> String {
    node_name
        .rsplit_once('@')
        .map(|(_, hive_id)| hive_id.trim())
        .filter(|value| !value.is_empty())
        .unwrap_or("motherbee")
        .to_string()
}

fn linkedhelper_profile_channel() -> &'static str {
    "linkedhelper"
}

fn linkedhelper_contact_channel() -> &'static str {
    "linkedhelper_contact"
}

/// Channel type used to register the node's OWN ICH
/// (`ICH.linkedhelper.<managed_instance_id>`). Kept distinct from the profile
/// channel so a managed instance can never collide with an external profile id.
fn linkedhelper_managed_instance_channel() -> &'static str {
    "linkedhelper_managed_instance"
}

/// Canonical LinkedHelper conversation thread id. Keyed on the managed instance
/// (stable across adapter re-enroll/migrate/rebind) + the external conversation
/// id — NOT the adapter binding — so cognition continuity survives binding
/// changes. Consistent with the envelope entrypoint (managed_instance).
fn linkedhelper_thread_id(
    managed_instance_id: &str,
    conversation_external_id: &str,
) -> Result<String, String> {
    compute_thread_id(ThreadIdInput::PersistentChannel {
        channel_type: linkedhelper_profile_channel(),
        entrypoint_id: Some(managed_instance_id),
        conversation_id: conversation_external_id,
    })
    .map_err(|err| err.to_string())
}

/// Register (idempotently) the node's own managed-instance ICH with SY.identity.
/// Per the intermediate spec (§3.8, acceptance criteria) the node must own
/// `ICH.linkedhelper.<managed_instance_id>` and must not be operational without
/// it — callers treat a failure here as `own_ich_registration_failed`.
async fn ensure_linkedhelper_own_ich(
    dispatcher: &Arc<RouterDispatcher>,
    identity_cfg: &IdentityProvisionConfig,
    self_ilk_id: Option<&str>,
    self_tenant_id: Option<&str>,
    managed_instance_id: &str,
) -> Result<EnsureOwnIchResult> {
    let self_ilk_id = self_ilk_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("missing self_ilk_id for IO.linkedhelper own ICH registration")
        })?;
    let self_tenant_id = self_tenant_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("missing self_tenant_id for IO.linkedhelper own ICH registration")
        })?;
    let result = ensure_own_ich(
        dispatcher,
        identity_cfg,
        identity_cfg.target.as_str(),
        self_ilk_id,
        self_tenant_id,
        linkedhelper_managed_instance_channel(),
        managed_instance_id,
    )
    .await
    .map_err(|err| anyhow::anyhow!("own ICH registration failed: {err}"))?;
    Ok(result)
}

fn normalize_registration_status(status: &str) -> &'static str {
    match status.trim() {
        "complete" => "complete",
        "partial" => "partial",
        _ => "temporary",
    }
}

fn profile_is_usable(status: &str) -> bool {
    normalize_registration_status(status).eq("complete")
}

fn default_identity_target_for_hive(hive_id: &str) -> String {
    format!("SY.identity@{}", hive_id.trim())
}

fn build_identity_provision_config(
    registry: &LinkedHelperRuntimeRegistry,
    hive_id: &str,
) -> IdentityProvisionConfig {
    IdentityProvisionConfig {
        target: if registry.identity_target.trim().is_empty() {
            default_identity_target_for_hive(hive_id)
        } else {
            registry.identity_target.clone()
        },
        timeout: Duration::from_millis(registry.identity_timeout_ms.max(1)),
    }
}

async fn get_schema(State(state): State<Arc<HttpState>>) -> Response {
    let snapshot = state.control_plane.read().await.clone();
    let redacted = redact_state(&snapshot, state.adapter_contract.as_ref());
    let adapter_count = usize::from(state.runtime_registry.read().await.binding.is_some());

    let body = if let Some(effective) = redacted.effective_config.as_ref() {
        build_configured_schema(
            &state.node_name,
            &snapshot,
            effective,
            state.adapter_contract.as_ref(),
            adapter_count,
        )
    } else {
        build_unconfigured_schema(&state.node_name, &snapshot, state.adapter_contract.as_ref())
    };

    Json(body).into_response()
}

async fn post_poll(
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
    Json(request): Json<PollRequest>,
) -> Response {
    let control_snapshot = state.control_plane.read().await.clone();
    if control_snapshot.current_state != IoNodeLifecycleState::Configured {
        return poll_reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_not_ready",
            control_snapshot
                .last_error
                .as_ref()
                .map(|err| err.message.clone())
                .unwrap_or_else(|| "linkedhelper node is not configured".to_string()),
            OPERATIONAL_ENABLED,
        );
    }

    let registry = state.runtime_registry.read().await;
    let Some(runtime) = registry.binding.as_ref() else {
        return poll_reject(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_binding_unavailable",
            "linkedhelper node has no active managed instance binding",
            OPERATIONAL_ENABLED,
        );
    };
    // Node/managed-instance-level operational state (1 LinkedHelper account = 1
    // node), surfaced to the adapter in every control block below so enable/
    // disable is enforced here, not via a permanent Cloud heartbeat.
    //
    // The administrative enable/disable *toggle* is deferred (product decision),
    // so the node reports `enabled` by default today; the disable path (`pause`
    // directive + `instance_disabled` reject) is wired and unit-tested, ready for
    // when the toggle's source is added (Cloud-administered config / identity).
    // NOTE: do NOT source this from the own-ICH `enabled` flag — a freshly
    // registered ICH is `enabled=false`, which would wrongly disable every new
    // node and block normal event flow.
    let operational_state = OPERATIONAL_ENABLED;

    let request_size = serde_json::to_vec(&request).map(|bytes| bytes.len()).unwrap_or(0);
    if request_size > registry.max_request_bytes {
        return poll_reject(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            format!(
                "request body exceeds configured max_request_bytes ({})",
                registry.max_request_bytes
            ),
            operational_state,
        );
    }

    // Adapter authentication/authorization is delegated to the isolated
    // `AdapterAuthValidator` (auth contract §6/§10) so it can later move to the
    // Edge without touching the poll handler.
    let validator = AdapterAuthValidator::new(
        runtime.adapter_id.clone(),
        runtime.managed_instance_id.clone(),
        runtime.local_instance_id.clone(),
        runtime.adapter_secret.clone(),
    );
    let auth_request = InboundAuthRequest {
        header_adapter_id: headers
            .get("X-Fluxbee-Adapter-Id")
            .and_then(|value| value.to_str().ok()),
        bearer: extract_bearer_token(&headers),
        body_adapter_id: request.adapter_id.as_deref(),
        body_managed_instance_id: request.managed_instance_id.as_deref(),
        body_local_instance_id: request.local_instance_id.as_deref(),
    };
    if let Err(rejection) = validator.validate(&auth_request) {
        return auth_rejection_response(rejection, operational_state);
    }

    let runtime = runtime.clone();
    let identity_cfg = build_identity_provision_config(&registry, &state.hive_id);
    let dedup_max_entries = registry.dedup_max_entries;
    drop(registry);

    let request_id = request
        .request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("poll");
    let response_id = format!("resp:{request_id}");
    let mode = request
        .mode
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| if request.items.is_empty() { "heartbeat" } else { "events" });
    // Serialize the read-clone-mutate-writeback of durable state so concurrent
    // polls can't clobber each other's changes (processed_events, deliveries,
    // profile/automation state). Held until the handler returns.
    let _poll_guard = state.poll_lock.lock().await;
    let mut durable_state = state.durable_state.read().await.clone();
    durable_state.mark_adapter_poll(&runtime.adapter_id, request_id);
    refresh_pending_profiles_for_adapter(state.as_ref(), &runtime, &mut durable_state);
    let pending_deliveries = durable_state
        .drain_pending_deliveries(&runtime.adapter_id)
        .into_iter()
        .map(ResponseItem::from)
        .collect::<Vec<ResponseItem>>();

    let mut items = if mode.eq_ignore_ascii_case("heartbeat") {
        if !request.items.is_empty() {
            return poll_reject(
                StatusCode::BAD_REQUEST,
                "invalid_heartbeat_request",
                "heartbeat requests must not include items",
                operational_state,
            );
        }
        // Heartbeat/status polls always succeed (200) and carry the operational
        // state in the control block, so a disabled instance can keep polling
        // status until it is re-enabled.
        pending_deliveries
    } else if mode.eq_ignore_ascii_case("events") {
        // An administratively disabled managed instance must not emit events:
        // reject with a `pause` directive so the adapter drops to status-polling
        // until it flips back to enabled (enforcement lives here, not in Cloud).
        if operational_state == OPERATIONAL_DISABLED {
            return poll_reject(
                StatusCode::CONFLICT,
                "instance_disabled",
                "managed instance is administratively disabled; events are not accepted",
                operational_state,
            );
        }
        if request.items.is_empty() {
            return poll_reject(
                StatusCode::BAD_REQUEST,
                "empty_events_batch",
                "events requests must include at least one item",
                operational_state,
            );
        }
        let mut responses = Vec::with_capacity(request.items.len());
        for item in &request.items {
            let event_id = item.id.trim();
            // Events with no stable id/type get a validation error but are NOT
            // recorded for idempotent replay (there is no reliable dedup key).
            if event_id.is_empty() || item.item_type.trim().is_empty() {
                responses.push(ResponseItem::Result {
                    response_id: format!("resp:{request_id}:invalid"),
                    adapter_id: runtime.adapter_id.clone(),
                    event_id: item.id.clone(),
                    status: "error".to_string(),
                    result_type: "validation_error".to_string(),
                    payload: None,
                    error_code: Some("invalid_event".to_string()),
                    error_message: Some("event id and type are required".to_string()),
                    retryable: Some(false),
                });
                continue;
            }

            // Idempotency: if this event already reached a terminal outcome,
            // replay the recorded response(s) instead of re-running side effects.
            let dedup_key = format!(
                "{}:{}:{}",
                runtime.managed_instance_id, runtime.adapter_id, event_id
            );
            if let Some(record) = durable_state.processed_event(&dedup_key) {
                tracing::debug!(
                    node_name = %state.node_name,
                    adapter_id = %runtime.adapter_id,
                    event_id = %event_id,
                    "linkedhelper replayed idempotent response for duplicate event"
                );
                for stored in record.responses.clone() {
                    responses.push(ResponseItem::from(stored));
                }
                continue;
            }

            let mut item_responses: Vec<ResponseItem> = Vec::new();
            if item.item_type.eq_ignore_ascii_case("profile_create") {
                process_profile_create(
                    &state,
                    &runtime,
                    &identity_cfg,
                    &mut durable_state,
                    request_id,
                    item,
                    &mut item_responses,
                )
                .await;
            } else if item.item_type.eq_ignore_ascii_case("conversation_message") {
                process_conversation_message(
                    &state,
                    &runtime,
                    &identity_cfg,
                    &mut durable_state,
                    request_id,
                    item,
                    &mut item_responses,
                )
                .await;
            } else {
                item_responses.push(ResponseItem::Result {
                    response_id: format!("resp:{request_id}:{event_id}:unsupported"),
                    adapter_id: runtime.adapter_id.clone(),
                    event_id: item.id.clone(),
                    status: "error".to_string(),
                    result_type: "unsupported_event_type".to_string(),
                    payload: None,
                    error_code: Some("unsupported_event_type".to_string()),
                    error_message: Some(format!(
                        "event type '{}' is not implemented yet",
                        item.item_type
                    )),
                    retryable: Some(false),
                });
            }

            // Record only terminal outcomes; a retryable failure must be left
            // un-recorded so the adapter can retry it on a later poll.
            let retryable = item_responses.iter().any(ResponseItem::is_retryable);
            if !retryable && !item_responses.is_empty() {
                let stored: Vec<StoredResponseItem> =
                    item_responses.iter().map(ResponseItem::to_stored).collect();
                durable_state.record_processed_event(dedup_key, stored, dedup_max_entries);
            }
            responses.append(&mut item_responses);
        }
        responses.extend(pending_deliveries);
        responses
    } else {
        return poll_reject(
            StatusCode::BAD_REQUEST,
            "invalid_mode",
            "mode must be 'events' or 'heartbeat'",
            operational_state,
        );
    };
    if items.is_empty() {
        items.push(ResponseItem::Heartbeat {
            response_id,
            adapter_id: runtime.adapter_id.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        });
    }

    if let Err(err) = persist_linkedhelper_state(&state.state_dir, &state.node_name, &durable_state)
    {
        tracing::warn!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            request_id = %request_id,
            error = %err,
            "failed to persist linkedhelper durable state while processing poll"
        );
        return poll_reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            "durable_state_unavailable",
            "linkedhelper durable state is temporarily unavailable",
            operational_state,
        );
    }
    *state.durable_state.write().await = durable_state;

    tracing::debug!(
        adapter_id = %runtime.adapter_id,
        managed_instance_id = %runtime.managed_instance_id,
        adapter_label = %runtime.label.as_deref().unwrap_or(""),
        tenant_id = %runtime.tenant_id,
        local_instance_id = %runtime.local_instance_id.as_deref().unwrap_or(""),
        dst_node = %runtime.dst_node.as_deref().unwrap_or(""),
        mode = %runtime.mode,
        request_id = %request_id,
        poll_mode = %mode,
        item_count = request.items.len(),
        "io-linkedhelper poll processed"
    );

    Json(PollResponse {
        ok: true,
        accepted_at: chrono::Utc::now().to_rfc3339(),
        response_id: format!("resp:{request_id}"),
        adapter_id: runtime.adapter_id,
        actions: Vec::new(),
        control: PollControl::operating(operational_state),
        items,
    })
    .into_response()
}

fn auth_rejection_response(rejection: AuthRejection, operational_state: &'static str) -> Response {
    let status = match rejection.status {
        AuthStatus::BadRequest => StatusCode::BAD_REQUEST,
        AuthStatus::Unauthorized => StatusCode::UNAUTHORIZED,
        AuthStatus::Forbidden => StatusCode::FORBIDDEN,
        AuthStatus::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    // Carry the same control block as any other reject so the adapter can map an
    // auth failure to an administrative action (reenroll / reprovision).
    poll_reject(
        status,
        &rejection.error_code,
        rejection.error_message,
        operational_state,
    )
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let token = header.strip_prefix("Bearer ")?;
    let trimmed = token.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn refresh_pending_profiles_for_adapter(
    state: &HttpState,
    runtime: &AdapterRuntime,
    durable_state: &mut LinkedHelperDurableState,
) {
    let external_profile_ids: Vec<String> = durable_state
        .profiles
        .values()
        .filter(|profile| profile.adapter_id == runtime.adapter_id)
        .map(|profile| profile.external_profile_id.clone())
        .collect();

    for external_profile_id in external_profile_ids {
        let Some(existing) = durable_state.profiles.get(&external_profile_id).cloned() else {
            continue;
        };
        match resolve_identity_option_from_hive_id(
            &state.hive_id,
            linkedhelper_profile_channel(),
            &existing.external_profile_id,
            &existing.tenant_id,
        ) {
            Ok(Some(resolved))
                if normalize_registration_status(&resolved.ilk.registration_status).eq("complete") =>
            {
                let was_complete = profile_is_usable(&existing.status);
                durable_state.upsert_profile(
                    &existing.adapter_id,
                    &existing.tenant_id,
                    &existing.external_profile_id,
                    Some(resolved.ilk.ilk_id.clone()),
                    Some(resolved.ich_id.clone()),
                    "complete",
                    existing.display_name.clone(),
                    existing.metadata.clone(),
                );
                if !was_complete {
                    tracing::info!(
                        node_name = %state.node_name,
                        adapter_id = %existing.adapter_id,
                        external_profile_id = %existing.external_profile_id,
                        ilk_id = %resolved.ilk.ilk_id,
                        ich_id = %resolved.ich_id,
                        "linkedhelper profile promoted to complete/usable"
                    );
                    durable_state.enqueue_pending_delivery(
                        &existing.adapter_id,
                        format!("profile_ready:{}", existing.external_profile_id),
                        Some(format!("profile_ready:{}", existing.external_profile_id)),
                        StoredResponseItem::Result {
                            response_id: format!(
                                "resp:profile_ready:{}",
                                existing.external_profile_id
                            ),
                            adapter_id: existing.adapter_id.clone(),
                            event_id: existing.external_profile_id.clone(),
                            status: "success".to_string(),
                            result_type: "profile_ready".to_string(),
                            payload: Some(serde_json::json!({
                                "external_profile_id": existing.external_profile_id,
                                "ilk_id": resolved.ilk.ilk_id,
                                "ich_id": resolved.ich_id
                            })),
                            error_code: None,
                            error_message: None,
                            retryable: None,
                        },
                    );
                }
                refresh_ich_state_for_profile(
                    durable_state,
                    &existing,
                    &resolved.ich_id,
                    &resolved.ilk.ilk_id,
                    resolved.owner_l2_name.as_deref(),
                    resolved.enabled,
                );
            }
            Ok(Some(resolved)) => {
                let ilk_id = resolved.ilk.ilk_id.clone();
                let ich_id = resolved.ich_id.clone();
                let next_status = normalize_registration_status(&resolved.ilk.registration_status);
                durable_state.upsert_profile(
                    &existing.adapter_id,
                    &existing.tenant_id,
                    &existing.external_profile_id,
                    Some(ilk_id.clone()),
                    Some(ich_id.clone()),
                    next_status,
                    existing.display_name.clone(),
                    existing.metadata.clone(),
                );
                refresh_ich_state_for_profile(
                    durable_state,
                    &existing,
                    &ich_id,
                    &ilk_id,
                    resolved.owner_l2_name.as_deref(),
                    resolved.enabled,
                );
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(
                    node_name = %state.node_name,
                    adapter_id = %runtime.adapter_id,
                    external_profile_id = %existing.external_profile_id,
                    error = %err,
                    "linkedhelper failed to refresh pending profile from identity SHM"
                );
            }
        }
    }
}

fn refresh_ich_state_for_profile(
    durable_state: &mut LinkedHelperDurableState,
    profile: &ProfileStateRecord,
    ich_id: &str,
    ilk_id: &str,
    owner_l2_name: Option<&str>,
    enabled: bool,
) {
    let previous = durable_state.upsert_ich_state(
        ich_id,
        &profile.adapter_id,
        &profile.external_profile_id,
        Some(ilk_id.to_string()),
        owner_l2_name.map(ToString::to_string),
        enabled,
    );
    if previous == Some(enabled) {
        return;
    }
    let result_type = if enabled {
        "automation_enabled"
    } else {
        "automation_disabled"
    };
    tracing::info!(
        adapter_id = %profile.adapter_id,
        external_profile_id = %profile.external_profile_id,
        ilk_id = %ilk_id,
        ich_id = %ich_id,
        enabled,
        result_type = %result_type,
        "linkedhelper observed automation state change for own ICH"
    );
    durable_state.enqueue_pending_delivery(
        &profile.adapter_id,
        format!("{result_type}:{ich_id}"),
        Some(format!("ich_state:{ich_id}")),
        StoredResponseItem::Result {
            response_id: format!("resp:{result_type}:{ich_id}"),
            adapter_id: profile.adapter_id.clone(),
            event_id: profile.external_profile_id.clone(),
            status: "success".to_string(),
            result_type: result_type.to_string(),
            payload: Some(serde_json::json!({
                "external_profile_id": profile.external_profile_id,
                "ilk_id": ilk_id,
                "ich_id": ich_id
            })),
            error_code: None,
            error_message: None,
            retryable: None,
        },
    );
}

async fn process_profile_create(
    state: &Arc<HttpState>,
    runtime: &AdapterRuntime,
    identity_cfg: &IdentityProvisionConfig,
    durable_state: &mut LinkedHelperDurableState,
    request_id: &str,
    item: &PollItem,
    responses: &mut Vec<ResponseItem>,
) {
    let event_id = item.id.trim();
    let payload = match serde_json::from_value::<ProfileCreatePayload>(item.payload.clone()) {
        Ok(payload) => payload,
        Err(err) => {
            responses.push(ResponseItem::Result {
                response_id: format!("resp:{request_id}:{event_id}:invalid"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
                status: "error".to_string(),
                result_type: "validation_error".to_string(),
                payload: None,
                error_code: Some("invalid_profile_create_payload".to_string()),
                error_message: Some(err.to_string()),
                retryable: Some(false),
            });
            return;
        }
    };

    let external_profile_id = payload.external_profile_id.trim();
    let display_name = payload.display_name.trim();
    if external_profile_id.is_empty() || display_name.is_empty() {
        responses.push(ResponseItem::Result {
            response_id: format!("resp:{request_id}:{event_id}:invalid"),
            adapter_id: runtime.adapter_id.clone(),
            event_id: item.id.clone(),
            status: "error".to_string(),
            result_type: "validation_error".to_string(),
            payload: None,
            error_code: Some("invalid_profile_create_payload".to_string()),
            error_message: Some(
                "profile_create requires non-empty external_profile_id and display_name"
                    .to_string(),
            ),
            retryable: Some(false),
        });
        return;
    }

    let identity_input = ResolveOrCreateInput {
        channel: linkedhelper_profile_channel().to_string(),
        external_id: external_profile_id.to_string(),
        src_ilk_override: None,
        tenant_id: Some(runtime.tenant_id.clone()),
        tenant_hint: None,
        attributes: serde_json::json!({
            "display_name": display_name,
            "metadata": payload.metadata.clone(),
            "source": "io.linkedhelper"
        }),
        ilk_type: Some("agent".to_string()),
    };

    match resolve_identity_option_from_hive_id(
        &state.hive_id,
        &identity_input.channel,
        &identity_input.external_id,
        identity_input.tenant_id.as_deref().unwrap_or(""),
    ) {
        Ok(Some(resolved)) => {
            durable_state.upsert_profile(
                &runtime.adapter_id,
                &runtime.tenant_id,
                external_profile_id,
                Some(resolved.ilk.ilk_id.clone()),
                Some(resolved.ich_id.clone()),
                normalize_registration_status(&resolved.ilk.registration_status),
                Some(display_name.to_string()),
                payload.metadata.clone(),
            );
            tracing::info!(
                node_name = %state.node_name,
                adapter_id = %runtime.adapter_id,
                event_id = %item.id,
                external_profile_id = %external_profile_id,
                ilk_id = %resolved.ilk.ilk_id,
                ich_id = %resolved.ich_id,
                registration_status = %resolved.ilk.registration_status,
                "linkedhelper accepted profile_create for known profile"
            );
            responses.push(ResponseItem::Ack {
                response_id: format!("resp:{request_id}:{event_id}"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
            });
            if normalize_registration_status(&resolved.ilk.registration_status).eq("complete") {
                responses.push(ResponseItem::Result {
                    response_id: format!("resp:{request_id}:{event_id}:ready"),
                    adapter_id: runtime.adapter_id.clone(),
                    event_id: item.id.clone(),
                    status: "success".to_string(),
                    result_type: "profile_ready".to_string(),
                    payload: Some(serde_json::json!({
                        "external_profile_id": external_profile_id,
                        "ilk_id": resolved.ilk.ilk_id,
                        "ich_id": resolved.ich_id
                    })),
                    error_code: None,
                    error_message: None,
                    retryable: None,
                });
            }
        }
        Ok(None) => match strict_provision_ilk(
            &state.dispatcher,
            identity_cfg,
            identity_cfg.target.as_str(),
            &identity_input,
        )
        .await
        {
            Ok(src_ilk) => {
                durable_state.upsert_profile(
                    &runtime.adapter_id,
                    &runtime.tenant_id,
                    external_profile_id,
                    Some(src_ilk.clone()),
                    None,
                    "temporary",
                    Some(display_name.to_string()),
                    payload.metadata.clone(),
                );
                tracing::info!(
                    node_name = %state.node_name,
                    adapter_id = %runtime.adapter_id,
                    event_id = %item.id,
                    external_profile_id = %external_profile_id,
                    provisional_ilk = %src_ilk,
                    "linkedhelper provisioned provisional profile ILK"
                );
                responses.push(ResponseItem::Ack {
                    response_id: format!("resp:{request_id}:{event_id}"),
                    adapter_id: runtime.adapter_id.clone(),
                    event_id: item.id.clone(),
                });
            }
            Err(err) => {
                responses.push(identity_error_result(
                    runtime,
                    request_id,
                    &item.id,
                    "identity_error",
                    err,
                ));
            }
        },
        Err(err) => {
            responses.push(ResponseItem::Result {
                response_id: format!("resp:{request_id}:{event_id}:identity_lookup_error"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
                status: "error".to_string(),
                result_type: "identity_error".to_string(),
                payload: None,
                error_code: Some("identity_lookup_failed".to_string()),
                error_message: Some(err.to_string()),
                retryable: Some(true),
            });
        }
    }
}

async fn process_conversation_message(
    state: &Arc<HttpState>,
    runtime: &AdapterRuntime,
    identity_cfg: &IdentityProvisionConfig,
    durable_state: &mut LinkedHelperDurableState,
    request_id: &str,
    item: &PollItem,
    responses: &mut Vec<ResponseItem>,
) {
    let event_id = item.id.trim();
    let payload = match serde_json::from_value::<ConversationMessagePayload>(item.payload.clone()) {
        Ok(payload) => payload,
        Err(err) => {
            responses.push(ResponseItem::Result {
                response_id: format!("resp:{request_id}:{event_id}:invalid"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
                status: "error".to_string(),
                result_type: "validation_error".to_string(),
                payload: None,
                error_code: Some("invalid_conversation_message_payload".to_string()),
                error_message: Some(err.to_string()),
                retryable: Some(false),
            });
            return;
        }
    };

    let profile_ilk = payload.profile_ilk.trim();
    let contact_name = payload.contact_name.trim();
    let contact_external_id = payload.contact_external_composite_id.trim();
    let conversation_external_id = payload.conversation_external_id.trim();
    if profile_ilk.is_empty()
        || contact_name.is_empty()
        || contact_external_id.is_empty()
        || conversation_external_id.is_empty()
    {
        responses.push(ResponseItem::Result {
            response_id: format!("resp:{request_id}:{event_id}:invalid"),
            adapter_id: runtime.adapter_id.clone(),
            event_id: item.id.clone(),
            status: "error".to_string(),
            result_type: "validation_error".to_string(),
            payload: None,
            error_code: Some("invalid_conversation_message_payload".to_string()),
            error_message: Some(
                "conversation_message requires non-empty profile_ilk, contact_name, contact_external_composite_id and conversation_external_id"
                    .to_string(),
            ),
            retryable: Some(false),
        });
        return;
    }

    let Some(profile) = durable_state
        .profiles
        .values()
        .find(|profile| {
            profile.adapter_id == runtime.adapter_id
                && profile
                    .ilk_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    == Some(profile_ilk)
        })
        .cloned()
    else {
        tracing::info!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            event_id = %item.id,
            profile_ilk = %profile_ilk,
            error_code = "unknown_profile",
            "linkedhelper blocked conversation_message"
        );
        responses.push(blocked_profile_result(
            runtime,
            request_id,
            &item.id,
            "unknown_profile",
            "profile_ilk does not belong to a known profile for this adapter",
            Some(false),
        ));
        return;
    };

    if !profile_is_usable(&profile.status) {
        tracing::info!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            event_id = %item.id,
            external_profile_id = %profile.external_profile_id,
            profile_ilk = %profile_ilk,
            profile_status = %profile.status,
            error_code = "profile_not_ready",
            "linkedhelper blocked conversation_message"
        );
        responses.push(blocked_profile_result(
            runtime,
            request_id,
            &item.id,
            "profile_not_ready",
            "profile is not ready for automation yet",
            Some(true),
        ));
        return;
    }

    let Some(profile_ich_id) = profile
        .ich_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        tracing::info!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            event_id = %item.id,
            external_profile_id = %profile.external_profile_id,
            profile_ilk = %profile_ilk,
            error_code = "profile_missing_ich",
            "linkedhelper blocked conversation_message"
        );
        responses.push(blocked_profile_result(
            runtime,
            request_id,
            &item.id,
            "profile_missing_ich",
            "profile is missing its linked helper ICH state",
            Some(true),
        ));
        return;
    };

    let Some(ich_state) = durable_state.own_ichs.get(profile_ich_id).cloned() else {
        tracing::info!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            event_id = %item.id,
            external_profile_id = %profile.external_profile_id,
            profile_ilk = %profile_ilk,
            ich_id = %profile_ich_id,
            error_code = "automation_state_unknown",
            "linkedhelper blocked conversation_message"
        );
        responses.push(blocked_profile_result(
            runtime,
            request_id,
            &item.id,
            "automation_state_unknown",
            "profile automation state is not known yet",
            Some(true),
        ));
        return;
    };

    if !ich_state.automation_enabled {
        tracing::info!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            event_id = %item.id,
            external_profile_id = %profile.external_profile_id,
            profile_ilk = %profile_ilk,
            ich_id = %profile_ich_id,
            error_code = "automation_disabled",
            "linkedhelper blocked conversation_message"
        );
        responses.push(blocked_profile_result(
            runtime,
            request_id,
            &item.id,
            "automation_disabled",
            "profile automation is disabled for the linked helper ICH",
            Some(false),
        ));
        return;
    }

    let Some(dst_node) = runtime
        .dst_node
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
    else {
        responses.push(ResponseItem::Result {
            response_id: format!("resp:{request_id}:{event_id}:routing_error"),
            adapter_id: runtime.adapter_id.clone(),
            event_id: item.id.clone(),
            status: "error".to_string(),
            result_type: "routing_error".to_string(),
            payload: None,
            error_code: Some("missing_dst_node".to_string()),
            error_message: Some("adapter does not define dst_node".to_string()),
            retryable: Some(false),
        });
        return;
    };

    let message_payload = match normalize_conversation_payload(&payload.content) {
        Ok(value) => value,
        Err(message) => {
            responses.push(ResponseItem::Result {
                response_id: format!("resp:{request_id}:{event_id}:invalid_content"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
                status: "error".to_string(),
                result_type: "validation_error".to_string(),
                payload: None,
                error_code: Some("invalid_conversation_content".to_string()),
                error_message: Some(message),
                retryable: Some(false),
            });
            return;
        }
    };

    let contact_identity_input = ResolveOrCreateInput {
        channel: linkedhelper_contact_channel().to_string(),
        external_id: contact_external_id.to_string(),
        src_ilk_override: None,
        tenant_id: Some(runtime.tenant_id.clone()),
        tenant_hint: None,
        attributes: serde_json::json!({
            "display_name": contact_name,
            "contact_lh_person_id": payload.contact_lh_person_id.clone(),
            "source": "io.linkedhelper"
        }),
        ilk_type: Some("human".to_string()),
    };

    let (contact_src_ilk, contact_ich_id) = match resolve_identity_option_from_hive_id(
        &state.hive_id,
        &contact_identity_input.channel,
        &contact_identity_input.external_id,
        contact_identity_input.tenant_id.as_deref().unwrap_or(""),
    ) {
        Ok(Some(resolved)) => (resolved.ilk.ilk_id, Some(resolved.ich_id)),
        Ok(None) => match strict_provision_ilk(
            &state.dispatcher,
            identity_cfg,
            identity_cfg.target.as_str(),
            &contact_identity_input,
        )
        .await
        {
            Ok(src_ilk) => (src_ilk, None),
            Err(err) => {
                responses.push(identity_error_result(
                    runtime,
                    request_id,
                    &item.id,
                    "identity_error",
                    err,
                ));
                return;
            }
        },
        Err(err) => {
            responses.push(ResponseItem::Result {
                response_id: format!("resp:{request_id}:{event_id}:identity_lookup_error"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
                status: "error".to_string(),
                result_type: "identity_error".to_string(),
                payload: None,
                error_code: Some("identity_lookup_failed".to_string()),
                error_message: Some(err.to_string()),
                retryable: Some(true),
            });
            return;
        }
    };

    let thread_id = match linkedhelper_thread_id(&runtime.managed_instance_id, conversation_external_id) {
        Ok(thread_id) => thread_id,
        Err(err) => {
            responses.push(ResponseItem::Result {
                response_id: format!("resp:{request_id}:{event_id}:thread_error"),
                adapter_id: runtime.adapter_id.clone(),
                event_id: item.id.clone(),
                status: "error".to_string(),
                result_type: "validation_error".to_string(),
                payload: None,
                error_code: Some("invalid_conversation_external_id".to_string()),
                error_message: Some(err),
                retryable: Some(false),
            });
            return;
        }
    };

    let trace_id = new_trace_id();
    let profile_ilk_owned = profile_ilk.to_string();
    let sender = state.dispatcher.sender_snapshot();
    let src_uuid = sender.uuid().to_string();

    // Canonical inbound envelope: build the io-common `IoContext` + go through
    // `build_user_message`, so IO.linkedhelper emits the exact same shape as
    // io.slack / io.api instead of a hand-rolled `context.io`. `meta.ich` is
    // derived as `linkedhelper://<managed_instance_id>` (the node's own
    // managed-instance handle, mirroring `slack://<binding>`); `src_ilk` is the
    // contact, `dst_ilk` the target profile. LinkedHelper-specific routing +
    // reply metadata lives in `reply_target` (kind `linkedhelper_poll`).
    let io_context = IoContext {
        channel: linkedhelper_profile_channel().to_string(),
        entrypoint: PartyRef {
            kind: "linkedhelper_managed_instance".to_string(),
            id: runtime.managed_instance_id.clone(),
        },
        sender: PartyRef {
            kind: linkedhelper_contact_channel().to_string(),
            id: contact_external_id.to_string(),
        },
        conversation: ConversationRef {
            kind: "linkedhelper_conversation".to_string(),
            id: conversation_external_id.to_string(),
            thread_id: Some(thread_id.clone()),
        },
        message: MessageRef {
            id: item.id.clone(),
            timestamp: None,
        },
        reply_target: ReplyTarget {
            kind: "linkedhelper_poll".to_string(),
            address: runtime.adapter_id.clone(),
            params: serde_json::json!({
                "adapter_id": runtime.adapter_id,
                "managed_instance_id": runtime.managed_instance_id,
                "external_profile_id": profile.external_profile_id,
                "profile_ilk": profile_ilk_owned,
                "contact_external_id": contact_external_id,
                "contact_display_name": contact_name,
                "contact_lh_person_id": payload.contact_lh_person_id,
                "conversation_external_id": conversation_external_id,
                "request_id": request_id,
            }),
        },
    };
    let conversation_msg = build_user_message(
        &src_uuid,
        Some(dst_node.clone()),
        DEFAULT_TTL,
        trace_id.clone(),
        Some(contact_src_ilk.clone()),
        Some(profile_ilk_owned.clone()),
        wrap_in_meta_context(&io_context),
        message_payload,
    );

    if let Err(err) = sender.send(conversation_msg).await {
        tracing::warn!(
            node_name = %state.node_name,
            adapter_id = %runtime.adapter_id,
            event_id = %item.id,
            trace_id = %trace_id,
            dst_node = %dst_node,
            error = ?err,
            "failed to send linkedhelper conversation message to router"
        );
        responses.push(ResponseItem::Result {
            response_id: format!("resp:{request_id}:{event_id}:routing_error"),
            adapter_id: runtime.adapter_id.clone(),
            event_id: item.id.clone(),
            status: "error".to_string(),
            result_type: "routing_error".to_string(),
            payload: None,
            error_code: Some("router_unavailable".to_string()),
            error_message: Some("unable to dispatch conversation message to router".to_string()),
            retryable: Some(true),
        });
        return;
    }

    tracing::info!(
        node_name = %state.node_name,
        adapter_id = %runtime.adapter_id,
        event_id = %item.id,
        external_profile_id = %profile.external_profile_id,
        profile_ilk = %profile_ilk,
        contact_external_id = %contact_external_id,
        dst_node = %dst_node,
        thread_id = %thread_id,
        trace_id = %trace_id,
        "linkedhelper dispatched conversation_message to router"
    );

    responses.push(ResponseItem::Ack {
        response_id: format!("resp:{request_id}:{event_id}"),
        adapter_id: runtime.adapter_id.clone(),
        event_id: item.id.clone(),
    });
    responses.push(ResponseItem::Result {
        response_id: format!("resp:{request_id}:{event_id}:processed"),
        adapter_id: runtime.adapter_id.clone(),
        event_id: item.id.clone(),
        status: "success".to_string(),
        result_type: "conversation_processed".to_string(),
        payload: Some(serde_json::json!({
            "external_profile_id": profile.external_profile_id,
            "profile_ilk": profile_ilk,
            "contact_ilk": contact_src_ilk,
            "contact_ich_id": contact_ich_id,
            "conversation_external_id": conversation_external_id,
            "thread_id": thread_id,
            "dst_node": dst_node,
            "trace_id": trace_id
        })),
        error_code: None,
        error_message: None,
        retryable: None,
    });
}

fn normalize_conversation_payload(content: &Value) -> Result<Value, String> {
    match content {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err("conversation_message content must not be empty".to_string());
            }
            Ok(serde_json::json!({
                "type": "text",
                "content": trimmed
            }))
        }
        Value::Object(map) if !map.is_empty() => Ok(Value::Object(map.clone())),
        _ => Err(
            "conversation_message content must be a non-empty string or a non-empty object"
                .to_string(),
        ),
    }
}

fn identity_error_result(
    runtime: &AdapterRuntime,
    request_id: &str,
    event_id: &str,
    result_type: &str,
    err: IdentityError,
) -> ResponseItem {
    let (error_code, error_message, retryable) = match err {
        IdentityError::Timeout => (
            "identity_timeout".to_string(),
            "Identity did not respond in time".to_string(),
            true,
        ),
        IdentityError::Unavailable => (
            "identity_unavailable".to_string(),
            "Identity is currently unavailable".to_string(),
            true,
        ),
        IdentityError::Miss => (
            "identity_unavailable".to_string(),
            "Identity did not return a usable result".to_string(),
            true,
        ),
        IdentityError::Other(message) => (
            "identity_unavailable".to_string(),
            format!("Identity request failed: {message}"),
            true,
        ),
    };

    ResponseItem::Result {
        response_id: format!("resp:{request_id}:{event_id}:identity_error"),
        adapter_id: runtime.adapter_id.clone(),
        event_id: event_id.to_string(),
        status: "error".to_string(),
        result_type: result_type.to_string(),
        payload: None,
        error_code: Some(error_code),
        error_message: Some(error_message),
        retryable: Some(retryable),
    }
}

fn blocked_profile_result(
    runtime: &AdapterRuntime,
    request_id: &str,
    event_id: &str,
    error_code: &str,
    error_message: &str,
    retryable: Option<bool>,
) -> ResponseItem {
    ResponseItem::Result {
        response_id: format!("resp:{request_id}:{event_id}:blocked_profile"),
        adapter_id: runtime.adapter_id.clone(),
        event_id: event_id.to_string(),
        status: "error".to_string(),
        result_type: "blocked_profile".to_string(),
        payload: None,
        error_code: Some(error_code.to_string()),
        error_message: Some(error_message.to_string()),
        retryable,
    }
}

fn build_system_reply(
    incoming: &WireMessage,
    control_src: &str,
    response_msg: &str,
    payload: Value,
) -> WireMessage {
    let dst = if incoming.routing.src.trim().is_empty() {
        Destination::Broadcast
    } else {
        Destination::Unicast(incoming.routing.src.clone())
    };
    WireMessage {
        routing: Routing {
            src: control_src.to_string(),
            src_l2_name: None,
            dst,
            ttl: incoming.routing.ttl.max(1),
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(response_msg.to_string()),
            src_ilk: incoming.meta.src_ilk.clone(),
            dst_ilk: incoming.meta.dst_ilk.clone(),
            ich: incoming.meta.ich.clone(),
            thread_id: incoming.meta.thread_id.clone(),
            thread_seq: incoming.meta.thread_seq,
            ctx: None,
            ctx_seq: None,
            ctx_window: None,
            memory_package: incoming.meta.memory_package.clone(),
            scope: incoming.meta.scope.clone(),
            target: incoming.meta.target.clone(),
            action: Some(response_msg.to_string()),
            priority: incoming.meta.priority.clone(),
            context: incoming.meta.context.clone(),
            ..Meta::default()
        },
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_id_is_keyed_on_managed_instance() {
        let a = linkedhelper_thread_id("lhmi_1", "conv_1").unwrap();
        // Stable: same managed instance + conversation => same thread id, so
        // continuity survives even when the adapter binding changes.
        assert_eq!(a, linkedhelper_thread_id("lhmi_1", "conv_1").unwrap());
        // Distinct per managed instance and per conversation.
        assert_ne!(a, linkedhelper_thread_id("lhmi_2", "conv_1").unwrap());
        assert_ne!(a, linkedhelper_thread_id("lhmi_1", "conv_2").unwrap());
    }

    fn result_item(retryable: Option<bool>) -> ResponseItem {
        ResponseItem::Result {
            response_id: "resp:x".to_string(),
            adapter_id: "adp".to_string(),
            event_id: "evt".to_string(),
            status: "success".to_string(),
            result_type: "conversation_processed".to_string(),
            payload: Some(serde_json::json!({ "k": "v" })),
            error_code: None,
            error_message: None,
            retryable,
        }
    }

    #[test]
    fn is_retryable_only_for_retryable_result() {
        let ack = ResponseItem::Ack {
            response_id: "r".to_string(),
            adapter_id: "a".to_string(),
            event_id: "e".to_string(),
        };
        assert!(!ack.is_retryable());
        assert!(result_item(Some(true)).is_retryable());
        assert!(!result_item(Some(false)).is_retryable());
        assert!(!result_item(None).is_retryable());
    }

    #[test]
    fn to_stored_roundtrips_through_from() {
        let item = result_item(Some(false));
        let stored = item.to_stored();
        let back = ResponseItem::from(stored);
        // ResponseItem has no PartialEq; compare via their JSON projections.
        assert_eq!(
            serde_json::to_value(&item).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
    }

    #[test]
    fn auth_rejection_maps_to_http_status() {
        let validator = AdapterAuthValidator::new("adp", "mi", None, Some("s3cret".to_string()));
        let rejection = validator
            .validate(&InboundAuthRequest {
                header_adapter_id: None,
                bearer: Some("s3cret"),
                body_adapter_id: Some("adp"),
                body_managed_instance_id: Some("mi"),
                body_local_instance_id: None,
            })
            .expect_err("missing adapter-id header should be rejected");
        let response = auth_rejection_response(rejection, OPERATIONAL_ENABLED);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn operating_control_is_continue_when_enabled() {
        let control = PollControl::operating(OPERATIONAL_ENABLED);
        assert_eq!(control.operational_state, OPERATIONAL_ENABLED);
        assert_eq!(control.directive, poll_directive::CONTINUE);
        assert!(control.reason.is_none());
    }

    #[test]
    fn operating_control_is_pause_when_disabled() {
        let control = PollControl::operating(OPERATIONAL_DISABLED);
        assert_eq!(control.operational_state, OPERATIONAL_DISABLED);
        assert_eq!(control.directive, poll_directive::PAUSE);
        assert_eq!(control.reason.as_deref(), Some("instance_disabled"));
    }

    #[test]
    fn reject_control_maps_error_codes_to_directives() {
        // Credential failure => recover credentials from Cloud.
        assert_eq!(
            PollControl::for_reject("invalid_adapter_secret", OPERATIONAL_ENABLED).directive,
            poll_directive::REENROLL
        );
        // Stale instance→node mapping => re-ask Cloud for the runtime destination.
        for code in [
            "adapter_not_allowed",
            "managed_instance_id_mismatch",
            "local_instance_id_mismatch",
        ] {
            assert_eq!(
                PollControl::for_reject(code, OPERATIONAL_ENABLED).directive,
                poll_directive::REPROVISION,
                "{code} should map to reprovision"
            );
        }
        // Transient node-side conditions => retry the runtime poll.
        for code in [
            "node_not_ready",
            "node_binding_unavailable",
            "auth_secret_unavailable",
            "durable_state_unavailable",
        ] {
            assert_eq!(
                PollControl::for_reject(code, OPERATIONAL_ENABLED).directive,
                poll_directive::RETRY,
                "{code} should map to retry"
            );
        }
        // Administrative disable => pause.
        assert_eq!(
            PollControl::for_reject("instance_disabled", OPERATIONAL_DISABLED).directive,
            poll_directive::PAUSE
        );
        // Malformed request => nothing administratively changed.
        assert_eq!(
            PollControl::for_reject("invalid_mode", OPERATIONAL_ENABLED).directive,
            poll_directive::CONTINUE
        );
        // The reason always echoes the stable error code.
        assert_eq!(
            PollControl::for_reject("invalid_mode", OPERATIONAL_ENABLED).reason.as_deref(),
            Some("invalid_mode")
        );
    }

    #[test]
    fn poll_response_serializes_control_block() {
        let response = PollResponse {
            ok: true,
            accepted_at: "2026-07-03T00:00:00Z".to_string(),
            response_id: "resp:x".to_string(),
            adapter_id: "adp".to_string(),
            actions: Vec::new(),
            control: PollControl::operating(OPERATIONAL_ENABLED),
            items: Vec::new(),
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["control"]["operational_state"], "enabled");
        assert_eq!(value["control"]["directive"], "continue");
        // reason/retry_after are omitted when absent (skip_serializing_if).
        assert!(value["control"].get("reason").is_none());
    }
}
