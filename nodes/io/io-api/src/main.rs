#![forbid(unsafe_code)]

use anyhow::Result;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    build_node_secret_record, compute_thread_id, connect, load_node_secret_record,
    load_node_secret_record_with_root, managed_node_config_path, save_node_secret_record,
    save_node_secret_record_with_root, try_handle_default_node_status, NodeConfig, NodeSecretError,
    NodeSecretRecord, NodeSecretWriteOptions, NodeUuidMode, ThreadIdInput, FLUXBEE_NODE_NAME_ENV,
};
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_api_adapter_config::{api_key_storage_key, IoApiAdapterConfigContract};
use io_common::io_context::{ConversationRef, IoContext, MessageRef, PartyRef, ReplyTarget};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload,
    ensure_config_version_advances, parse_and_validate_io_control_plane_request, IoConfigSource,
    IoControlPlaneErrorInfo, IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_invalid_runtime_credentials,
    log_config_set_persist_error, log_config_set_stale_rejected,
    log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::{default_state_dir, persist_io_control_plane_state};
use io_common::provision::{FluxbeeIdentityProvisioner, IdentityProvisionConfig, RouterInbox};
use io_common::relay::{
    AssembledTurn, InMemoryRelayStore, RelayBuffer, RelayDecision, RelayFlushHints, RelayFragment,
    RelayPolicy,
};
use io_common::router_message::DEFAULT_TTL;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

#[derive(Clone)]
struct Config {
    node_name: String,
    island_id: String,
    node_version: String,
    listen_addr: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    state_dir: PathBuf,
    spawn_config_path: PathBuf,
    identity_target: String,
    identity_timeout_ms: u64,
    ttl: u32,
    dedup_ttl_ms: u64,
    dedup_max_entries: usize,
    relay: ApiRelayConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApiRelayConfig {
    window_ms: u64,
    max_open_sessions: usize,
    max_fragments_per_session: usize,
    max_bytes_per_session: usize,
}

impl Default for ApiRelayConfig {
    fn default() -> Self {
        Self {
            window_ms: 0,
            max_open_sessions: 10_000,
            max_fragments_per_session: 8,
            max_bytes_per_session: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ApiAuthRegistry {
    keys: Vec<ApiKeyRuntime>,
}

#[derive(Debug, Clone)]
struct ApiKeyRuntime {
    key_id: String,
    token: String,
    caller_identity: Option<Value>,
}

#[derive(Clone)]
struct HttpState {
    node_name: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
    sender: fluxbee_sdk::NodeSender,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
}

#[derive(Clone)]
struct RuntimeUpdateHandles {
    node_name: String,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
}

#[derive(Debug, Clone)]
struct AuthMatch {
    key_id: String,
    caller_identity: Option<Value>,
}

struct SpawnConfig {
    path: PathBuf,
    doc: Value,
}

struct ParsedHttpMessage {
    request_id: String,
    identity_input: ResolveOrCreateInput,
    io_context: IoContext,
    payload: Value,
    relay_final: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,io_api=debug,fluxbee_sdk=info")
    });
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!(
        node_name = %config.node_name,
        island_id = %config.island_id,
        listen = %config.listen_addr,
        router_socket = %config.router_socket.display(),
        state_dir = %config.state_dir.display(),
        spawn_config_path = %config.spawn_config_path.display(),
        "io-api starting"
    );

    let (sender, receiver) = connect(&NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    })
    .await?;

    tracing::info!(
        full_name = %receiver.full_name(),
        vpn_id = %receiver.vpn_id(),
        "io-api connected to router"
    );

    let inbox = Arc::new(Mutex::new(RouterInbox::new(receiver)));
    let identity: Arc<dyn IdentityResolver> = Arc::new(ShmIdentityResolver::new(&config.island_id));
    let provisioner: Arc<dyn IdentityProvisioner> = Arc::new(FluxbeeIdentityProvisioner::new(
        sender.clone(),
        inbox.clone(),
        IdentityProvisionConfig {
            target: config.identity_target.clone(),
            timeout: Duration::from_millis(config.identity_timeout_ms),
        },
    ));

    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoApiAdapterConfigContract);
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

