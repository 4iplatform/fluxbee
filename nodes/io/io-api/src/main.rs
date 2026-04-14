#![forbid(unsafe_code)]

mod attachments;
mod auth;
mod config;
mod http;
mod schema;
mod subject;

use anyhow::Result;
use axum::body::to_bytes;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fluxbee_sdk::identity::tenant_exists_in_hive_id;
use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{connect, try_handle_default_node_status, NodeConfig, NodeUuidMode};
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_api_adapter_config::IoApiAdapterConfigContract;
use io_common::io_context::IoContext;
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
use io_common::io_control_plane_store::persist_io_control_plane_state;
use io_common::provision::strict_provision_ilk;
use io_common::provision::{FluxbeeIdentityProvisioner, IdentityProvisionConfig, RouterInbox};
use io_common::relay::{
    AssembledTurn, InMemoryRelayStore, RelayBuffer, RelayDecision, RelayFlushHints, RelayFragment,
};
use io_common::text_v1_blob::{
    build_text_v1_inbound_payload, IoBlobRuntimeConfig, IoTextBlobConfig,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};

use attachments::{
    collect_multipart_blob_attachments, effective_blob_payload_cfg, map_blob_error_to_http,
};
use auth::{authenticate_bearer, extract_bearer_token, prepare_runtime_api_config};
use config::{
    api_relay_policy, api_relay_policy_from_config, extract_runtime_dst_node,
    extract_runtime_listen_addr, extract_runtime_relay_config,
};
use http::{accepted_response, api_error};
use schema::{
    build_configured_schema, build_unconfigured_schema, extract_accepted_content_types,
    extract_max_request_bytes, extract_subject_mode, lifecycle_status,
};
use subject::{api_relay_key, parse_json_message_request, ExplicitSubjectMode};

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
    blob_runtime: IoBlobRuntimeConfig,
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
    tenant_id: String,
    token: String,
    caller_identity: Option<Value>,
}

#[derive(Clone)]
struct HttpState {
    hive_id: String,
    node_name: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    auth_registry: Arc<RwLock<ApiAuthRegistry>>,
    sender: fluxbee_sdk::NodeSender,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    router_inbox: Arc<Mutex<RouterInbox>>,
    identity_provision_cfg: IdentityProvisionConfig,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
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
    tenant_id: String,
    caller_identity: Option<Value>,
}

struct SpawnConfig {
    path: PathBuf,
    doc: Value,
}

#[derive(Debug)]
struct ParsedHttpMessage {
    request_id: String,
    identity_input: ResolveOrCreateInput,
    io_context: IoContext,
    payload: Value,
    relay_final: bool,
    explicit_subject_mode: Option<subject::ExplicitSubjectMode>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = Config::from_env()?;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,io_api=debug,fluxbee_sdk=info")
    });
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

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
    let boot_listen_addr =
        extract_runtime_listen_addr(boot_state.effective_config.as_ref(), &config.listen_addr);
    if boot_listen_addr != config.listen_addr {
        tracing::info!(
            node_name = %config.node_name,
            previous_listen = %config.listen_addr,
            effective_listen = %boot_listen_addr,
            config_source = %boot_state.config_source.as_str(),
            "io-api boot listen overridden from effective config"
        );
        config.listen_addr = boot_listen_addr;
    }

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
    let identity_provision_cfg = IdentityProvisionConfig {
        target: config.identity_target.clone(),
        timeout: Duration::from_millis(config.identity_timeout_ms),
    };
    let provisioner: Arc<dyn IdentityProvisioner> = Arc::new(FluxbeeIdentityProvisioner::new(
        sender.clone(),
        inbox.clone(),
        identity_provision_cfg.clone(),
    ));

    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoApiAdapterConfigContract);
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
    let blob_toolkit = Arc::new(
        config
            .blob_runtime
            .build_toolkit()
            .map_err(|err| anyhow::anyhow!("invalid blob runtime config: {err}"))?,
    );
    let blob_payload_cfg = config.blob_runtime.text_v1.clone();

    let control_plane = Arc::new(RwLock::new(boot_state.clone()));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    ));
    let control_src = sender.full_name().to_string();

    let http_state = Arc::new(HttpState {
        hive_id: config.island_id.clone(),
        node_name: config.node_name.clone(),
        control_plane: control_plane.clone(),
        adapter_contract: adapter_contract.clone(),
        auth_registry: auth_registry.clone(),
        sender: sender.clone(),
        identity: identity.clone(),
        provisioner: provisioner.clone(),
        router_inbox: inbox.clone(),
        identity_provision_cfg: identity_provision_cfg.clone(),
        inbound: inbound.clone(),
        relay: relay.clone(),
        blob_toolkit,
        blob_payload_cfg,
    });

    let http_task = match try_bind_http_listener(
        &config.node_name,
        &config.listen_addr,
        control_plane.clone(),
        control_metrics.clone(),
    )
    .await
    {
        Some(http_listener) => Some(tokio::spawn(run_http_server(
            http_listener,
            http_state.clone(),
        ))),
        None => None,
    };
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

    if let Some(http_task) = http_task {
        let _ = tokio::join!(http_task, relay_flush_task, control_task);
    } else {
        let _ = tokio::join!(relay_flush_task, control_task);
    }
    Ok(())
}

