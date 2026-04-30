#![forbid(unsafe_code)]

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
    compute_thread_id, connect, resolve_identity_option_from_hive_id,
    try_handle_default_node_status, NodeConfig, NodeUuidMode, ThreadIdInput,
    FLUXBEE_NODE_NAME_ENV,
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
use io_common::provision::{strict_provision_ilk, IdentityProvisionConfig, RouterInbox};
use io_common::router_message::new_trace_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};

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
struct AdapterRuntime {
    adapter_id: String,
    tenant_id: String,
    installation_key: String,
    label: Option<String>,
    dst_node: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct LinkedHelperRuntimeRegistry {
    adapters: HashMap<String, AdapterRuntime>,
    max_request_bytes: usize,
    identity_target: String,
    identity_timeout_ms: u64,
}

#[derive(Clone)]
struct HttpState {
    node_name: String,
    hive_id: String,
    state_dir: PathBuf,
    sender: fluxbee_sdk::NodeSender,
    router_inbox: Arc<Mutex<RouterInbox>>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
}

#[derive(Debug, Deserialize)]
struct PollRequest {
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    adapter_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    items: Vec<PollItem>,
}

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Serialize)]
struct PollResponse {
    response_id: String,
    adapter_id: String,
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

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env();
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,io_linkedhelper=info,fluxbee_sdk=info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        state_dir = %config.state_dir.display(),
        listen_addr = %config.listen_addr,
        "io-linkedhelper starting"
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
        "connected to router"
    );

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

    let runtime_registry = Arc::new(RwLock::new(
        load_runtime_registry(boot_state.effective_config.as_ref()).unwrap_or_else(|err| {
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
        }),
    ));
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
    let inbox = Arc::new(Mutex::new(RouterInbox::new(receiver)));
    let http_state = Arc::new(HttpState {
        node_name: config.node_name.clone(),
        hive_id: hive_id_from_node_name(&config.node_name),
        state_dir: config.state_dir.clone(),
        sender: sender.clone(),
        router_inbox: inbox.clone(),
        control_plane: control_plane.clone(),
        adapter_contract: adapter_contract.clone(),
        runtime_registry: runtime_registry.clone(),
        durable_state: durable_state.clone(),
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
        inbox,
        sender,
        config.node_name.clone(),
        config.state_dir.clone(),
        control_plane,
        control_metrics,
        adapter_contract,
        runtime_registry,
        durable_state,
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

async fn run_router_control_loop(
    inbox: Arc<Mutex<RouterInbox>>,
    sender: fluxbee_sdk::NodeSender,
    node_name: String,
    state_dir: PathBuf,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
) -> Result<()> {
    let control_src = sender.uuid().to_string();
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
            &control_src,
            &state_dir,
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            runtime_registry.clone(),
            durable_state.clone(),
        )
        .await
        {
            sender.send(response).await?;
        }
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
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
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
        let adapter_count = runtime_registry.read().await.adapters.len();
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
                "active_adapter_count": adapter_count
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
                obj.insert(
                    "metrics".to_string(),
                    serde_json::json!({
                        "control_plane": control_metrics.snapshot(),
                        "runtime": { "active_adapter_count": runtime_registry.read().await.adapters.len() }
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
                control_plane.clone(),
                control_metrics,
                adapter_contract,
                runtime_registry,
                durable_state,
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

async fn apply_linkedhelper_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    runtime_registry: Arc<RwLock<LinkedHelperRuntimeRegistry>>,
    durable_state: Arc<RwLock<LinkedHelperDurableState>>,
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
    let apply_hot = vec!["runtime_registry".to_string()];
    let apply_reinit: Vec<String> = Vec::new();
    let apply_restart_required: Vec<String> = Vec::new();
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
    build_io_config_set_ok_payload(node_name, &redacted)
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
    let adapters = effective_config
        .and_then(|cfg| cfg.get("adapters"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let max_request_bytes = effective_config
        .and_then(|cfg| cfg.get("http"))
        .and_then(|http| http.get("max_request_bytes"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(256 * 1024);
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

    let mut map = HashMap::new();
    for adapter in adapters {
        let adapter = adapter
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("config.adapters entries must be objects"))?;
        let adapter_id = adapter
            .get("adapter_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.adapters[].adapter_id is required"))?
            .to_string();
        let tenant_id = adapter
            .get("tenant_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.adapters[].tenant_id is required"))?
            .to_string();
        let installation_key = adapter
            .get("installation_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("config.adapters[].installation_key is required"))?
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
        map.insert(
            adapter_id.clone(),
            AdapterRuntime {
                adapter_id,
                tenant_id,
                installation_key,
                label,
                dst_node,
            },
        );
    }

    Ok(LinkedHelperRuntimeRegistry {
        adapters: map,
        max_request_bytes,
        identity_target,
        identity_timeout_ms,
    })
}

fn runtime_registry_snapshots(registry: &LinkedHelperRuntimeRegistry) -> Vec<AdapterSnapshot> {
    registry
        .adapters
        .values()
        .map(|adapter| AdapterSnapshot {
            adapter_id: adapter.adapter_id.clone(),
            tenant_id: adapter.tenant_id.clone(),
            label: adapter.label.clone(),
            dst_node: adapter.dst_node.clone(),
            installation_key_ref:
                io_common::io_linkedhelper_adapter_config::installation_key_storage_key(
                    &adapter.adapter_id,
                ),
        })
        .collect()
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
    let adapter_count = state.runtime_registry.read().await.adapters.len();

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
    let adapter_id_header = match headers
        .get("X-Fluxbee-Adapter-Id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error_code": "missing_adapter_id",
                    "error_message": "X-Fluxbee-Adapter-Id header is required"
                })),
            )
                .into_response()
        }
    };

    let bearer = match extract_bearer_token(&headers) {
        Some(value) => value,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error_code": "missing_bearer_token",
                    "error_message": "Authorization: Bearer <installation_key> is required"
                })),
            )
                .into_response()
        }
    };

    let registry = state.runtime_registry.read().await;
    let Some(runtime) = registry.adapters.get(adapter_id_header) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error_code": "unknown_adapter",
                "error_message": "adapter_id is not configured"
            })),
        )
            .into_response();
    };
    if runtime.installation_key != bearer {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error_code": "invalid_installation_key",
                "error_message": "invalid installation key"
            })),
        )
            .into_response();
    }
    let runtime = runtime.clone();
    let identity_cfg = build_identity_provision_config(&registry, &state.hive_id);
    drop(registry);

    if request.adapter_id.as_deref().map(str::trim).filter(|v| !v.is_empty()) != Some(adapter_id_header)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error_code": "adapter_id_mismatch",
                "error_message": "adapter_id body/header mismatch"
            })),
        )
            .into_response();
    }

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
        .unwrap_or("heartbeat");
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
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error_code": "invalid_heartbeat_request",
                    "error_message": "heartbeat requests must not include items"
                })),
            )
                .into_response();
        }
        pending_deliveries
    } else if mode.eq_ignore_ascii_case("events") {
        if request.items.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error_code": "empty_events_batch",
                    "error_message": "events requests must include at least one item"
                })),
            )
                .into_response();
        }
        let mut responses = Vec::with_capacity(request.items.len());
        for item in &request.items {
            let event_id = item.id.trim();
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
            if item.item_type.eq_ignore_ascii_case("profile_create") {
                process_profile_create(
                    &state,
                    &runtime,
                    &identity_cfg,
                    &mut durable_state,
                    request_id,
                    item,
                    &mut responses,
                )
                .await;
                continue;
            }
            if item.item_type.eq_ignore_ascii_case("conversation_message") {
                process_conversation_message(
                    &state,
                    &runtime,
                    &identity_cfg,
                    &mut durable_state,
                    request_id,
                    item,
                    &mut responses,
                )
                .await;
                continue;
            }
            responses.push(ResponseItem::Result {
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
        responses.extend(pending_deliveries);
        responses
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error_code": "invalid_mode",
                "error_message": "mode must be 'events' or 'heartbeat'"
            })),
        )
            .into_response();
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
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error_code": "durable_state_unavailable",
                "error_message": "linkedhelper durable state is temporarily unavailable"
            })),
        )
            .into_response();
    }
    *state.durable_state.write().await = durable_state;

    tracing::debug!(
        adapter_id = %runtime.adapter_id,
        adapter_label = %runtime.label.as_deref().unwrap_or(""),
        tenant_id = %runtime.tenant_id,
        dst_node = %runtime.dst_node.as_deref().unwrap_or(""),
        request_id = %request_id,
        mode = %mode,
        item_count = request.items.len(),
        "io-linkedhelper poll processed"
    );

    Json(PollResponse {
        response_id: format!("resp:{request_id}"),
        adapter_id: runtime.adapter_id,
        items,
    })
    .into_response()
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
                if resolved.ilk.registration_status.eq_ignore_ascii_case("complete") =>
            {
                let was_ready = existing.status.eq_ignore_ascii_case("ready");
                durable_state.upsert_profile(
                    &existing.adapter_id,
                    &existing.tenant_id,
                    &existing.external_profile_id,
                    Some(resolved.ilk.ilk_id.clone()),
                    Some(resolved.ich_id.clone()),
                    "ready",
                    existing.display_name.clone(),
                    existing.metadata.clone(),
                );
                if !was_ready {
                    tracing::info!(
                        node_name = %state.node_name,
                        adapter_id = %existing.adapter_id,
                        external_profile_id = %existing.external_profile_id,
                        ilk_id = %resolved.ilk.ilk_id,
                        ich_id = %resolved.ich_id,
                        "linkedhelper profile promoted to ready"
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
                let next_status = if existing.status.eq_ignore_ascii_case("ready") {
                    "ready"
                } else {
                    "pending_promotion"
                };
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
                if resolved.ilk.registration_status.eq_ignore_ascii_case("complete") {
                    "ready"
                } else {
                    "pending_promotion"
                },
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
            if resolved.ilk.registration_status.eq_ignore_ascii_case("complete") {
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
            &state.sender,
            state.router_inbox.clone(),
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
                    "pending_promotion",
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

    if !profile.status.eq_ignore_ascii_case("ready") {
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
            &state.sender,
            state.router_inbox.clone(),
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

    let thread_id = match compute_thread_id(ThreadIdInput::PersistentChannel {
        channel_type: linkedhelper_profile_channel(),
        entrypoint_id: Some(runtime.adapter_id.as_str()),
        conversation_id: conversation_external_id,
    }) {
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
                error_message: Some(err.to_string()),
                retryable: Some(false),
            });
            return;
        }
    };

    let trace_id = new_trace_id();
    let profile_ilk_owned = profile_ilk.to_string();
    let conversation_msg = WireMessage {
        routing: Routing {
            src: state.sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(dst_node.clone()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            src_ilk: Some(contact_src_ilk.clone()),
            dst_ilk: Some(profile_ilk_owned.clone()),
            ich: contact_ich_id.clone(),
            thread_id: Some(thread_id.clone()),
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: Some(serde_json::json!({
                "io": {
                    "channel": linkedhelper_profile_channel(),
                    "adapter": {
                        "id": runtime.adapter_id
                    },
                    "entrypoint": {
                        "id": runtime.adapter_id
                    },
                    "sender": {
                        "id": contact_external_id,
                        "display_name": contact_name
                    },
                    "recipient": {
                        "id": profile_ilk_owned
                    },
                    "conversation": {
                        "id": conversation_external_id,
                        "thread_id": thread_id
                    },
                    "message": {
                        "id": item.id,
                        "request_id": request_id
                    },
                    "linkedhelper": {
                        "external_profile_id": profile.external_profile_id,
                        "contact_lh_person_id": payload.contact_lh_person_id,
                        "profile_ilk": profile_ilk
                    }
                }
            })),
            ..Meta::default()
        },
        payload: message_payload,
    };

    if let Err(err) = state.sender.send(conversation_msg).await {
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