    let auth_registry = Arc::new(RwLock::new(ApiAuthRegistry::default()));
    if let Some(effective) = boot_state.effective_config.clone() {
        match prepare_runtime_api_config(&config.node_name, &effective, None) {
            Ok((sanitized_effective, registry)) => {
                tracing::info!(
                    key_count = registry.keys.len(),
                    config_source = boot_state.config_source.as_str(),
                    "io-api runtime auth initialized"
                );
                boot_state.effective_config = Some(sanitized_effective);
                *auth_registry.write().await = registry;
            }
            Err(err) => {
                boot_state.current_state = IoNodeLifecycleState::FailedConfig;
                boot_state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err.to_string(),
                });
                tracing::warn!(
                    node_name = %config.node_name,
                    error = %err,
                    "boot effective config is missing/invalid api auth config; starting in FAILED_CONFIG"
                );
            }
        }
    }

    let initial_dst_node = extract_runtime_dst_node(boot_state.effective_config.as_ref());
    let inbound = Arc::new(Mutex::new(InboundProcessor::new(
        sender.uuid().to_string(),
        InboundConfig {
            ttl: config.ttl,
            dedup_ttl: Duration::from_millis(config.dedup_ttl_ms),
            dedup_max_entries: config.dedup_max_entries,
            dst_node: initial_dst_node,
            provision_on_miss: true,
            blob_runtime: None,
        },
    )));

    let relay_policy = api_relay_policy(&config, boot_state.effective_config.as_ref())?;
    tracing::info!(
        relay_enabled = relay_policy.enabled,
        relay_window_ms = relay_policy.relay_window_ms,
        relay_max_open_sessions = relay_policy.max_open_sessions,
        relay_max_fragments = relay_policy.max_fragments_per_session,
        relay_max_bytes = relay_policy.max_bytes_per_session,
        "io-api relay policy initialized"
    );
    let relay = Arc::new(Mutex::new(
        RelayBuffer::new(relay_policy.clone(), InMemoryRelayStore::new())
            .map_err(|err| anyhow::anyhow!("invalid relay policy: {err}"))?,
    ));

    let control_plane = Arc::new(RwLock::new(boot_state.clone()));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    ));
    let control_src = sender.full_name().to_string();

    let http_state = Arc::new(HttpState {
        node_name: config.node_name.clone(),
        control_plane: control_plane.clone(),
        adapter_contract: adapter_contract.clone(),
        auth_registry: auth_registry.clone(),
        sender: sender.clone(),
        identity: identity.clone(),
        provisioner: provisioner.clone(),
        inbound: inbound.clone(),
        relay: relay.clone(),
    });

    let http_listener = TcpListener::bind(&config.listen_addr)
        .await
        .map_err(|err| {
            anyhow::anyhow!("failed to bind HTTP listener {}: {err}", config.listen_addr)
        })?;
    let http_task = tokio::spawn(run_http_server(http_listener, http_state.clone()));
    let relay_flush_task = tokio::spawn(run_relay_flush_loop(
        relay,
        sender.clone(),
        identity,
        provisioner,
        inbound,
    ));
    let control_task = tokio::spawn(run_router_control_loop(
        inbox,
        sender,
        config.node_name.clone(),
        config.state_dir.clone(),
        control_src,
        control_plane,
        control_metrics,
        adapter_contract,
        auth_registry,
        http_state_ref_for_runtime_updates(config.node_name.clone(), http_state),
    ));

    let _ = tokio::join!(http_task, relay_flush_task, control_task);
    Ok(())
}

impl Config {
    fn from_env() -> Result<Self> {
        let resolved_node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            anyhow::anyhow!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let resolved_island_id = hive_from_node_name(&resolved_node_name).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid {FLUXBEE_NODE_NAME_ENV}='{resolved_node_name}': expected <name>@<hive>"
            )
        })?;
        let spawn_cfg = load_spawn_config(&resolved_node_name)?;
        tracing::info!(path = %spawn_cfg.path.display(), "io-api loaded spawn config");
        let spawn_doc = Some(&spawn_cfg.doc);

        let listen_address = env("IO_API_LISTEN_ADDRESS")
            .or_else(|| {
                json_get_string_opt(
                    spawn_doc,
                    &[
                        "config.listen.address",
                        "listen.address",
                        "node.listen.address",
                    ],
                )
            })
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let listen_port = env("IO_API_LISTEN_PORT")
            .and_then(|value| value.parse::<u16>().ok())
            .or_else(|| {
                json_get_u16_opt(
                    spawn_doc,
                    &["config.listen.port", "listen.port", "node.listen.port"],
                )
            })
            .unwrap_or(8080);

        Ok(Self {
            node_name: resolved_node_name.clone(),
            island_id: resolved_island_id.clone(),
            node_version: env("NODE_VERSION")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_doc,
                        &["_system.runtime_version", "runtime.version", "node.version"],
                    )
                })
                .unwrap_or_else(|| "0.1".to_string()),
            listen_addr: format!("{listen_address}:{listen_port}"),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| {
                        json_get_string_opt(spawn_doc, &["node.router_socket", "router_socket"])
                    })
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_doc,
                            &["node.uuid_persistence_dir", "uuid_persistence_dir"],
                        )
                    })
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.config_dir", "config_dir"]))
                    .unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            state_dir: PathBuf::from(
                env("STATE_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.state_dir", "state_dir"]))
                    .unwrap_or_else(|| default_state_dir().display().to_string()),
            ),
            spawn_config_path: spawn_cfg.path,
            identity_target: env("IDENTITY_TARGET")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_doc,
                        &[
                            "config.identity.target",
                            "identity.target",
                            "identity_target",
                        ],
                    )
                })
                .unwrap_or_else(|| format!("SY.identity@{resolved_island_id}")),
            identity_timeout_ms: env("IDENTITY_TIMEOUT_MS")
                .and_then(|value| value.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(
                        spawn_doc,
                        &[
                            "config.identity.timeout_ms",
                            "identity.timeout_ms",
                            "identity_timeout_ms",
                        ],
                    )
                })
                .unwrap_or(10_000),
            ttl: env("TTL")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_TTL),
            dedup_ttl_ms: env("DEDUP_TTL_MS")
                .and_then(|value| value.parse().ok())
                .unwrap_or(10 * 60 * 1000),
            dedup_max_entries: env("DEDUP_MAX_ENTRIES")
                .and_then(|value| value.parse().ok())
                .unwrap_or(50_000),
            relay: ApiRelayConfig {
                window_ms: json_get_u64_opt(
                    spawn_doc,
                    &[
                        "config.io.relay.window_ms",
                        "io.relay.window_ms",
                        "relay.window_ms",
                    ],
                )
                .unwrap_or_default(),
                max_open_sessions: json_get_usize_opt(
                    spawn_doc,
                    &[
                        "config.io.relay.max_open_sessions",
                        "io.relay.max_open_sessions",
                        "relay.max_open_sessions",
                    ],
                )
                .unwrap_or(10_000),
                max_fragments_per_session: json_get_usize_opt(
                    spawn_doc,
                    &[
                        "config.io.relay.max_fragments_per_session",
                        "io.relay.max_fragments_per_session",
                        "relay.max_fragments_per_session",
                    ],
                )
                .unwrap_or(8),
                max_bytes_per_session: json_get_usize_opt(
                    spawn_doc,
                    &[
                        "config.io.relay.max_bytes_per_session",
                        "io.relay.max_bytes_per_session",
                        "relay.max_bytes_per_session",
                    ],
                )
                .unwrap_or(256 * 1024),
            },
        })
    }
}