async fn run_http_server(listener: TcpListener, state: Arc<HttpState>) -> Result<()> {
    let app = Router::new()
        .route("/", get(get_schema).post(post_messages))
        .route("/schema", get(get_schema))
        .route("/messages", post(post_messages))
        .with_state(state);
    tracing::info!(addr = %listener.local_addr()?, "io-api http listener ready");
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
                "io-api http listener bind failed; node stays alive for control-plane recovery"
            );
            None
        }
    }
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
        build_unconfigured_schema(&state.node_name, &snapshot, state.adapter_contract.as_ref())
    };

    (StatusCode::OK, Json(body)).into_response()
}

async fn post_messages(State(state): State<Arc<HttpState>>, request: Request) -> Response {
    let request_path = request.uri().path().to_string();
    let headers = request.headers().clone();

    let state_snapshot = state.control_plane.read().await.clone();
    if state_snapshot.current_state != IoNodeLifecycleState::Configured
        || state_snapshot.effective_config.is_none()
    {
        tracing::warn!(
            node_name = %state.node_name,
            path = %request_path,
            lifecycle_state = lifecycle_status(&state_snapshot.current_state),
            "io-api request rejected: node not configured"
        );
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
            tracing::warn!(
                node_name = %state.node_name,
                path = %request_path,
                "io-api request rejected: missing bearer token"
            );
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing bearer token",
            );
        }
    };
    let auth_match = match authenticate_bearer(&state.auth_registry, bearer.as_str()).await {
        Some(value) => value,
        None => {
            tracing::warn!(
                node_name = %state.node_name,
                path = %request_path,
                "io-api request rejected: invalid bearer token"
            );
            return api_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Invalid bearer token",
            );
        }
    };
    if let Err((status, code, message)) =
        validate_authenticated_tenant(state.hive_id.as_str(), auth_match.tenant_id.as_str())
    {
        tracing::warn!(
            node_name = %state.node_name,
            path = %request_path,
            key_id = %auth_match.key_id,
            tenant_id = %auth_match.tenant_id,
            error_code = code,
            "io-api request rejected: authenticated key tenant is invalid"
        );
        return api_error(status, code, message);
    }

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string();
    tracing::debug!(
        node_name = %state.node_name,
        path = %request_path,
        key_id = %auth_match.key_id,
        tenant_id = %auth_match.tenant_id,
        content_type = %content_type,
        "io-api request authenticated"
    );
    let accepted_content_types = extract_accepted_content_types(Some(effective));
    if !content_type_matches_any(content_type.as_str(), &accepted_content_types) {
        tracing::debug!(
            node_name = %state.node_name,
            path = %request_path,
            content_type = %content_type,
            accepted = ?accepted_content_types,
            "io-api request rejected: unsupported content type"
        );
        return api_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Request content type is not accepted by this IO.api instance",
        );
    }
    if content_type.starts_with("multipart/form-data") {
        let mut multipart = match Multipart::from_request(request, &()).await {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!(
                    node_name = %state.node_name,
                    path = %request_path,
                    error = %err,
                    "io-api request rejected: invalid multipart envelope"
                );
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_multipart",
                    format!("Invalid multipart request: {err}"),
                );
            }
        };
        let blob_cfg = effective_blob_payload_cfg(Some(effective), &state.blob_payload_cfg);
        let (metadata, attachments) = match collect_multipart_blob_attachments(
            &mut multipart,
            state.blob_toolkit.as_ref(),
            &blob_cfg,
        )
        .await
        {
            Ok(value) => value,
            Err((status, code, message)) => return api_error(status, code, message),
        };
        tracing::debug!(
            node_name = %state.node_name,
            path = %request_path,
            key_id = %auth_match.key_id,
            attachment_count = attachments.len(),
            "io-api multipart payload accepted"
        );
        let parsed = match parse_json_message_request(
            &metadata,
            effective,
            &auth_match,
            !attachments.is_empty(),
        ) {
            Ok(value) => value,
            Err((status, code, message)) => return api_error(status, code, message),
        };
        let response_ilk = match parsed.explicit_subject_mode.as_ref() {
            Some(ExplicitSubjectMode::ByIlk { .. }) => {
                tracing::info!(
                    node_name = %state.node_name,
                    path = %request_path,
                    "io-api explicit_subject by_ilk requested but not implemented yet"
                );
                return api_error(
                    StatusCode::NOT_IMPLEMENTED,
                    "subject_ilk_not_implemented",
                    "explicit_subject by_ilk is not implemented yet in IO.api",
                );
            }
            Some(ExplicitSubjectMode::ByData) => {
                match resolve_explicit_subject_ilk_for_http(state.as_ref(), &parsed.identity_input)
                    .await
                {
                    Ok(src_ilk) => Some(src_ilk),
                    Err((status, code, message)) => return api_error(status, code, message),
                }
            }
            None => None,
        };
        let text = parsed
            .payload
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");
        let payload = match build_text_v1_inbound_payload(
            state.blob_toolkit.as_ref(),
            &blob_cfg,
            text,
            attachments,
        ) {
            Ok(value) => value,
            Err(err) => {
                let (status, code, message) = map_blob_error_to_http(&err);
                return api_error(status, code, message);
            }
        };
        let parsed = ParsedHttpMessage { payload, ..parsed };
        let relay_fragment =
            build_api_relay_fragment(&state.node_name, &parsed, metadata.clone(), &auth_match);

        return match state.relay.lock().await.handle_fragment(relay_fragment) {
            RelayDecision::Hold => accepted_response(
                &state.node_name,
                parsed.request_id,
                None,
                "held",
                response_ilk.as_deref(),
            ),
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
                    response_ilk.as_deref(),
                )
            }
            RelayDecision::DropDuplicate => accepted_response(
                &state.node_name,
                parsed.request_id,
                None,
                "duplicate_dropped",
                response_ilk.as_deref(),
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
                    response_ilk.as_deref(),
                )
            }
            RelayDecision::DropExpired => api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "relay_unavailable",
                "Relay session expired before request could be accepted",
            ),
        };
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
            tracing::debug!(
                node_name = %state.node_name,
                path = %request_path,
                error = %err,
                "io-api request rejected: invalid json"
            );
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                format!("Request body is not valid JSON: {err}"),
            );
        }
    };

    let parsed = match parse_json_message_request(&envelope, effective, &auth_match, false) {
        Ok(value) => value,
        Err((status, code, message)) => return api_error(status, code, message),
    };
    let response_ilk = match parsed.explicit_subject_mode.as_ref() {
        Some(ExplicitSubjectMode::ByIlk { .. }) => {
            tracing::info!(
                node_name = %state.node_name,
                path = %request_path,
                "io-api explicit_subject by_ilk requested but not implemented yet"
            );
            return api_error(
                StatusCode::NOT_IMPLEMENTED,
                "subject_ilk_not_implemented",
                "explicit_subject by_ilk is not implemented yet in IO.api",
            );
        }
        Some(ExplicitSubjectMode::ByData) => {
            match resolve_explicit_subject_ilk_for_http(state.as_ref(), &parsed.identity_input)
                .await
            {
                Ok(src_ilk) => Some(src_ilk),
                Err((status, code, message)) => return api_error(status, code, message),
            }
        }
        None => None,
    };
    tracing::debug!(
        node_name = %state.node_name,
        path = %request_path,
        key_id = %auth_match.key_id,
        tenant_id = %auth_match.tenant_id,
        subject_mode = extract_subject_mode(Some(effective)).unwrap_or_else(|| "unknown".to_string()),
        relay_final = parsed.relay_final,
        "io-api json payload accepted"
    );

    let relay_fragment =
        build_api_relay_fragment(&state.node_name, &parsed, envelope.clone(), &auth_match);

    match state.relay.lock().await.handle_fragment(relay_fragment) {
        RelayDecision::Hold => accepted_response(
            &state.node_name,
            parsed.request_id,
            None,
            "held",
            response_ilk.as_deref(),
        ),
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
                response_ilk.as_deref(),
            )
        }
        RelayDecision::DropDuplicate => accepted_response(
            &state.node_name,
            parsed.request_id,
            None,
            "duplicate_dropped",
            response_ilk.as_deref(),
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
                response_ilk.as_deref(),
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
        .and_then(|relay_cfg| api_relay_policy_from_config(&relay_cfg))
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

fn content_type_matches_any(content_type: &str, accepted: &[String]) -> bool {
    accepted
        .iter()
        .any(|candidate| content_type.starts_with(candidate))
}

async fn resolve_explicit_subject_ilk_for_http(
    state: &HttpState,
    identity_input: &ResolveOrCreateInput,
) -> std::result::Result<String, (StatusCode, &'static str, String)> {
    match state
        .identity
        .lookup(&identity_input.channel, &identity_input.external_id)
    {
        Ok(Some(src_ilk)) => {
            tracing::info!(
                channel = %identity_input.channel,
                external_id = %identity_input.external_id,
                src_ilk = %src_ilk,
                "io-api explicit_subject identity lookup hit"
            );
            state.identity.remember(
                &identity_input.channel,
                &identity_input.external_id,
                &src_ilk,
            );
            return Ok(src_ilk);
        }
        Ok(None) => {
            tracing::debug!(
                channel = %identity_input.channel,
                external_id = %identity_input.external_id,
                "io-api explicit_subject identity lookup miss"
            );
        }
        Err(err) => {
            tracing::warn!(
                error = ?err,
                channel = %identity_input.channel,
                external_id = %identity_input.external_id,
                "io-api explicit_subject identity lookup error; trying strict provision"
            );
        }
    }

    let src_ilk = strict_provision_ilk(
        &state.sender,
        state.router_inbox.clone(),
        &state.identity_provision_cfg,
        state.identity_provision_cfg.target.as_str(),
        identity_input,
    )
    .await
    .map_err(map_identity_error_to_http)?;

    tracing::info!(
        channel = %identity_input.channel,
        external_id = %identity_input.external_id,
        src_ilk = %src_ilk,
        "io-api explicit_subject identity provisioned"
    );
    state.identity.remember(
        &identity_input.channel,
        &identity_input.external_id,
        &src_ilk,
    );
    Ok(src_ilk)
}

fn map_identity_error_to_http(
    err: io_common::identity::IdentityError,
) -> (StatusCode, &'static str, String) {
    match err {
        io_common::identity::IdentityError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            "identity_timeout",
            "Identity did not respond in time".to_string(),
        ),
        io_common::identity::IdentityError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_unavailable",
            "Identity is currently unavailable".to_string(),
        ),
        io_common::identity::IdentityError::Miss => (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_unavailable",
            "Identity did not return a usable result".to_string(),
        ),
        io_common::identity::IdentityError::Other(message) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_unavailable",
            format!("Identity request failed: {message}"),
        ),
    }
}