async fn run_http_server(listener: TcpListener, state: Arc<HttpState>) -> Result<()> {
    let app = Router::new()
        .route("/schema", get(get_schema))
        .route("/messages", post(post_messages))
        .with_state(state);
    tracing::info!(addr = %listener.local_addr()?, "io-api http listener ready");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_router_control_loop(
    inbox: Arc<Mutex<RouterInbox>>,
    sender: fluxbee_sdk::NodeSender,
    node_name: String,
    state_dir: PathBuf,
    control_src: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
    runtime_updates: RuntimeUpdateHandles,
) -> Result<()> {
    loop {
        let maybe_msg = inbox
            .lock()
            .await
            .recv_next_timeout(Duration::from_millis(250))
            .await?;
        let Some(msg) = maybe_msg else {
            continue;
        };

        if try_handle_default_node_status(&sender, &msg).await? {
            continue;
        }

        if let Some(response) = handle_io_control_plane_message(
            &msg,
            &node_name,
            control_src.as_str(),
            &state_dir,
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            auth_registry.clone(),
            runtime_updates.clone(),
        )
        .await
        {
            sender.send(response).await?;
            continue;
        }

        tracing::debug!(
            msg_type = %msg.meta.msg_type,
            msg = %msg.meta.msg.as_deref().unwrap_or(""),
            trace_id = %msg.routing.trace_id,
            "io-api ignored non-control-plane router message"
        );
    }
}

async fn run_relay_flush_loop(
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    sender: fluxbee_sdk::NodeSender,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let turns = relay.lock().await.flush_expired(now_epoch_ms());
        for turn in turns {
            dispatch_assembled_turn(
                sender.clone(),
                identity.as_ref(),
                provisioner.as_ref(),
                inbound.clone(),
                turn,
                "relay scheduled flush",
            )
            .await;
        }
    }
}

async fn get_schema(State(state): State<Arc<HttpState>>) -> Response {
    let snapshot = state.control_plane.read().await.clone();
    let redacted = redact_state(&snapshot, state.adapter_contract.as_ref());
    let key_count = state.auth_registry.read().await.keys.len();

    let body = if let Some(effective) = redacted.effective_config.as_ref() {
        build_configured_schema(
            &state.node_name,
            &snapshot,
            effective,
            state.adapter_contract.as_ref(),
            key_count,
        )
    } else {
        serde_json::json!({
            "status": lifecycle_status(&snapshot.current_state),
            "node_name": state.node_name,
            "runtime": "IO.api",
            "contract_version": 1,
            "effective_schema": Value::Null,
            "required_configuration": state.adapter_contract.required_fields(),
            "last_error": snapshot.last_error,
        })
    };

    (StatusCode::OK, Json(body)).into_response()
}

async fn post_messages(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let headers = request.headers().clone();

    let state_snapshot = state.control_plane.read().await.clone();
    if state_snapshot.current_state != IoNodeLifecycleState::Configured
        || state_snapshot.effective_config.is_none()
    {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_not_configured",
            "IO.api instance is not configured yet",
        );
    }
    let effective = state_snapshot
        .effective_config
        .as_ref()
        .expect("checked above");

    let bearer = match extract_bearer_token(&headers) {
        Some(value) => value,
        None => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing bearer token",
            )
        }
    };
    let auth_match = match authenticate_bearer(&state.auth_registry, bearer.as_str()).await {
        Some(value) => value,
        None => {
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Invalid bearer token",
            )
        }
    };

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string();
    let accepted_content_types = extract_accepted_content_types(Some(effective));
    if !content_type_matches_any(content_type.as_str(), &accepted_content_types) {
        return api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Request content type is not accepted by this IO.api instance",
        );
    }
    if content_type.starts_with("multipart/form-data") {
        return api_error(
            StatusCode::NOT_IMPLEMENTED,
            "multipart_not_implemented",
            "multipart/form-data ingress is not implemented yet in this phase",
        );
    }

    let body_limit = extract_max_request_bytes(Some(effective));
    let body = match to_bytes(request.into_body(), body_limit).await {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request body exceeds configured limit",
            );
        }
    };

    let envelope: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                format!("Request body is not valid JSON: {err}"),
            );
        }
    };

    let parsed = match parse_json_message_request(&envelope, effective, &auth_match) {
        Ok(value) => value,
        Err((status, code, message)) => return api_error(status, code, message),
    };

    let relay_fragment =
        build_api_relay_fragment(&state.node_name, &parsed, envelope.clone(), &auth_match);

    match state.relay.lock().await.handle_fragment(relay_fragment) {
        RelayDecision::Hold => accepted_response(&state.node_name, parsed.request_id, None, "held"),
        RelayDecision::FlushNow(turn) => {
            let trace_id = dispatch_assembled_turn_for_http(
                state.sender.clone(),
                state.identity.as_ref(),
                state.provisioner.as_ref(),
                state.inbound.clone(),
                turn,
                "relay immediate flush",
            )
            .await;
            accepted_response(
                &state.node_name,
                parsed.request_id,
                trace_id,
                "flushed_immediately",
            )
        }
        RelayDecision::DropDuplicate => accepted_response(
            &state.node_name,
            parsed.request_id,
            None,
            "duplicate_dropped",
        ),
        RelayDecision::RejectCapacity => {
            let outcome = state
                .inbound
                .lock()
                .await
                .process_inbound(
                    state.identity.as_ref(),
                    Some(state.provisioner.as_ref()),
                    parsed.identity_input,
                    parsed.io_context,
                    parsed.payload,
                )
                .await;
            let trace_id = send_inbound_outcome(
                state.sender.clone(),
                outcome,
                "relay fail-open direct inbound",
            )
            .await;
            accepted_response(
                &state.node_name,
                parsed.request_id,
                trace_id,
                "flushed_immediately",
            )
        }
        RelayDecision::DropExpired => api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "relay_unavailable",
            "Relay session expired before request could be accepted",
        ),
    }
}

async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
    runtime_updates: RuntimeUpdateHandles,
) -> Option<WireMessage> {
    let command = msg.meta.msg.as_deref().unwrap_or_default();
    if !is_control_plane_msg_type(&msg.meta.msg_type) {
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
        let key_count = auth_registry.read().await.keys.len();
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
            "config_source": state.config_source.as_str(),
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": state.last_error,
            "metrics": {
                "control_plane": metrics
            },
            "runtime": {
                "auth_key_count": key_count
            }
        });
        return Some(build_system_reply(
            msg,
            control_src,
            "STATUS_RESPONSE",
            payload,
        ));
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
                build_io_adapter_contract_payload(
                    adapter_contract,
                    state.effective_config.as_ref(),
                ),
            );
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "metrics".to_string(),
                    serde_json::json!({
                        "control_plane": control_metrics.snapshot(),
                        "auth": {
                            "key_count": auth_registry.read().await.keys.len()
                        }
                    }),
                );
            }
            payload
        }
        Ok(IoControlPlaneRequest::Set(set_payload)) => {
            apply_io_config_set(
                &set_payload,
                node_name,
                state_dir,
                control_plane.clone(),
                control_metrics,
                adapter_contract,
                auth_registry,
                runtime_updates,
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

async fn apply_io_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
    runtime_updates: RuntimeUpdateHandles,
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
                payload.config_version,
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

    let (effective, registry) = match prepare_runtime_api_config(node_name, &candidate, None) {
        Ok(out) => out,
        Err(err) => {
            let err_text = err.to_string();
            log_config_set_invalid_runtime_credentials(
                node_name,
                payload.config_version,
                err_text.as_str(),
            );
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: err_text.clone(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                payload.config_version,
                "invalid_config",
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                "invalid_config",
                err_text,
            );
        }
    };

    let previous_effective = state.effective_config.clone();
    let mut apply_hot = Vec::<String>::new();
    let mut apply_reinit = Vec::<String>::new();
    let mut apply_restart_required = Vec::<String>::new();

    if extract_runtime_dst_node(previous_effective.as_ref())
        != extract_runtime_dst_node(Some(&effective))
    {
        runtime_updates
            .inbound
            .lock()
            .await
            .set_dst_node(extract_runtime_dst_node(Some(&effective)));
        apply_hot.push("io.dst_node".to_string());
    }

    match extract_runtime_relay_config(Some(&effective), &ApiRelayConfig::default())
        .and_then(api_relay_policy_from_config)
    {
        Ok(policy) => {
            let mut relay_guard = runtime_updates.relay.lock().await;
            let relay_changed = relay_guard.policy() != &policy;
            if let Err(err) = relay_guard.replace_policy(policy.clone()) {
                let err_text = err.to_string();
                state.current_state = IoNodeLifecycleState::FailedConfig;
                state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err_text.clone(),
                });
                control_metrics.record_config_set_error(
                    state.current_state.as_str(),
                    payload.config_version,
                    "invalid_config",
                );
                let redacted = redact_state(&state, adapter_contract);
                return build_io_config_set_error_payload(
                    node_name,
                    &redacted,
                    "invalid_config",
                    err_text,
                );
            }
            drop(relay_guard);
            if relay_changed {
                apply_hot.push("io.relay.*".to_string());
            }
        }
        Err(err) => {
            let err_text = err.to_string();
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: err_text.clone(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                payload.config_version,
                "invalid_config",
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                "invalid_config",
                err_text,
            );
        }
    }

    if section_changed(
        previous_effective.as_ref(),
        &effective,
        &["auth", "api_keys"],
    ) {
        *auth_registry.write().await = registry.clone();
        apply_reinit.push("auth.api_keys".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["listen"]) {
        apply_restart_required.push("listen.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["ingress"]) {
        apply_hot.push("ingress.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["node"]) {
        apply_restart_required.push("node.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["runtime"]) {
        apply_restart_required.push("runtime.*".to_string());
    }

    state.current_state = IoNodeLifecycleState::Configured;
    state.config_source = IoConfigSource::Dynamic;
    state.schema_version = payload.schema_version;
    state.config_version = payload.config_version;
    state.effective_config = Some(effective.clone());
    state.last_error = None;

    if let Err(err) = persist_io_control_plane_state(state_dir, node_name, &state) {
        let err_text = err.to_string();
        log_config_set_persist_error(
            node_name,
            payload.schema_version,
            payload.config_version,
            err_text.as_str(),
        );
        state.current_state = IoNodeLifecycleState::FailedConfig;
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: "config_persist_error".to_string(),
            message: err_text.clone(),
        });
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            payload.config_version,
            "config_persist_error",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            "config_persist_error",
            err_text,
        );
    }

    tracing::info!(
        node_name = %runtime_updates.node_name,
        key_count = registry.keys.len(),
        "io-api runtime config hot-applied"
    );
    control_metrics.record_config_set_ok(state.current_state.as_str(), state.config_version);
    log_config_set_applied(
        node_name,
        payload.schema_version,
        payload.config_version,
        &apply_hot,
        &apply_reinit,
        &apply_restart_required,
    );

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

fn http_state_ref_for_runtime_updates(
    node_name: String,
    state: Arc<HttpState>,
) -> RuntimeUpdateHandles {
    RuntimeUpdateHandles {
        node_name,
        inbound: state.inbound.clone(),
        relay: state.relay.clone(),
    }
}

fn lifecycle_status(state: &IoNodeLifecycleState) -> &'static str {
    match state {
        IoNodeLifecycleState::Unconfigured => "unconfigured",
        IoNodeLifecycleState::Configured => "configured",
        IoNodeLifecycleState::FailedConfig => "failed_config",
    }
}