fn validate_authenticated_tenant(
    hive_id: &str,
    tenant_id: &str,
) -> std::result::Result<(), (StatusCode, &'static str, String)> {
    match tenant_exists_in_hive_id(hive_id, tenant_id) {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::FORBIDDEN,
            "tenant_not_found",
            format!("Authenticated API key tenant '{tenant_id}' does not exist"),
        )),
        Err(err) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "identity_unavailable",
            format!("Unable to validate authenticated API key tenant: {err}"),
        )),
    }
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
    let attachments = parsed
        .payload
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    RelayFragment {
        relay_key: api_relay_key(
            node_name,
            parsed.io_context.conversation.id.as_str(),
            parsed.identity_input.external_id.as_str(),
        ),
        fragment_id: parsed.io_context.message.id.clone(),
        received_at_ms: now_epoch_ms(),
        content_text,
        attachments,
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

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{load_runtime_api_registry, materialize_inline_api_keys};
    use axum::http::header::AUTHORIZATION;
    use fluxbee_sdk::{
        build_node_secret_record, load_node_secret_record_with_root,
        save_node_secret_record_with_root, NodeSecretWriteOptions,
    };
    use fluxbee_sdk::{compute_thread_id, ThreadIdInput};
    use serde_json::Map;

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
                        "tenant_id": "tnt:partner1",
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
        assert_eq!(registry.keys[0].tenant_id, "tnt:partner1");
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

    fn configured_effective(subject_mode: &str) -> Value {
        serde_json::json!({
            "listen": {
                "address": "127.0.0.1",
                "port": 8080
            },
            "auth": {
                "mode": "api_key",
                "api_keys": [
                    {
                        "key_id": "partner1",
                        "tenant_id": "tnt:partner1",
                        "token_ref": "local_file:io_api_key__partner1",
                        "caller_identity": {
                            "external_user_id": "caller-1",
                            "display_name": "Caller One"
                        }
                    }
                ]
            },
            "ingress": {
                "subject_mode": subject_mode,
                "accepted_content_types": ["application/json"]
            },
            "io": {
                "dst_node": "resolve"
            }
        })
    }

    #[test]
    fn extract_bearer_token_parses_authorization_header() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer secret-token"),
        );
        assert_eq!(
            extract_bearer_token(&headers).as_deref(),
            Some("secret-token")
        );
    }

    #[tokio::test]
    async fn authenticate_bearer_matches_registry_token() {
        let registry = Arc::new(RwLock::new(ApiAuthRegistry {
            keys: vec![ApiKeyRuntime {
                key_id: "partner1".to_string(),
                tenant_id: "tnt:partner1".to_string(),
                token: "secret-token".to_string(),
                caller_identity: Some(serde_json::json!({
                    "external_user_id": "caller-1"
                })),
            }],
        }));

        let auth = authenticate_bearer(&registry, "secret-token")
            .await
            .expect("auth match");
        assert_eq!(auth.key_id, "partner1");
        assert_eq!(auth.tenant_id, "tnt:partner1");
        assert_eq!(
            auth.caller_identity
                .as_ref()
                .and_then(|value| value.get("external_user_id"))
                .and_then(Value::as_str),
            Some("caller-1")
        );
    }

    #[test]
    fn build_unconfigured_schema_reports_required_configuration() {
        let state = IoControlPlaneState {
            current_state: IoNodeLifecycleState::FailedConfig,
            last_error: Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: "missing auth".to_string(),
            }),
            ..IoControlPlaneState::default()
        };
        let schema =
            build_unconfigured_schema("IO.api.demo@motherbee", &state, &IoApiAdapterConfigContract);

        assert_eq!(
            schema.get("status").and_then(Value::as_str),
            Some("failed_config")
        );
        assert_eq!(
            schema.get("runtime").and_then(Value::as_str),
            Some("IO.api")
        );
        assert!(schema
            .get("required_configuration")
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty()));
        assert_eq!(
            schema
                .get("last_error")
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str),
            Some("invalid_config")
        );
    }

    #[test]
    fn build_configured_schema_reflects_effective_contract() {
        let effective = configured_effective("explicit_subject");
        let state = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            effective_config: Some(effective.clone()),
            config_version: 7,
            ..IoControlPlaneState::default()
        };

        let schema = build_configured_schema(
            "IO.api.demo@motherbee",
            &state,
            &effective,
            &IoApiAdapterConfigContract,
            1,
        );

        assert_eq!(
            schema.get("status").and_then(Value::as_str),
            Some("configured")
        );
        assert_eq!(
            schema
                .get("ingress")
                .and_then(|value| value.get("subject_mode"))
                .and_then(Value::as_str),
            Some("explicit_subject")
        );
        assert_eq!(
            schema
                .get("auth")
                .and_then(|value| value.get("active_key_count"))
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            schema
                .get("relay")
                .and_then(|value| value.get("config_path"))
                .and_then(Value::as_str),
            Some("config.io.relay.*")
        );
    }

    #[test]
    fn parse_json_message_request_accepts_explicit_subject() {
        let effective = configured_effective("explicit_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: None,
        };
        let envelope = serde_json::json!({
            "subject": {
                "external_user_id": "crm:123",
                "display_name": "Juan Perez",
                "email": "juan@example.com",
                "company_name": "ACME Support",
                "attributes": {
                    "crm_customer_id": "crm-123"
                }
            },
            "message": {
                "text": "hola",
                "external_message_id": "crm-msg-1"
            },
            "options": {
                "relay": {
                    "final": true
                },
                "metadata": {
                    "conversation_id": "conv-1"
                }
            }
        });

        let parsed =
            parse_json_message_request(&envelope, &effective, &auth_match, false).expect("parsed");
        assert_eq!(parsed.identity_input.channel, "api");
        assert_eq!(parsed.identity_input.external_id, "crm:123");
        assert_eq!(
            parsed.identity_input.tenant_id.as_deref(),
            Some("tnt:partner1")
        );
        assert_eq!(parsed.identity_input.tenant_hint, None);
        assert_eq!(
            parsed
                .identity_input
                .attributes
                .get("company_name")
                .and_then(Value::as_str),
            Some("ACME Support")
        );
        assert_eq!(
            parsed
                .identity_input
                .attributes
                .get("crm_customer_id")
                .and_then(Value::as_str),
            Some("crm-123")
        );
        assert_eq!(parsed.io_context.conversation.id, "conv-1");
        assert_eq!(parsed.io_context.message.id, "crm-msg-1");
        assert_eq!(parsed.io_context.channel, "api");
        assert_eq!(parsed.io_context.sender.id, "crm:123");
        assert_eq!(parsed.io_context.entrypoint.id, "127.0.0.1");
        assert_eq!(
            parsed.io_context.conversation.thread_id.as_deref(),
            Some(
                compute_thread_id(ThreadIdInput::PersistentChannel {
                    channel_type: "api",
                    entrypoint_id: Some("127.0.0.1"),
                    conversation_id: "conv-1",
                })
                .expect("thread id")
                .as_str()
            )
        );
        assert!(parsed.relay_final);
        assert_eq!(
            parsed.explicit_subject_mode,
            Some(ExplicitSubjectMode::ByData)
        );
    }

    #[test]
    fn parse_json_message_request_accepts_explicit_subject_by_ilk() {
        let effective = configured_effective("explicit_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: None,
        };
        let envelope = serde_json::json!({
            "subject": {
                "ilk": "ilk:abc-123",
                "display_name": "Ignored Name",
                "email": "ignored@example.com"
            },
            "message": {
                "text": "hola"
            }
        });

        let parsed =
            parse_json_message_request(&envelope, &effective, &auth_match, false).expect("parsed");
        assert_eq!(parsed.identity_input.external_id, "ilk:abc-123");
        assert_eq!(
            parsed.identity_input.tenant_id.as_deref(),
            Some("tnt:partner1")
        );
        assert_eq!(
            parsed.explicit_subject_mode,
            Some(ExplicitSubjectMode::ByIlk {
                ilk: "ilk:abc-123".to_string()
            })
        );
    }

    #[test]
    fn parse_json_message_request_rejects_incomplete_explicit_subject_by_data() {
        let effective = configured_effective("explicit_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: None,
        };
        let envelope = serde_json::json!({
            "subject": {
                "external_user_id": "crm:123",
                "display_name": "Juan Perez"
            },
            "message": {
                "text": "hola"
            }
        });

        let err = parse_json_message_request(&envelope, &effective, &auth_match, false)
            .expect_err("must reject incomplete by_data");
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.1, "subject_data_incomplete");
    }

    #[test]
    fn parse_json_message_request_rejects_subject_for_caller_is_subject() {
        let effective = configured_effective("caller_is_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: Some(serde_json::json!({
                "external_user_id": "caller-1"
            })),
        };
        let envelope = serde_json::json!({
            "subject": {
                "external_user_id": "crm:123"
            },
            "message": {
                "text": "hola"
            }
        });

        let err = parse_json_message_request(&envelope, &effective, &auth_match, false)
            .expect_err("must reject subject");
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.1, "invalid_payload");
    }

    #[test]
    fn parse_json_message_request_uses_authenticated_caller() {
        let effective = configured_effective("caller_is_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: Some(serde_json::json!({
                "external_user_id": "caller-1",
                "display_name": "Caller One"
            })),
        };
        let envelope = serde_json::json!({
            "message": {
                "text": "hola"
            }
        });

        let parsed =
            parse_json_message_request(&envelope, &effective, &auth_match, false).expect("parsed");
        assert_eq!(parsed.identity_input.external_id, "caller-1");
        assert_eq!(
            parsed.identity_input.tenant_id.as_deref(),
            Some("tnt:partner1")
        );
        assert_eq!(parsed.identity_input.tenant_hint, None);
        assert_eq!(parsed.io_context.channel, "api");
        assert_eq!(parsed.io_context.sender.id, "caller-1");
        assert_eq!(
            parsed.io_context.conversation.thread_id.as_deref(),
            Some(
                compute_thread_id(ThreadIdInput::PersistentChannel {
                    channel_type: "api",
                    entrypoint_id: Some("127.0.0.1"),
                    conversation_id: "caller-1",
                })
                .expect("thread id")
                .as_str()
            )
        );
    }

    #[test]
    fn parse_json_message_request_allows_empty_text_when_attachments_exist() {
        let effective = configured_effective("explicit_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: None,
        };
        let envelope = serde_json::json!({
            "subject": {
                "external_user_id": "crm:123",
                "display_name": "Juan Perez",
                "email": "juan@example.com"
            },
            "message": {
                "text": ""
            }
        });

        let parsed =
            parse_json_message_request(&envelope, &effective, &auth_match, true).expect("parsed");
        assert_eq!(parsed.identity_input.external_id, "crm:123");
        assert_eq!(
            parsed.explicit_subject_mode,
            Some(ExplicitSubjectMode::ByData)
        );
    }

    #[test]
    fn parse_json_message_request_rejects_subject_tenant_hint() {
        let effective = configured_effective("explicit_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: None,
        };
        let envelope = serde_json::json!({
            "subject": {
                "external_user_id": "crm:123",
                "display_name": "Juan Perez",
                "email": "juan@example.com",
                "tenant_hint": "acme"
            },
            "message": {
                "text": "hola"
            }
        });

        let err = parse_json_message_request(&envelope, &effective, &auth_match, false)
            .expect_err("must reject tenant_hint");
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.1, "invalid_payload");
    }

    #[test]
    fn parse_json_message_request_rejects_subject_tenant_id() {
        let effective = configured_effective("explicit_subject");
        let auth_match = AuthMatch {
            key_id: "partner1".to_string(),
            tenant_id: "tnt:partner1".to_string(),
            caller_identity: None,
        };
        let envelope = serde_json::json!({
            "subject": {
                "external_user_id": "crm:123",
                "display_name": "Juan Perez",
                "email": "juan@example.com",
                "tenant_id": "tnt:other"
            },
            "message": {
                "text": "hola"
            }
        });

        let err = parse_json_message_request(&envelope, &effective, &auth_match, false)
            .expect_err("must reject tenant_id");
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.1, "invalid_payload");
    }
}