fn build_configured_schema(
    node_name: &str,
    state: &IoControlPlaneState,
    effective: &Value,
    adapter_contract: &dyn IoAdapterConfigContract,
    key_count: usize,
) -> Value {
    let subject_mode =
        extract_subject_mode(Some(effective)).unwrap_or_else(|| "unknown".to_string());
    let accepted_content_types = extract_accepted_content_types(Some(effective));
    let subject = if subject_mode == "explicit_subject" {
        serde_json::json!({
            "required": true,
            "allowed": true,
            "lookup_key_field": "external_user_id",
            "optional_identity_candidates": ["display_name", "email", "tenant_hint"]
        })
    } else {
        serde_json::json!({
            "required": false,
            "allowed": false,
            "resolution": "derived_from_authenticated_caller"
        })
    };
    let required_fields = if subject_mode == "explicit_subject" {
        serde_json::json!({
            "json": ["subject", "message"],
            "multipart": ["metadata"]
        })
    } else {
        serde_json::json!({
            "json": ["message"],
            "multipart": ["metadata"]
        })
    };
    serde_json::json!({
        "status": lifecycle_status(&state.current_state),
        "node_name": node_name,
        "runtime": "IO.api",
        "contract_version": 1,
        "config_version": state.config_version,
        "auth": {
            "mode": effective.get("auth").and_then(|auth| auth.get("mode")).and_then(Value::as_str).unwrap_or("api_key"),
            "transport": "Authorization: Bearer <token>",
            "active_key_count": key_count
        },
        "ingress": {
            "subject_mode": subject_mode,
            "accepted_content_types": accepted_content_types
        },
        "required_fields": required_fields,
        "subject": subject,
        "attachments": {
            "supported": accepted_content_types.iter().any(|value| value == "multipart/form-data"),
            "mode": "multipart"
        },
        "relay": {
            "config_path": "config.io.relay.*",
            "effective": effective.get("io").and_then(|io| io.get("relay")).cloned().unwrap_or(Value::Null)
        },
        "secrets": build_io_adapter_contract_payload(adapter_contract, Some(effective))
            .get("secrets")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
        "last_error": state.last_error.clone(),
    })
}

fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    let token = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let trimmed = token.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

async fn authenticate_bearer(
    registry: &Arc<RwLock<ApiAuthRegistry>>,
    token: &str,
) -> Option<AuthMatch> {
    let registry = registry.read().await;
    registry.keys.iter().find_map(|entry| {
        (entry.token == token).then(|| AuthMatch {
            key_id: entry.key_id.clone(),
            caller_identity: entry.caller_identity.clone(),
        })
    })
}

fn content_type_matches_any(content_type: &str, accepted: &[String]) -> bool {
    accepted
        .iter()
        .any(|candidate| content_type.starts_with(candidate))
}

fn extract_accepted_content_types(effective_config: Option<&Value>) -> Vec<String> {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("accepted_content_types"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| vec!["application/json".to_string()])
}

fn extract_max_request_bytes(effective_config: Option<&Value>) -> usize {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("max_request_bytes"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(256 * 1024)
}

fn parse_json_message_request(
    envelope: &Value,
    effective: &Value,
    auth_match: &AuthMatch,
) -> std::result::Result<ParsedHttpMessage, (StatusCode, &'static str, String)> {
    let subject_mode = extract_subject_mode(Some(effective)).unwrap_or_default();
    let message = envelope
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Field 'message' is required".to_string(),
            )
        })?;
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if text.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            "Field 'message.text' is required in JSON ingress until attachments are implemented"
                .to_string(),
        ));
    }

    let subject = envelope.get("subject");
    let caller_identity = auth_match.caller_identity.as_ref();
    let (external_user_id, display_name, email, tenant_hint) = if subject_mode == "explicit_subject"
    {
        let subject_obj = subject.and_then(Value::as_object).ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Field 'subject' is required for subject_mode=explicit_subject".to_string(),
            )
        })?;
        let external_user_id = subject_obj
            .get("external_user_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_payload",
                    "Field 'subject.external_user_id' is required".to_string(),
                )
            })?
            .to_string();
        (
            external_user_id,
            subject_obj.get("display_name").cloned(),
            subject_obj.get("email").cloned(),
            subject_obj.get("tenant_hint").cloned(),
        )
    } else {
        if subject.is_some() {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Field 'subject' is not allowed for subject_mode=caller_is_subject".to_string(),
            ));
        }
        let caller = caller_identity.and_then(Value::as_object).ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Authenticated caller does not define caller_identity".to_string(),
            )
        })?;
        let external_user_id = caller
            .get("external_user_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_payload",
                    "Authenticated caller is missing external_user_id".to_string(),
                )
            })?
            .to_string();
        (
            external_user_id,
            caller.get("display_name").cloned(),
            caller.get("email").cloned(),
            caller.get("tenant_hint").cloned(),
        )
    };

    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let external_message_id = message
        .get("external_message_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| request_id.clone());
    let timestamp = message
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let conversation_seed = envelope
        .get("options")
        .and_then(|options| options.get("metadata"))
        .and_then(|metadata| metadata.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| external_user_id.clone());
    let thread_id = compute_thread_id(ThreadIdInput::PersistentChannel {
        channel_type: "api",
        entrypoint_id: Some(
            effective
                .get("listen")
                .and_then(|listen| listen.get("address"))
                .and_then(Value::as_str)
                .unwrap_or("api"),
        ),
        conversation_id: conversation_seed.as_str(),
    })
    .map_err(|err| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            format!("Failed to build thread_id: {err}"),
        )
    })?;

    let mut attributes = serde_json::Map::new();
    attributes.insert(
        "auth_key_id".to_string(),
        Value::String(auth_match.key_id.clone()),
    );
    if let Some(value) = display_name.clone() {
        attributes.insert("display_name".to_string(), value);
    }
    if let Some(value) = email.clone() {
        attributes.insert("email".to_string(), value);
    }
    if let Some(metadata) = envelope
        .get("options")
        .and_then(|options| options.get("metadata"))
        .cloned()
    {
        attributes.insert("request_metadata".to_string(), metadata);
    }

    let text_payload = TextV1Payload::new(text, vec![]).to_value().map_err(|err| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            format!("Unable to build text/v1 payload: {err}"),
        )
    })?;

    Ok(ParsedHttpMessage {
        request_id,
        identity_input: ResolveOrCreateInput {
            channel: "api".to_string(),
            external_id: external_user_id.clone(),
            tenant_hint: tenant_hint
                .as_ref()
                .and_then(Value::as_str)
                .map(ToString::to_string),
            attributes: Value::Object(attributes),
        },
        io_context: IoContext {
            channel: "api".to_string(),
            entrypoint: PartyRef {
                kind: "io_api_instance".to_string(),
                id: effective
                    .get("listen")
                    .and_then(|listen| listen.get("address"))
                    .and_then(Value::as_str)
                    .unwrap_or("api")
                    .to_string(),
            },
            sender: PartyRef {
                kind: "api_subject".to_string(),
                id: external_user_id,
            },
            conversation: ConversationRef {
                kind: "api_conversation".to_string(),
                id: conversation_seed,
                thread_id: Some(thread_id),
            },
            message: MessageRef {
                id: external_message_id,
                timestamp,
            },
            reply_target: ReplyTarget {
                kind: "io_api_noop".to_string(),
                address: effective
                    .get("listen")
                    .and_then(|listen| listen.get("address"))
                    .and_then(Value::as_str)
                    .unwrap_or("api")
                    .to_string(),
                params: serde_json::json!({}),
            },
        },
        payload: text_payload,
        relay_final: envelope
            .get("options")
            .and_then(|options| options.get("relay"))
            .and_then(|relay| relay.get("final"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn build_api_relay_fragment(
    node_name: &str,
    parsed: &ParsedHttpMessage,
    raw_payload: Value,
    auth_match: &AuthMatch,
) -> RelayFragment {
    let content_text = parsed
        .payload
        .get("content")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    RelayFragment {
        relay_key: api_relay_key(
            node_name,
            parsed.io_context.conversation.id.as_str(),
            parsed.identity_input.external_id.as_str(),
        ),
        fragment_id: parsed.io_context.message.id.clone(),
        received_at_ms: now_epoch_ms(),
        content_text,
        attachments: Vec::new(),
        raw_payload: Some(serde_json::json!({
            "request": raw_payload,
            "auth_key_id": auth_match.key_id.clone(),
        })),
        io_context: parsed.io_context.clone(),
        identity_input: parsed.identity_input.clone(),
        flush_hints: RelayFlushHints {
            final_fragment: parsed.relay_final,
        },
    }
}

async fn dispatch_assembled_turn(
    sender: fluxbee_sdk::NodeSender,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
    inbound: Arc<Mutex<InboundProcessor>>,
    turn: AssembledTurn,
    send_context: &str,
) {
    let outcome = inbound
        .lock()
        .await
        .process_assembled_turn(identity, Some(provisioner), turn)
        .await;
    let _ = send_inbound_outcome(sender, outcome, send_context).await;
}

async fn dispatch_assembled_turn_for_http(
    sender: fluxbee_sdk::NodeSender,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
    inbound: Arc<Mutex<InboundProcessor>>,
    turn: AssembledTurn,
    send_context: &str,
) -> Option<String> {
    let outcome = inbound
        .lock()
        .await
        .process_assembled_turn(identity, Some(provisioner), turn)
        .await;
    send_inbound_outcome(sender, outcome, send_context).await
}

async fn send_inbound_outcome(
    sender: fluxbee_sdk::NodeSender,
    outcome: InboundOutcome,
    send_context: &str,
) -> Option<String> {
    match outcome {
        InboundOutcome::SendNow(msg) => {
            let trace_id = msg.routing.trace_id.clone();
            let has_src_ilk = msg
                .meta
                .src_ilk
                .as_deref()
                .is_some_and(|value| !value.is_empty());
            if let Err(err) = sender.send(msg).await {
                tracing::warn!(error = ?err, %trace_id, context = send_context, "failed to send to router");
                None
            } else {
                tracing::debug!(%trace_id, has_src_ilk, context = send_context, "sent to router");
                Some(trace_id)
            }
        }
        InboundOutcome::DroppedDuplicate => {
            tracing::debug!(context = send_context, "dedup hit; dropping inbound");
            None
        }
    }
}

fn accepted_response(
    node_name: &str,
    request_id: String,
    trace_id: Option<String>,
    relay_status: &str,
) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "accepted",
            "request_id": request_id,
            "trace_id": trace_id,
            "relay_status": relay_status,
            "node_name": node_name,
        })),
    )
        .into_response()
}

fn api_error(
    status: StatusCode,
    error_code: &'static str,
    error_message: impl Into<String>,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "status": "error",
            "error_code": error_code,
            "error_message": error_message.into(),
        })),
    )
        .into_response()
}

fn prepare_runtime_api_config(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<(Value, ApiAuthRegistry)> {
    let sanitized = materialize_inline_api_keys(node_name, effective, secret_root)?;
    let registry = load_runtime_api_registry(node_name, &sanitized, secret_root)?;
    Ok((sanitized, registry))
}

fn materialize_inline_api_keys(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<Value> {
    let mut sanitized = effective.clone();
    let api_keys = sanitized
        .get_mut("auth")
        .and_then(Value::as_object_mut)
        .and_then(|auth| auth.get_mut("api_keys"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("missing config.auth.api_keys"))?;

    let mut secret_record = load_or_default_secret_record(node_name, secret_root)?;
    let mut secrets_changed = false;

    for entry in api_keys {
        let Some(entry_obj) = entry.as_object_mut() else {
            return Err(anyhow::anyhow!(
                "config.auth.api_keys[] entries must be objects"
            ));
        };
        let key_id = entry_obj
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].key_id is required"))?;
        let inline_token = entry_obj
            .get("token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let Some(token) = inline_token else {
            continue;
        };

        let storage_key = api_key_storage_key(key_id);
        secret_record
            .secrets
            .insert(storage_key.clone(), Value::String(token));
        entry_obj.remove("token");
        entry_obj.insert(
            "token_ref".to_string(),
            Value::String(format!("local_file:{storage_key}")),
        );
        secrets_changed = true;
    }

    if secrets_changed {
        let record =
            build_node_secret_record(secret_record.secrets, &NodeSecretWriteOptions::default());
        save_secret_record(node_name, secret_root, &record)?;
    }

    Ok(sanitized)
}

fn load_runtime_api_registry(
    node_name: &str,
    effective: &Value,
    secret_root: Option<&Path>,
) -> Result<ApiAuthRegistry> {
    let api_keys = effective
        .get("auth")
        .and_then(|auth| auth.get("api_keys"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing config.auth.api_keys"))?;
    let mut registry = ApiAuthRegistry::default();
    let mut secret_record: Option<NodeSecretRecord> = None;

    for entry in api_keys {
        let Some(entry_obj) = entry.as_object() else {
            return Err(anyhow::anyhow!(
                "config.auth.api_keys[] entries must be objects"
            ));
        };
        let key_id = entry_obj
            .get("key_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[].key_id is required"))?;
        let token = resolve_api_key_token(node_name, entry_obj, &mut secret_record, secret_root)?;
        registry.keys.push(ApiKeyRuntime {
            key_id: key_id.to_string(),
            token,
            caller_identity: entry_obj.get("caller_identity").cloned(),
        });
    }

    Ok(registry)
}

fn resolve_api_key_token(
    node_name: &str,
    entry: &Map<String, Value>,
    secret_record: &mut Option<NodeSecretRecord>,
    secret_root: Option<&Path>,
) -> Result<String> {
    if let Some(token) = entry
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(token.to_string());
    }

    let reference = entry
        .get("token_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("config.auth.api_keys[] requires token or token_ref"))?;

    if let Some(var) = reference.strip_prefix("env:") {
        return env(var)
            .ok_or_else(|| anyhow::anyhow!("unresolved env secret reference '{reference}'"));
    }

    if let Some(storage_key) = reference.strip_prefix("local_file:") {
        if secret_record.is_none() {
            *secret_record = Some(load_or_default_secret_record(node_name, secret_root)?);
        }
        return secret_record
            .as_ref()
            .and_then(|record| record.secrets.get(storage_key))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("missing local secret '{storage_key}'"));
    }

    Err(anyhow::anyhow!(
        "unsupported secret reference '{reference}' for config.auth.api_keys[].token_ref"
    ))
}

fn api_relay_key(node_name: &str, conversation_id: &str, external_user_id: &str) -> String {
    format!("api:{node_name}:{conversation_id}:{external_user_id}")
}

fn api_relay_policy(config: &Config, effective_config: Option<&Value>) -> Result<RelayPolicy> {
    let relay_cfg = extract_runtime_relay_config(effective_config, &config.relay)?;
    api_relay_policy_from_config(&relay_cfg)
}

fn api_relay_policy_from_config(relay_cfg: &ApiRelayConfig) -> Result<RelayPolicy> {
    let mut policy = RelayPolicy {
        enabled: relay_cfg.window_ms > 0,
        relay_window_ms: relay_cfg.window_ms,
        max_open_sessions: relay_cfg.max_open_sessions,
        max_fragments_per_session: relay_cfg.max_fragments_per_session,
        max_bytes_per_session: relay_cfg.max_bytes_per_session,
        ..RelayPolicy::default()
    };
    if policy.relay_window_ms == 0 {
        policy.enabled = false;
        policy.stale_session_ttl_ms = 0;
    } else {
        policy.stale_session_ttl_ms = policy
            .relay_window_ms
            .saturating_mul(4)
            .max(policy.relay_window_ms);
    }
    policy.validate().map_err(|err| anyhow::anyhow!(err))?;
    Ok(policy)
}

fn extract_runtime_relay_config(
    effective_config: Option<&Value>,
    defaults: &ApiRelayConfig,
) -> Result<ApiRelayConfig> {
    let Some(relay) = effective_config
        .and_then(|cfg| cfg.get("io"))
        .and_then(|io| io.get("relay"))
        .and_then(Value::as_object)
    else {
        return Ok(defaults.clone());
    };
    Ok(ApiRelayConfig {
        window_ms: relay
            .get("window_ms")
            .and_then(Value::as_u64)
            .unwrap_or(defaults.window_ms),
        max_open_sessions: relay
            .get("max_open_sessions")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_open_sessions),
        max_fragments_per_session: relay
            .get("max_fragments_per_session")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_fragments_per_session),
        max_bytes_per_session: relay
            .get("max_bytes_per_session")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_bytes_per_session),
    })
}

fn extract_runtime_dst_node(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|cfg| cfg.get("io"))
        .and_then(|io| io.get("dst_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn load_or_default_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
) -> Result<NodeSecretRecord> {
    match load_secret_record(node_name, secret_root) {
        Ok(record) => Ok(record),
        Err(NodeSecretError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(NodeSecretRecord::default())
        }
        Err(err) => Err(err.into()),
    }
}

fn load_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
) -> Result<NodeSecretRecord, NodeSecretError> {
    match secret_root {
        Some(root) => load_node_secret_record_with_root(node_name, root),
        None => load_node_secret_record(node_name),
    }
}

fn save_secret_record(
    node_name: &str,
    secret_root: Option<&Path>,
    record: &NodeSecretRecord,
) -> Result<()> {
    match secret_root {
        Some(root) => {
            save_node_secret_record_with_root(node_name, root, record)?;
        }
        None => {
            save_node_secret_record(node_name, record)?;
        }
    }
    Ok(())
}

fn load_spawn_config(node_name: &str) -> Result<SpawnConfig> {
    let path = managed_node_config_path(node_name)
        .map_err(|err| anyhow::anyhow!("failed to resolve managed config path: {err}"))?;
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        anyhow::anyhow!(
            "failed to read managed config file {}: {err}",
            path.display()
        )
    })?;
    let doc = serde_json::from_str::<Value>(&raw).map_err(|err| {
        anyhow::anyhow!(
            "failed to parse managed config JSON {}: {err}",
            path.display()
        )
    })?;
    Ok(SpawnConfig { path, doc })
}

fn hive_from_node_name(node_name: &str) -> Option<String> {
    node_name
        .split_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn json_get_string_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<String> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(value) = json_get_path(doc, path).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn json_get_u64_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<u64> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(value) = json_get_path(doc, path) {
            if let Some(number) = value.as_u64() {
                return Some(number);
            }
            if let Some(text) = value.as_str() {
                if let Ok(number) = text.parse::<u64>() {
                    return Some(number);
                }
            }
        }
    }
    None
}

fn json_get_u16_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<u16> {
    json_get_u64_opt(doc, dotted_paths).and_then(|value| u16::try_from(value).ok())
}

fn json_get_usize_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<usize> {
    json_get_u64_opt(doc, dotted_paths).and_then(|value| usize::try_from(value).ok())
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn is_control_plane_msg_type(msg_type: &str) -> bool {
    msg_type.eq_ignore_ascii_case(SYSTEM_KIND) || msg_type.eq_ignore_ascii_case("admin")
}

fn section_changed(
    previous_effective: Option<&Value>,
    current_effective: &Value,
    path: &[&str],
) -> bool {
    value_at_path(previous_effective, path) != value_at_path(Some(current_effective), path)
}

fn value_at_path<'a>(root: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    let mut current = root?;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn extract_subject_mode(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|cfg| cfg.get("ingress"))
        .and_then(|ingress| ingress.get("subject_mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn redact_state(
    state: &IoControlPlaneState,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> IoControlPlaneState {
    let mut redacted = state.clone();
    redacted.effective_config = state
        .effective_config
        .as_ref()
        .map(|cfg| adapter_contract.redact_effective_config(cfg));
    redacted
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

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_millis())
            .unwrap_or(0);
        let root =
            std::env::temp_dir().join(format!("io-api-{label}-{}-{now_ms}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn materialize_inline_api_key_moves_secret_to_local_file_ref() {
        let root = temp_root("materialize");
        let config = serde_json::json!({
            "auth": {
                "api_keys": [
                    {
                        "key_id": "partner-1",
                        "token": "secret-1"
                    }
                ]
            }
        });

        let sanitized =
            materialize_inline_api_keys("IO.api.demo@motherbee", &config, Some(root.as_path()))
                .expect("materialize");

        let entry = sanitized
            .get("auth")
            .and_then(|auth| auth.get("api_keys"))
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_object)
            .expect("entry");
        assert!(entry.get("token").is_none());
        assert_eq!(
            entry.get("token_ref").and_then(Value::as_str),
            Some("local_file:io_api_key__partner-1")
        );

        let record =
            load_node_secret_record_with_root("IO.api.demo@motherbee", &root).expect("load record");
        assert_eq!(
            record.secrets.get("io_api_key__partner-1"),
            Some(&Value::String("secret-1".to_string()))
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_runtime_api_registry_resolves_local_file_secret() {
        let root = temp_root("registry");
        let mut secrets = Map::new();
        secrets.insert(
            "io_api_key__partner_1".to_string(),
            Value::String("secret-2".to_string()),
        );
        let record = build_node_secret_record(secrets, &NodeSecretWriteOptions::default());
        save_node_secret_record_with_root("IO.api.demo@motherbee", &root, &record)
            .expect("save record");

        let config = serde_json::json!({
            "auth": {
                "api_keys": [
                    {
                        "key_id": "partner 1",
                        "token_ref": "local_file:io_api_key__partner_1",
                        "caller_identity": {
                            "external_user_id": "caller-1"
                        }
                    }
                ]
            }
        });

        let registry =
            load_runtime_api_registry("IO.api.demo@motherbee", &config, Some(root.as_path()))
                .expect("registry");

        assert_eq!(registry.keys.len(), 1);
        assert_eq!(registry.keys[0].key_id, "partner 1");
        assert_eq!(registry.keys[0].token, "secret-2");
        assert_eq!(
            registry.keys[0]
                .caller_identity
                .as_ref()
                .and_then(|value| value.get("external_user_id"))
                .and_then(Value::as_str),
            Some("caller-1")
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
