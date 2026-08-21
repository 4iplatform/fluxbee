#![forbid(unsafe_code)]

//! Edge-native, instanced API ingress for Fluxbee.
//!
//! `IO.api` has no TCP listener. Each managed instance owns one ICH, asks `SY.admin` to
//! externalize that channel on `SY.edge`, and accepts only router-stamped messages from that exact
//! Edge/ICH pair. Public bearer validation stays at Edge; Admin mints and stores the credential
//! through its existing `externalize` action.

mod config;
mod subject;

use anyhow::Result;
use fluxbee_sdk::identity::{
    list_ilks_from_hive_id, resolve_identity_option_from_hive_id, IdentityIlkOption,
};
use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::rpc::AdminCommandRequest;
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    PendingMatcher, RouteMatch, RouteTarget, RouterDispatcher, RpcRequestLabels,
};
use io_common::frontdesk_contract::{
    FrontdeskHandoffPayload, FrontdeskHandoffSubject, FRONTDESK_HANDOFF_PAYLOAD_TYPE,
    FRONTDESK_SCHEMA_VERSION_V1,
};
use io_common::frontdesk_gate::frontdesk_response_contract;
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_api_adapter_config::{
    IoApiAdapterConfigContract, IO_API_CHANNEL_TYPE, IO_API_INBOUND_FAMILY,
};
use io_common::io_context::{parse_structured_response_payload, set_response_envelope};
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
use io_common::provision::{
    ensure_own_ich, set_ich_enabled, strict_provision_ilk, FluxbeeIdentityProvisioner,
    IdentityProvisionConfig,
};
use io_common::relay::{
    AssembledTurn, InMemoryRelayStore, RelayBuffer, RelayDecision, RelayFlushHints, RelayFragment,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use subject::{
    api_relay_key, parse_api_message_request, ApiIngressError, EndpointPrincipal,
    ExplicitSubjectMode, ParsedApiMessage,
};
use tokio::sync::{Mutex, RwLock};
use tracing_subscriber::EnvFilter;

const RPC_CH_INCOMING: &str = "incoming";
const EDGE_METHOD: &str = "POST";
const USER_KIND: &str = "user";

#[derive(Clone)]
struct Config {
    node_name: String,
    hive_id: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    spawn_config_path: PathBuf,
    identity_target: String,
    frontdesk_target: String,
    admin_target: String,
    orchestrator_target: String,
    identity_timeout_ms: u64,
    reconcile_interval_secs: u64,
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

struct SpawnConfig {
    path: PathBuf,
    doc: Value,
}

#[derive(Debug, Clone, Serialize, Default)]
struct PublicationSnapshot {
    status: String,
    ich: Option<String>,
    edge_node: Option<String>,
    url: Option<String>,
    last_error: Option<String>,
    updated_at_ms: u64,
}

#[derive(Default)]
struct PublicationRuntime {
    snapshot: PublicationSnapshot,
    active_channel_id: Option<String>,
    active_ich: Option<String>,
    active_edge_node: Option<String>,
    active_entry_token: Option<String>,
    pending_entry_token: Option<String>,
}

// Manual Debug so the entry-token (a live bearer credential) is NEVER emitted in plaintext
// via a `{:?}` log/format — only its presence is shown. (io.api hardening: latent log-leak.)
impl std::fmt::Debug for PublicationRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |t: &Option<String>| if t.is_some() { "<redacted>" } else { "<none>" };
        f.debug_struct("PublicationRuntime")
            .field("snapshot", &self.snapshot)
            .field("active_channel_id", &self.active_channel_id)
            .field("active_ich", &self.active_ich)
            .field("active_edge_node", &self.active_edge_node)
            .field("active_entry_token", &redact(&self.active_entry_token))
            .field("pending_entry_token", &redact(&self.pending_entry_token))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesiredPublication {
    publish: bool,
    edge_node: String,
    api_channel_id: String,
}

#[derive(Clone)]
struct RuntimeState {
    config: Config,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    dispatcher: Arc<RouterDispatcher>,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    identity_provision_cfg: IdentityProvisionConfig,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    publication: Arc<Mutex<PublicationRuntime>>,
    publication_reconcile: Arc<Mutex<()>>,
}

#[derive(Debug, Clone)]
struct FrontdeskResolvedRequest {
    src_ilk: String,
    payload: FrontdeskHandoffPayload,
    dst_node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FrontdeskApiEnvelope {
    success: bool,
    human_message: String,
    #[serde(default)]
    error_code: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedExplicitSubject {
    src_ilk: String,
    registration_status: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,io_api=debug,io_common=info,fluxbee_sdk=info")
        }))
        .init();

    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoApiAdapterConfigContract);
    // Single-config model: boot reads ONLY the node-dir config.json and validates it through the
    // adapter contract via the SDK — no state/io-nodes dynamic file to shadow a respawn (BUG-4).
    let mut boot_state = bootstrap_io_control_plane_state(&config.node_name, adapter_contract.as_ref())
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

    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    if boot_state.effective_config.is_some() && (self_ilk_id.is_none() || self_tenant_id.is_none())
    {
        boot_state.current_state = IoNodeLifecycleState::FailedConfig;
        boot_state.last_error = Some(IoControlPlaneErrorInfo {
            code: "managed_identity_missing".to_string(),
            message:
                "IO.api requires Orchestrator-injected FLUXBEE_NODE_ILK_ID and FLUXBEE_NODE_TENANT_ID"
                    .to_string(),
        });
    }

    tracing::info!(
        node_name = %config.node_name,
        runtime_version = %config.node_version,
        hive_id = %config.hive_id,
        router_socket = %config.router_socket.display(),
        spawn_config_path = %config.spawn_config_path.display(),
        identity_target = %config.identity_target,
        frontdesk_target = %config.frontdesk_target,
        admin_target = %config.admin_target,
        self_ilk_id = ?self_ilk_id,
        self_tenant_id = ?self_tenant_id,
        "Edge-native IO.api starting"
    );

    let profile = OperationalRouteProfile::builder()
        .command_channel(RPC_CH_INCOMING)
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command(RPC_CH_INCOMING))
        .build()?;
    let dispatcher = RouterDispatcher::connect_with_retry(
        NodeConfig {
            name: config.node_name.clone(),
            router_socket: config.router_socket.clone(),
            uuid_persistence_dir: config.uuid_persistence_dir.clone(),
            uuid_mode: NodeUuidMode::Persistent,
            config_dir: config.config_dir.clone(),
            version: config.node_version.clone(),
        },
        Duration::from_secs(1),
        profile,
    )
    .await?;
    let sender = dispatcher.sender_snapshot();
    tracing::info!(full_name = %sender.full_name(), "IO.api connected to router socket");

    let identity: Arc<dyn IdentityResolver> = Arc::new(ShmIdentityResolver::new(&config.hive_id));
    let identity_provision_cfg = IdentityProvisionConfig {
        target: config.identity_target.clone(),
        timeout: Duration::from_millis(config.identity_timeout_ms),
    };
    let provisioner: Arc<dyn IdentityProvisioner> = Arc::new(FluxbeeIdentityProvisioner::new(
        dispatcher.clone(),
        identity_provision_cfg.clone(),
    ));
    let initial_dst = config::extract_runtime_dst_node(boot_state.effective_config.as_ref());
    let inbound = Arc::new(Mutex::new(InboundProcessor::new(
        sender.uuid().to_string(),
        InboundConfig {
            ttl: config.ttl,
            dedup_ttl: Duration::from_millis(config.dedup_ttl_ms),
            dedup_max_entries: config.dedup_max_entries,
            dst_node: initial_dst,
            provision_on_miss: true,
            blob_runtime: None,
            // io.api authenticates a principal and puts ITS tenant on each input, so this is only
            // the fallback for a request that somehow carries none.
            self_tenant_id: self_tenant_id.clone(),
        },
    )));
    let relay_policy = config::api_relay_policy(&config, boot_state.effective_config.as_ref())?;
    let relay = Arc::new(Mutex::new(
        RelayBuffer::new(relay_policy, InMemoryRelayStore::new())
            .map_err(|err| anyhow::anyhow!(err))?,
    ));
    let state = Arc::new(RuntimeState {
        config,
        self_ilk_id,
        self_tenant_id,
        control_plane: Arc::new(RwLock::new(boot_state.clone())),
        control_metrics: Arc::new(IoControlPlaneMetrics::with_initial_state(
            boot_state.current_state.as_str(),
            boot_state.config_version,
        )),
        adapter_contract,
        dispatcher,
        identity,
        provisioner,
        identity_provision_cfg,
        inbound,
        relay,
        publication: Arc::new(Mutex::new(PublicationRuntime::default())),
        publication_reconcile: Arc::new(Mutex::new(())),
    });

    reconcile_publication(&state).await;
    let mut router_task = tokio::spawn(run_router_loop(state.clone()));
    let relay_task = tokio::spawn(run_relay_flush_loop(state.clone()));
    let reconcile_task = tokio::spawn(run_publication_reconcile_loop(state.clone()));

    let router_finished = tokio::select! {
        result = &mut router_task => {
            result??;
            true
        }
        _ = shutdown_signal() => {
            tracing::info!(node = %state.config.node_name, "IO.api shutdown requested; leaving the durable Edge publication intact");
            false
        }
    };
    if !router_finished {
        router_task.abort();
        let _ = router_task.await;
    }
    relay_task.abort();
    reconcile_task.abort();
    let _ = relay_task.await;
    let _ = reconcile_task.await;
    Ok(())
}

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

async fn run_router_loop(state: Arc<RuntimeState>) -> Result<()> {
    let mut incoming = state
        .dispatcher
        .take_command_receiver(RPC_CH_INCOMING)
        .await?;
    while let Some(message) = incoming.recv().await {
        let sender = state.dispatcher.sender_snapshot();
        if try_handle_default_node_status(&sender, &message).await? {
            continue;
        }
        if is_config_command(&message) {
            let response = handle_control_message(&state, &message).await;
            sender.send(response).await?;
            continue;
        }
        if message.meta.msg_type == IO_API_INBOUND_FAMILY {
            let payload = handle_edge_request(&state, &message).await;
            sender
                .send(build_edge_reply(sender.uuid(), &message, payload))
                .await?;
            continue;
        }
        tracing::debug!(
            trace_id = %message.routing.trace_id,
            msg_type = %message.meta.msg_type,
            msg = ?message.meta.msg,
            source = ?message.routing.src_l2_name,
            "IO.api ignored unrelated mesh message"
        );
    }
    Ok(())
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

fn control_caller_authorized(state: &RuntimeState, message: &Message) -> bool {
    let caller = message.routing.src_l2_name.as_deref().map(str::trim);
    caller == Some(state.config.admin_target.as_str())
        || caller == Some(state.config.orchestrator_target.as_str())
}

async fn handle_control_message(state: &Arc<RuntimeState>, message: &Message) -> Message {
    let payload = if !control_caller_authorized(state, message) {
        tracing::warn!(
            trace_id = %message.routing.trace_id,
            source = ?message.routing.src_l2_name,
            command = ?message.meta.msg,
            "IO.api rejected configuration command from non-authority"
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
    log_config_get_served(
        &message.routing.trace_id,
        &state.config.node_name,
        &snapshot,
    );
    let mut payload = build_io_config_get_response_payload(
        &state.config.node_name,
        &snapshot,
        build_io_adapter_contract_payload(
            state.adapter_contract.as_ref(),
            snapshot.effective_config.as_ref(),
        ),
    );
    inject_runtime_status(&mut payload, state, false).await;
    payload
}

async fn apply_config_set(
    state: &Arc<RuntimeState>,
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
    if state.self_ilk_id.is_none() || state.self_tenant_id.is_none() {
        return build_io_config_set_error_payload(
            &state.config.node_name,
            &current,
            "managed_identity_missing",
            "Orchestrator did not inject the IO.api ILK and tenant",
        );
    }

    let relay_cfg =
        match config::extract_runtime_relay_config(Some(&effective), &ApiRelayConfig::default())
            .and_then(|value| config::api_relay_policy_from_config(&value))
        {
            Ok(policy) => policy,
            Err(err) => {
                return build_io_config_set_error_payload(
                    &state.config.node_name,
                    &current,
                    "invalid_config",
                    err.to_string(),
                )
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
    *state.control_plane.write().await = next.clone();
    if let Err(err) = state.relay.lock().await.replace_policy(relay_cfg) {
        tracing::error!(error = %err, "validated IO.api relay policy failed to hot-apply");
    }
    state
        .inbound
        .lock()
        .await
        .set_dst_node(config::extract_runtime_dst_node(Some(&effective)));
    state
        .control_metrics
        .record_config_set_ok(next.current_state.as_str(), next.config_version);

    let mut hot_applied = Vec::new();
    if section_changed(
        current.effective_config.as_ref(),
        &effective,
        &["io", "dst_node"],
    ) {
        hot_applied.push("io.dst_node".to_string());
    }
    if section_changed(
        current.effective_config.as_ref(),
        &effective,
        &["io", "relay"],
    ) {
        hot_applied.push("io.relay.*".to_string());
    }
    if section_changed(current.effective_config.as_ref(), &effective, &["ingress"]) {
        hot_applied.push("ingress.*".to_string());
    }
    if section_changed(current.effective_config.as_ref(), &effective, &["edge"])
        || section_changed(
            current.effective_config.as_ref(),
            &effective,
            &["io", "api_channel_id"],
        )
    {
        hot_applied.push("edge publication/ICH".to_string());
    }
    let mut restart_required = Vec::new();
    for section in ["node", "runtime"] {
        if section_changed(current.effective_config.as_ref(), &effective, &[section]) {
            restart_required.push(format!("{section}.*"));
        }
    }
    log_config_set_applied(
        &state.config.node_name,
        payload.schema_version,
        payload.config_version,
        &hot_applied,
        &[],
        &restart_required,
    );

    reconcile_publication(state).await;
    let mut response = build_io_config_set_ok_payload(&state.config.node_name, &next);
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "apply".to_string(),
            json!({
                "mode":"hot_reload",
                "hot_applied":hot_applied,
                "reinit_performed":[],
                "restart_required":restart_required,
            }),
        );
    }
    inject_runtime_status(&mut response, state, true).await;
    response
}

async fn inject_runtime_status(payload: &mut Value, state: &RuntimeState, issue_credential: bool) {
    let mut runtime = state.publication.lock().await;
    let mut publication = serde_json::to_value(&runtime.snapshot).unwrap_or_else(|_| json!({}));
    if let Some(object) = publication.as_object_mut() {
        object.insert(
            "credential_pending".to_string(),
            Value::Bool(runtime.pending_entry_token.is_some()),
        );
        if issue_credential {
            if let Some(token) = runtime.pending_entry_token.take() {
                object.insert("entry_token".to_string(), Value::String(token));
                object.insert("entry_token_one_time".to_string(), Value::Bool(true));
                object.insert("credential_pending".to_string(), Value::Bool(false));
            }
        }
    }
    drop(runtime);
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "runtime".to_string(),
            json!({
                "transport":"router_socket",
                "public_frontier":"SY.edge",
                "inbound_family":IO_API_INBOUND_FAMILY,
                "publication":publication,
                "control_plane_metrics":state.control_metrics.snapshot(),
            }),
        );
    }
}

async fn run_publication_reconcile_loop(state: Arc<RuntimeState>) {
    let mut ticker =
        tokio::time::interval(Duration::from_secs(state.config.reconcile_interval_secs));
    ticker.tick().await;
    loop {
        ticker.tick().await;
        reconcile_publication(&state).await;
    }
}

async fn reconcile_publication(state: &Arc<RuntimeState>) {
    let _reconcile_guard = state.publication_reconcile.lock().await;
    let control = state.control_plane.read().await.clone();
    let Some(effective) = control.effective_config.as_ref() else {
        set_publication_status(state, "unconfigured", None, None, None).await;
        return;
    };
    if control.current_state != IoNodeLifecycleState::Configured {
        set_publication_status(
            state,
            "error",
            None,
            None,
            control
                .last_error
                .as_ref()
                .map(|error| error.message.clone()),
        )
        .await;
        return;
    }
    let desired = match desired_publication(effective) {
        Ok(value) => value,
        Err(err) => {
            set_publication_status(state, "error", None, None, Some(err.detail)).await;
            return;
        }
    };
    let (Some(self_ilk), Some(self_tenant)) = (
        state.self_ilk_id.as_deref(),
        state.self_tenant_id.as_deref(),
    ) else {
        set_publication_status(
            state,
            "error",
            None,
            Some(desired.edge_node),
            Some("managed IO.api identity is missing".to_string()),
        )
        .await;
        return;
    };

    let active = {
        let runtime = state.publication.lock().await;
        (
            runtime.active_channel_id.clone(),
            runtime.active_ich.clone(),
            runtime.active_edge_node.clone(),
            runtime.snapshot.status.clone(),
        )
    };
    let active_matches_desired = active.1.is_some()
        && active.0.as_deref() == Some(desired.api_channel_id.as_str())
        && active.2.as_deref() == Some(desired.edge_node.as_str());
    if !desired.publish {
        if active_matches_desired && active.3 == "disabled" {
            return;
        }
        {
            let mut runtime = state.publication.lock().await;
            runtime.snapshot.status = "disabled".to_string();
            runtime.snapshot.last_error = None;
            runtime.snapshot.updated_at_ms = now_epoch_ms();
        }
        if active.1.is_some() && !active_matches_desired {
            if let (Some(ich), Some(edge_node)) = (active.1.as_deref(), active.2.as_deref()) {
                if let Err(err) = close_active_publication(state, ich, edge_node, true).await {
                    set_publication_status(
                        state,
                        "error",
                        Some(ich.to_string()),
                        Some(edge_node.to_string()),
                        Some(err),
                    )
                    .await;
                    return;
                }
            }
        }
        let ich = match active.1.clone().filter(|_| active_matches_desired) {
            Some(ich) => ich,
            None => {
                match ensure_publication_ich(state, self_ilk, self_tenant, &desired.api_channel_id)
                    .await
                {
                    Ok(ich) => ich,
                    Err(err) => {
                        set_publication_status(
                            state,
                            "error",
                            None,
                            Some(desired.edge_node),
                            Some(err),
                        )
                        .await;
                        return;
                    }
                }
            }
        };
        if let Err(err) = close_active_publication(state, &ich, &desired.edge_node, true).await {
            set_publication_status(
                state,
                "error",
                Some(ich),
                Some(desired.edge_node),
                Some(err),
            )
            .await;
            return;
        }
        let mut runtime = state.publication.lock().await;
        clear_active_publication(&mut runtime);
        runtime.active_channel_id = Some(desired.api_channel_id);
        runtime.active_ich = Some(ich.clone());
        runtime.active_edge_node = Some(desired.edge_node.clone());
        runtime.snapshot = PublicationSnapshot {
            status: "disabled".to_string(),
            ich: Some(ich),
            edge_node: Some(desired.edge_node),
            updated_at_ms: now_epoch_ms(),
            ..PublicationSnapshot::default()
        };
        return;
    }

    let active_changed = active.1.is_some()
        && (active.0.as_deref() != Some(desired.api_channel_id.as_str())
            || active.2.as_deref() != Some(desired.edge_node.as_str()));
    if active_changed {
        if let (Some(ich), Some(edge_node)) = (active.1.as_deref(), active.2.as_deref()) {
            if let Err(err) = close_active_publication(state, ich, edge_node, true).await {
                let mut runtime = state.publication.lock().await;
                runtime.snapshot.status = "error".to_string();
                runtime.snapshot.last_error = Some(err);
                runtime.snapshot.updated_at_ms = now_epoch_ms();
                return;
            }
        }
        {
            let mut runtime = state.publication.lock().await;
            clear_active_publication(&mut runtime);
        }
    }
    let ich = if !active_changed
        && active.0.as_deref() == Some(desired.api_channel_id.as_str())
        && active.2.as_deref() == Some(desired.edge_node.as_str())
        && active.3 != "disabled"
    {
        active.1.clone()
    } else {
        None
    };
    let ich = match ich {
        Some(ich) => ich,
        None => match ensure_publication_ich(state, self_ilk, self_tenant, &desired.api_channel_id)
            .await
        {
            Ok(ich) => ich,
            Err(err) => {
                let mut runtime = state.publication.lock().await;
                runtime.snapshot = PublicationSnapshot {
                    status: "error".to_string(),
                    edge_node: Some(desired.edge_node),
                    last_error: Some(err),
                    updated_at_ms: now_epoch_ms(),
                    ..PublicationSnapshot::default()
                };
                return;
            }
        },
    };
    match inspect_edge_publication(state, &desired.edge_node, &ich).await {
        Ok(Some(url)) => {
            let mut runtime = state.publication.lock().await;
            runtime.active_channel_id = Some(desired.api_channel_id);
            runtime.active_ich = Some(ich.clone());
            runtime.active_edge_node = Some(desired.edge_node.clone());
            runtime.snapshot = PublicationSnapshot {
                status: "published".to_string(),
                ich: Some(ich),
                edge_node: Some(desired.edge_node),
                url: Some(url),
                last_error: None,
                updated_at_ms: now_epoch_ms(),
            };
            return;
        }
        Ok(None) => {}
        Err(err) => {
            let mut runtime = state.publication.lock().await;
            if runtime.active_ich.as_deref() == Some(ich.as_str())
                && runtime.active_edge_node.as_deref() == Some(desired.edge_node.as_str())
                && runtime.snapshot.status == "published"
            {
                runtime.snapshot.last_error = Some(err);
                runtime.snapshot.updated_at_ms = now_epoch_ms();
            } else {
                runtime.snapshot = PublicationSnapshot {
                    status: "error".to_string(),
                    ich: Some(ich),
                    edge_node: Some(desired.edge_node),
                    last_error: Some(err),
                    updated_at_ms: now_epoch_ms(),
                    ..PublicationSnapshot::default()
                };
            }
            return;
        }
    }

    let reused_token = state.publication.lock().await.active_entry_token.clone();
    let mut params = json!({
        "ich":ich,
        "edge_node":desired.edge_node,
        "inbound_family":IO_API_INBOUND_FAMILY,
        "auth_mode":"shared-secret",
        "methods":[EDGE_METHOD],
    });
    if let Some(token) = reused_token.as_ref() {
        params["secret"] = Value::String(token.clone());
    }
    let response = state
        .dispatcher
        .send_admin_rpc(AdminCommandRequest {
            admin_target: &state.config.admin_target,
            action: "externalize",
            target: None,
            params,
            request_id: None,
            timeout: Duration::from_secs(20),
        })
        .await;
    match response {
        Ok(response) if response.status.eq_ignore_ascii_case("ok") => {
            let url = response
                .payload
                .get("url")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("/e/{ich}"));
            let entry_token = response
                .payload
                .get("token")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            if entry_token.is_none() {
                let mut runtime = state.publication.lock().await;
                runtime.snapshot = PublicationSnapshot {
                    status: "error".to_string(),
                    ich: Some(ich),
                    edge_node: Some(desired.edge_node),
                    last_error: Some(
                        "Admin externalize returned no token for shared-secret publication"
                            .to_string(),
                    ),
                    updated_at_ms: now_epoch_ms(),
                    ..PublicationSnapshot::default()
                };
                return;
            }
            let mut runtime = state.publication.lock().await;
            runtime.active_channel_id = Some(desired.api_channel_id);
            runtime.active_ich = Some(ich.clone());
            runtime.active_edge_node = Some(desired.edge_node.clone());
            runtime.active_entry_token = entry_token.clone();
            if reused_token.is_none() {
                runtime.pending_entry_token = entry_token;
            }
            runtime.snapshot = PublicationSnapshot {
                status: "published".to_string(),
                ich: Some(ich),
                edge_node: Some(desired.edge_node),
                url: Some(url),
                last_error: None,
                updated_at_ms: now_epoch_ms(),
            };
        }
        Ok(response) => {
            let mut runtime = state.publication.lock().await;
            runtime.snapshot = PublicationSnapshot {
                status: "error".to_string(),
                ich: Some(ich),
                edge_node: Some(desired.edge_node),
                last_error: Some(format!(
                    "Admin externalize rejected: {}",
                    response
                        .error_detail
                        .unwrap_or_else(|| response.payload.clone())
                )),
                updated_at_ms: now_epoch_ms(),
                ..PublicationSnapshot::default()
            };
        }
        Err(err) => {
            let mut runtime = state.publication.lock().await;
            runtime.snapshot = PublicationSnapshot {
                status: "error".to_string(),
                ich: Some(ich),
                edge_node: Some(desired.edge_node),
                last_error: Some(format!("Admin externalize failed: {err}")),
                updated_at_ms: now_epoch_ms(),
                ..PublicationSnapshot::default()
            };
        }
    }
}

async fn set_publication_status(
    state: &RuntimeState,
    status: &str,
    ich: Option<String>,
    edge_node: Option<String>,
    error: Option<String>,
) {
    state.publication.lock().await.snapshot = PublicationSnapshot {
        status: status.to_string(),
        ich,
        edge_node,
        url: None,
        last_error: error,
        updated_at_ms: now_epoch_ms(),
    };
}

fn desired_publication(effective: &Value) -> Result<DesiredPublication, ApiIngressError> {
    let edge = effective
        .get("edge")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiIngressError::new("node_not_configured", "missing edge config"))?;
    let io = effective
        .get("io")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiIngressError::new("node_not_configured", "missing io config"))?;
    Ok(DesiredPublication {
        publish: edge.get("publish").and_then(Value::as_bool).unwrap_or(true),
        edge_node: required_config_string(edge, "node", "edge.node")?,
        api_channel_id: required_config_string(io, "api_channel_id", "io.api_channel_id")?,
    })
}

fn required_config_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<String, ApiIngressError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| ApiIngressError::new("node_not_configured", format!("missing {label}")))
}

async fn ensure_publication_ich(
    state: &RuntimeState,
    self_ilk: &str,
    self_tenant: &str,
    api_channel_id: &str,
) -> Result<String, String> {
    ensure_own_ich(
        &state.dispatcher,
        &state.identity_provision_cfg,
        &state.config.identity_target,
        self_ilk,
        self_tenant,
        IO_API_CHANNEL_TYPE,
        api_channel_id,
    )
    .await
    .map(|result| result.ich_id)
    .map_err(|err| format!("own ICH registration failed: {err}"))
}

async fn inspect_edge_publication(
    state: &RuntimeState,
    edge_node: &str,
    ich: &str,
) -> Result<Option<String>, String> {
    let response = state
        .dispatcher
        .send_admin_rpc(AdminCommandRequest {
            admin_target: &state.config.admin_target,
            action: "list_externalized",
            target: None,
            params: json!({"edge_node":edge_node}),
            request_id: None,
            timeout: Duration::from_secs(20),
        })
        .await
        .map_err(|err| format!("Admin list_externalized failed: {err}"))?;
    if !response.status.eq_ignore_ascii_case("ok") {
        return Err(format!(
            "Admin list_externalized rejected: {}",
            response
                .error_detail
                .unwrap_or_else(|| response.payload.clone())
        ));
    }
    let matching = response
        .payload
        .get("channels")
        .and_then(Value::as_array)
        .and_then(|channels| {
            channels
                .iter()
                .find(|channel| publication_row_matches(channel, ich))
        });
    Ok(matching.map(|channel| {
        channel
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("/e/{ich}"))
    }))
}

fn publication_row_matches(channel: &Value, ich: &str) -> bool {
    let methods = channel
        .get("methods")
        .and_then(Value::as_array)
        .is_some_and(|methods| {
            methods.len() == 1
                && methods[0]
                    .as_str()
                    .is_some_and(|method| method.eq_ignore_ascii_case(EDGE_METHOD))
        });
    channel.get("ich").and_then(Value::as_str) == Some(ich)
        && channel.get("inbound_family").and_then(Value::as_str) == Some(IO_API_INBOUND_FAMILY)
        && channel.get("auth_mode").and_then(Value::as_str) == Some("shared-secret")
        && methods
}

async fn close_active_publication(
    state: &RuntimeState,
    ich: &str,
    edge_node: &str,
    disable_ich: bool,
) -> Result<(), String> {
    let response = state
        .dispatcher
        .send_admin_rpc(AdminCommandRequest {
            admin_target: &state.config.admin_target,
            action: "unexternalize",
            target: None,
            params: json!({"ich":ich, "edge_node":edge_node}),
            request_id: None,
            timeout: Duration::from_secs(20),
        })
        .await
        .map_err(|err| format!("Admin unexternalize failed: {err}"))?;
    if !response.status.eq_ignore_ascii_case("ok") {
        return Err(format!(
            "Admin unexternalize rejected: {}",
            response
                .error_detail
                .unwrap_or_else(|| response.payload.clone())
        ));
    }
    if disable_ich {
        set_ich_enabled(
            &state.dispatcher,
            &state.identity_provision_cfg,
            &state.config.identity_target,
            ich,
            false,
        )
        .await
        .map_err(|err| format!("failed to disable old ICH: {err}"))?;
    }
    Ok(())
}

fn clear_active_publication(runtime: &mut PublicationRuntime) {
    runtime.active_channel_id = None;
    runtime.active_ich = None;
    runtime.active_edge_node = None;
    runtime.active_entry_token = None;
    runtime.pending_entry_token = None;
}

async fn handle_edge_request(state: &Arc<RuntimeState>, message: &Message) -> Value {
    let control = state.control_plane.read().await.clone();
    if control.current_state != IoNodeLifecycleState::Configured {
        return api_error(
            "node_not_configured",
            control
                .last_error
                .as_ref()
                .map(|error| error.message.as_str())
                .unwrap_or("IO.api instance is not configured"),
        );
    }
    let Some(effective) = control.effective_config.as_ref() else {
        return api_error("node_not_configured", "IO.api has no effective config");
    };
    if let Err(err) = authorize_edge_message(state, message).await {
        return api_error(err.code, err.detail);
    }
    if let Err(err) = validate_edge_http_context(message.meta.context.as_ref()) {
        return api_error(err.code, err.detail);
    }
    let Some(tenant_id) = state.self_tenant_id.as_ref() else {
        return api_error("managed_identity_missing", "IO.api tenant is unavailable");
    };
    let principal = EndpointPrincipal {
        tenant_id: tenant_id.clone(),
        caller_identity: effective
            .get("ingress")
            .and_then(|ingress| ingress.get("caller_identity"))
            .cloned(),
    };
    let mut parsed = match parse_api_message_request(&message.payload, effective, &principal) {
        Ok(value) => value,
        Err(err) => return api_error(err.code, err.detail),
    };
    let resolved_subject = match parsed.explicit_subject_mode.as_ref() {
        Some(ExplicitSubjectMode::ByIlk { ilk }) => {
            match validate_explicit_subject_ilk(&state.config.hive_id, tenant_id, ilk) {
                Ok(()) => Some(ResolvedExplicitSubject {
                    src_ilk: ilk.clone(),
                    registration_status: None,
                }),
                Err(err) => return api_error(err.code, err.detail),
            }
        }
        Some(ExplicitSubjectMode::ByData) => {
            match resolve_explicit_subject(state, &parsed.identity_input).await {
                Ok(subject) => Some(subject),
                Err(err) => return api_error(err.code, err.detail),
            }
        }
        None => None,
    };
    // #4 fail-closed for explicit subjects: resolve_explicit_subject / by_ilk validation above
    // already fail CLOSED (an unresolvable subject returns api_error, never continues). Pin the
    // resolved ILK onto identity_input.src_ilk_override so a BUFFERED turn is dispatched with the
    // SAME ILK instead of being re-resolved by the relay-flush provisioner, which fails OPEN to a
    // null src_ilk (provision.rs FluxbeeIdentityProvisioner::provision -> Ok(None)). Result: an
    // explicit-subject turn can never reach a downstream node with a null subject. A message with
    // NO explicit subject (resolved_subject == None) intentionally keeps the anonymous path.
    if let Some(subject) = resolved_subject.as_ref() {
        parsed.identity_input.src_ilk_override = Some(subject.src_ilk.clone());
    }
    let dst_node = config::extract_runtime_dst_node(Some(effective));
    let response_ilk = resolved_subject
        .as_ref()
        .map(|subject| subject.src_ilk.as_str());

    if let Some(frontdesk_node) = dst_node
        .as_deref()
        .filter(|node| is_frontdesk_node(node))
        .map(ToString::to_string)
    {
        let Some(src_ilk) = response_ilk else {
            return api_error(
                "invalid_payload",
                "Frontdesk handoff requires an explicit subject ILK",
            );
        };
        let request = match build_frontdesk_handoff_request(
            &state.config.node_name,
            &parsed,
            &message.payload,
            src_ilk,
            frontdesk_node,
        ) {
            Ok(value) => value,
            Err(err) => return api_error(err.code, err.detail),
        };
        return match request_frontdesk_handoff(state, &parsed, request).await {
            Ok(result) => json!({
                "status": if result.success {"ok"} else {"error"},
                "request_id":parsed.request_id,
                "success":result.success,
                "human_message":result.human_message,
                "error_code":result.error_code,
            }),
            Err(err) => api_error(err.code, err.detail),
        };
    }
    if requires_frontdesk_intermediate(&parsed, resolved_subject.as_ref()) {
        let Some(src_ilk) = response_ilk else {
            return api_error(
                "invalid_payload",
                "Frontdesk handoff requires an explicit subject ILK",
            );
        };
        let request = match build_frontdesk_handoff_request(
            &state.config.node_name,
            &parsed,
            &message.payload,
            src_ilk,
            state.config.frontdesk_target.clone(),
        ) {
            Ok(value) => value,
            Err(err) => return api_error(err.code, err.detail),
        };
        match request_frontdesk_handoff(state, &parsed, request).await {
            Ok(result) if result.success => {}
            Ok(result) => {
                return json!({
                    "status":"error",
                    "request_id":parsed.request_id,
                    "success":false,
                    "human_message":result.human_message,
                    "error_code":result.error_code,
                })
            }
            Err(err) => return api_error(err.code, err.detail),
        }
    }
    route_api_message(state, parsed, message.payload.clone(), response_ilk).await
}

async fn authorize_edge_message(
    state: &RuntimeState,
    message: &Message,
) -> Result<(), ApiIngressError> {
    let runtime = state.publication.lock().await;
    if runtime.snapshot.status != "published" {
        return Err(ApiIngressError::new(
            "endpoint_not_ready",
            "IO.api endpoint is not currently published",
        ));
    }
    if message.routing.src_l2_name.as_deref().map(str::trim) != runtime.active_edge_node.as_deref()
    {
        return Err(ApiIngressError::new(
            "unauthorized",
            "request did not originate from the configured SY.edge",
        ));
    }
    if message.meta.ich.as_deref() != runtime.active_ich.as_deref() {
        return Err(ApiIngressError::new(
            "unauthorized",
            "request targeted an unexpected ICH",
        ));
    }
    Ok(())
}

fn validate_edge_http_context(context: Option<&Value>) -> Result<(), ApiIngressError> {
    let context = context.and_then(Value::as_object).ok_or_else(|| {
        ApiIngressError::new("invalid_edge_envelope", "missing Edge HTTP context")
    })?;
    if context.get("method").and_then(Value::as_str) != Some(EDGE_METHOD) {
        return Err(ApiIngressError::new(
            "method_not_allowed",
            "IO.api accepts POST only",
        ));
    }
    let path = context.get("path").and_then(Value::as_str).unwrap_or("/");
    if path != "/" {
        return Err(ApiIngressError::new(
            "path_not_found",
            "IO.api exposes only the root endpoint",
        ));
    }
    Ok(())
}

fn build_edge_reply(sender_uuid: &str, incoming: &Message, payload: Value) -> Message {
    Message {
        routing: Routing {
            src: sender_uuid.to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(incoming.routing.src.clone()),
            ttl: 16,
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: IO_API_INBOUND_FAMILY.to_string(),
            ich: incoming.meta.ich.clone(),
            ..Meta::default()
        },
        payload,
    }
}

fn api_error(code: &str, detail: impl Into<String>) -> Value {
    json!({
        "status":"error",
        "error_code":code,
        "error_detail":detail.into(),
    })
}

/// Validate a by_ilk explicit subject: the supplied ILK must exist and belong to THIS
/// IO.api instance's tenant. Cross-tenant subjects are rejected here.
///
/// DESIGN DECISION (audit residual #2, ratified 2026-07-21): there is intentionally NO
/// `ilk_type` gate — a bearer holder may present ANY same-tenant ILK as the subject,
/// including an agent/system ILK (the by_data path forces ilk_type=human, by_ilk does not).
/// This is safe ONLY under the standing constraint that NO downstream consumer treats the
/// forwarded `src_ilk` as an authenticated principal (it is a routing/attribution hint, and
/// is inert as an authority today). If a downstream node ever begins trusting `src_ilk` as a
/// principal (e.g. for vault/memory authorization), this gate MUST be revisited (add an
/// ilk_type allowlist) BEFORE that lands. Tenant isolation is the load-bearing boundary here.
fn validate_explicit_subject_ilk(
    hive_id: &str,
    tenant_id: &str,
    ilk_id: &str,
) -> Result<(), ApiIngressError> {
    let snapshot = list_ilks_from_hive_id(hive_id).map_err(|err| {
        ApiIngressError::new(
            "identity_unavailable",
            format!("unable to validate subject identity: {err}"),
        )
    })?;
    if ilk_belongs_to_tenant(&snapshot.ilks, ilk_id, tenant_id) {
        Ok(())
    } else {
        Err(ApiIngressError::new(
            "subject_not_found",
            "requested subject does not exist in this IO.api tenant",
        ))
    }
}

fn ilk_belongs_to_tenant(ilks: &[IdentityIlkOption], ilk_id: &str, tenant_id: &str) -> bool {
    ilks.iter()
        .any(|ilk| ilk.ilk_id == ilk_id && ilk.tenant_id == tenant_id)
}

async fn resolve_explicit_subject(
    state: &RuntimeState,
    input: &ResolveOrCreateInput,
) -> Result<ResolvedExplicitSubject, ApiIngressError> {
    match resolve_identity_option_from_hive_id(
        &state.config.hive_id,
        &input.channel,
        &input.external_id,
        input.tenant_id.as_deref().unwrap_or(""),
    ) {
        Ok(Some(resolved)) => {
            state.identity.remember(input, &resolved.ilk.ilk_id);
            return Ok(ResolvedExplicitSubject {
                src_ilk: resolved.ilk.ilk_id,
                registration_status: Some(resolved.ilk.registration_status),
            });
        }
        Ok(None) => {
            // #3 replica-lag guard: the identity SHM snapshot on a REPLICA hive can
            // transiently MISS a subject this process already resolved+provisioned. Before
            // re-provisioning, consult the local resolver cache: a hit means this is a
            // stale-SHM false-miss, not a genuine first contact, so reuse the known ILK
            // instead of firing a fresh ILK_PROVISION RPC on every repeat. Downstream
            // behavior is deliberately identical to the fresh-provision path below
            // (registration_status = "temporary" so the by_data multi-turn Frontdesk handoff
            // still fires); ILK_REGISTER is idempotent (sy_identity: same-tenant re-register
            // rewrites to the same "complete" state, deterministic canonical_ilk_id, no
            // duplicate ILK), so any resulting re-handoff is a no-op at the identity layer.
            // The io-api router loop is sequential (single recv().await consumer), so there
            // is no in-process concurrency to single-flight; and EDGE-H4 keeps public IO.api
            // instances off replica workers, so this path is a defensive backstop.
            if let Ok(Some(cached_ilk)) = state.identity.lookup(input) {
                return Ok(ResolvedExplicitSubject {
                    src_ilk: cached_ilk,
                    registration_status: Some("temporary".to_string()),
                });
            }
        }
        Err(err) => {
            return Err(ApiIngressError::new(
                "identity_unavailable",
                format!("unable to resolve subject identity: {err}"),
            ))
        }
    }
    let src_ilk = strict_provision_ilk(
        &state.dispatcher,
        &state.identity_provision_cfg,
        &state.config.identity_target,
        input,
    )
    .await
    .map_err(|err| ApiIngressError::new("identity_unavailable", err.to_string()))?;
    state.identity.remember(input, &src_ilk);
    Ok(ResolvedExplicitSubject {
        src_ilk,
        registration_status: Some("temporary".to_string()),
    })
}

fn is_frontdesk_node(node: &str) -> bool {
    node == "SY.frontdesk.gov" || node.starts_with("SY.frontdesk.gov@")
}

fn requires_frontdesk_intermediate(
    parsed: &ParsedApiMessage,
    subject: Option<&ResolvedExplicitSubject>,
) -> bool {
    matches!(
        parsed.explicit_subject_mode,
        Some(ExplicitSubjectMode::ByData)
    ) && subject
        .and_then(|value| value.registration_status.as_deref())
        .is_some_and(|status| !status.eq_ignore_ascii_case("complete"))
}

fn build_frontdesk_handoff_request(
    node_name: &str,
    parsed: &ParsedApiMessage,
    raw_payload: &Value,
    src_ilk: &str,
    dst_node: String,
) -> Result<FrontdeskResolvedRequest, ApiIngressError> {
    if !matches!(
        parsed.explicit_subject_mode,
        Some(ExplicitSubjectMode::ByData)
    ) {
        return Err(ApiIngressError::new(
            "invalid_payload",
            "Frontdesk handoff requires explicit_subject by_data",
        ));
    }
    let subject = raw_payload
        .get("subject")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiIngressError::new("invalid_payload", "field 'subject' is required"))?;
    let required = |field: &str| {
        subject
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                ApiIngressError::new(
                    "subject_data_incomplete",
                    format!("field 'subject.{field}' is required for Frontdesk"),
                )
            })
    };
    let payload = FrontdeskHandoffPayload {
        payload_type: FRONTDESK_HANDOFF_PAYLOAD_TYPE.to_string(),
        schema_version: FRONTDESK_SCHEMA_VERSION_V1,
        operation: "complete_registration".to_string(),
        subject: FrontdeskHandoffSubject {
            display_name: Some(required("display_name")?),
            email: Some(required("email")?),
            phone: subject
                .get("phone")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            company_name: subject
                .get("company_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            attributes: subject
                .get("attributes")
                .and_then(Value::as_object)
                .cloned(),
        },
        tenant_id: parsed.identity_input.tenant_id.clone(),
        context: Some(json!({
            "source_node":node_name,
            "request_id":parsed.request_id,
            "external_user_id":parsed.identity_input.external_id,
        })),
    };
    Ok(FrontdeskResolvedRequest {
        src_ilk: src_ilk.to_string(),
        payload,
        dst_node,
    })
}

async fn request_frontdesk_handoff(
    state: &RuntimeState,
    parsed: &ParsedApiMessage,
    request: FrontdeskResolvedRequest,
) -> Result<FrontdeskApiEnvelope, ApiIngressError> {
    let response_contract = frontdesk_response_contract();
    let context = set_response_envelope(
        Some(json!({
            "io": {
                "channel":parsed.io_context.channel,
                "entrypoint":parsed.io_context.entrypoint,
                "sender":parsed.io_context.sender,
                "conversation":parsed.io_context.conversation,
                "message":parsed.io_context.message,
                "reply_target":parsed.io_context.reply_target,
            }
        })),
        response_contract.clone(),
    )
    .map_err(|err| ApiIngressError::new("invalid_response_contract", err.to_string()))?;
    let sender = state.dispatcher.sender_snapshot();
    let message = io_common::router_message::build_user_message(
        sender.uuid(),
        Some(request.dst_node.clone()),
        16,
        io_common::router_message::new_trace_id(),
        Some(request.src_ilk),
        None,
        context,
        serde_json::to_value(request.payload).expect("frontdesk handoff payload"),
    );
    let reply = state
        .dispatcher
        .send_with_matcher(
            message,
            frontdesk_reply_matcher(),
            RpcRequestLabels::new(&request.dst_node, "FRONTDESK_HANDOFF", "FRONTDESK_REPLY"),
            state.identity_provision_cfg.timeout,
        )
        .await
        .map_err(|err| ApiIngressError::new("frontdesk_unavailable", err.to_string()))?;
    parse_frontdesk_reply_payload(&reply.payload, &response_contract)
}

fn frontdesk_reply_matcher() -> PendingMatcher {
    PendingMatcher::new(
        vec![RouteMatch::any_msg_type(USER_KIND)],
        vec![
            RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
            RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
        ],
        Vec::new(),
    )
}

fn parse_frontdesk_reply_payload(
    payload: &Value,
    response_contract: &Value,
) -> Result<FrontdeskApiEnvelope, ApiIngressError> {
    if payload.get("type").and_then(Value::as_str) == Some("error") {
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("frontdesk_error");
        let message = payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Frontdesk rejected the request");
        return Err(ApiIngressError::new(
            "frontdesk_unavailable",
            format!("{code}: {message}"),
        ));
    }
    let structured = parse_structured_response_payload(payload, response_contract)
        .map_err(|err| ApiIngressError::new("invalid_frontdesk_response", err.to_string()))?;
    serde_json::from_value(Value::Object(structured))
        .map_err(|err| ApiIngressError::new("invalid_frontdesk_response", err.to_string()))
}

async fn route_api_message(
    state: &RuntimeState,
    parsed: ParsedApiMessage,
    raw_payload: Value,
    response_ilk: Option<&str>,
) -> Value {
    let fragment = build_api_relay_fragment(&state.config.node_name, &parsed, raw_payload);
    match state.relay.lock().await.handle_fragment(fragment) {
        RelayDecision::Hold => {
            accepted_response(&state.config.node_name, &parsed, None, "held", response_ilk)
        }
        RelayDecision::FlushNow(turn) => {
            let trace_id = dispatch_assembled_turn(
                state.dispatcher.sender_snapshot(),
                state.identity.as_ref(),
                state.provisioner.as_ref(),
                state.inbound.clone(),
                turn,
                "relay immediate flush",
            )
            .await;
            accepted_response(
                &state.config.node_name,
                &parsed,
                trace_id,
                "flushed_immediately",
                response_ilk,
            )
        }
        RelayDecision::DropDuplicate => accepted_response(
            &state.config.node_name,
            &parsed,
            None,
            "duplicate_dropped",
            response_ilk,
        ),
        RelayDecision::RejectCapacity => {
            let outcome = state
                .inbound
                .lock()
                .await
                .process_inbound(
                    state.identity.as_ref(),
                    Some(state.provisioner.as_ref()),
                    parsed.identity_input.clone(),
                    None,
                    parsed.io_context.clone(),
                    parsed.payload.clone(),
                )
                .await;
            let trace_id = send_inbound_outcome(
                state.dispatcher.sender_snapshot(),
                outcome,
                "relay capacity fail-open",
            )
            .await;
            accepted_response(
                &state.config.node_name,
                &parsed,
                trace_id,
                "flushed_immediately",
                response_ilk,
            )
        }
        RelayDecision::DropExpired => api_error(
            "relay_unavailable",
            "relay session expired before the request could be accepted",
        ),
    }
}

fn accepted_response(
    node_name: &str,
    parsed: &ParsedApiMessage,
    trace_id: Option<String>,
    relay: &str,
    subject_ilk: Option<&str>,
) -> Value {
    json!({
        "status":"accepted",
        "accepted":true,
        "request_id":parsed.request_id,
        "trace_id":trace_id,
        "relay":relay,
        "subject_ilk":subject_ilk,
        "handled_by":node_name,
    })
}

fn build_api_relay_fragment(
    node_name: &str,
    parsed: &ParsedApiMessage,
    raw_payload: Value,
) -> RelayFragment {
    RelayFragment {
        relay_key: api_relay_key(
            node_name,
            &parsed.io_context.conversation.id,
            &parsed.identity_input.external_id,
        ),
        fragment_id: parsed.io_context.message.id.clone(),
        received_at_ms: now_epoch_ms(),
        content_text: parsed
            .payload
            .get("content")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        attachments: Vec::new(),
        raw_payload: Some(json!({"request":raw_payload, "transport":"sy.edge"})),
        io_context: parsed.io_context.clone(),
        identity_input: parsed.identity_input.clone(),
        dst_node_override: None,
        flush_hints: RelayFlushHints {
            final_fragment: parsed.relay_final,
        },
    }
}

async fn run_relay_flush_loop(state: Arc<RuntimeState>) -> Result<()> {
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    loop {
        ticker.tick().await;
        let turns = state.relay.lock().await.flush_expired(now_epoch_ms());
        for turn in turns {
            dispatch_assembled_turn(
                state.dispatcher.sender_snapshot(),
                state.identity.as_ref(),
                state.provisioner.as_ref(),
                state.inbound.clone(),
                turn,
                "relay scheduled flush",
            )
            .await;
        }
    }
}

async fn dispatch_assembled_turn(
    sender: NodeSender,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
    inbound: Arc<Mutex<InboundProcessor>>,
    turn: AssembledTurn,
    context: &str,
) -> Option<String> {
    let outcome = inbound
        .lock()
        .await
        .process_assembled_turn(identity, Some(provisioner), turn)
        .await;
    send_inbound_outcome(sender, outcome, context).await
}

async fn send_inbound_outcome(
    sender: NodeSender,
    outcome: InboundOutcome,
    context: &str,
) -> Option<String> {
    match outcome {
        InboundOutcome::SendNow(message) => {
            let trace_id = message.routing.trace_id.clone();
            match sender.send(message).await {
                Ok(()) => Some(trace_id),
                Err(err) => {
                    tracing::warn!(error = ?err, %trace_id, context, "IO.api failed to send inbound message");
                    None
                }
            }
        }
        InboundOutcome::DroppedDuplicate => None,
    }
}

fn section_changed(previous: Option<&Value>, current: &Value, path: &[&str]) -> bool {
    value_at_path(previous, path) != value_at_path(Some(current), path)
}

fn value_at_path<'a>(root: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    let mut value = root?;
    for segment in path {
        value = value.get(*segment)?;
    }
    Some(value)
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(source: &str, ich: &str) -> Message {
        Message {
            routing: Routing {
                src: "edge-uuid".to_string(),
                src_l2_name: Some(source.to_string()),
                dst: Destination::Unicast("IO.api.orders@worker".to_string()),
                ttl: 16,
                trace_id: "trace-1".to_string(),
            },
            meta: Meta {
                msg_type: IO_API_INBOUND_FAMILY.to_string(),
                ich: Some(ich.to_string()),
                context: Some(json!({"method":"POST", "path":"/"})),
                ..Meta::default()
            },
            payload: json!({"message":{"text":"hello"}}),
        }
    }

    #[test]
    fn desired_publication_uses_only_typed_edge_fields() {
        let desired = desired_publication(&json!({
            "edge": {"node":"SY.edge@ingress-1", "publish":true},
            "io": {"api_channel_id":"orders"}
        }))
        .expect("desired publication");
        assert_eq!(desired.edge_node, "SY.edge@ingress-1");
        assert_eq!(desired.api_channel_id, "orders");
    }

    #[test]
    fn existing_edge_row_must_match_fixed_protocol() {
        let row = json!({
            "ich":"ich:orders",
            "inbound_family":IO_API_INBOUND_FAMILY,
            "auth_mode":"shared-secret",
            "methods":["POST"]
        });
        assert!(publication_row_matches(&row, "ich:orders"));

        let mut drifted = row.clone();
        drifted["methods"] = json!(["GET"]);
        assert!(!publication_row_matches(&drifted, "ich:orders"));
        assert!(!publication_row_matches(&row, "ich:other"));
    }

    #[test]
    fn http_context_is_post_root_only() {
        assert!(validate_edge_http_context(Some(&json!({"method":"POST", "path":"/"}))).is_ok());
        assert!(validate_edge_http_context(Some(&json!({"method":"GET", "path":"/"}))).is_err());
        assert!(
            validate_edge_http_context(Some(&json!({"method":"POST", "path":"/admin"}))).is_err()
        );
    }

    #[test]
    fn ilk_tenant_match_is_exact() {
        let ilks = vec![IdentityIlkOption {
            ilk_id: "ilk:11111111-1111-4111-8111-111111111111".to_string(),
            tenant_id: "tnt:11111111-1111-4111-8111-111111111111".to_string(),
            display_name: None,
            handler_node: None,
            registration_status: "complete".to_string(),
            ilk_type: "human".to_string(),
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
        }];
        assert!(ilk_belongs_to_tenant(
            &ilks,
            "ilk:11111111-1111-4111-8111-111111111111",
            "tnt:11111111-1111-4111-8111-111111111111"
        ));
        assert!(!ilk_belongs_to_tenant(
            &ilks,
            "ilk:11111111-1111-4111-8111-111111111111",
            "tnt:22222222-2222-4222-8222-222222222222"
        ));
    }

    #[test]
    fn edge_reply_preserves_trace_and_channel() {
        let incoming = message("SY.edge@ingress-1", "ich:orders");
        let reply = build_edge_reply("io-api-uuid", &incoming, json!({"status":"accepted"}));
        assert_eq!(reply.routing.trace_id, "trace-1");
        assert_eq!(reply.meta.ich.as_deref(), Some("ich:orders"));
        assert_eq!(reply.meta.msg_type, IO_API_INBOUND_FAMILY);
    }

    #[test]
    fn frontdesk_rpc_matches_only_user_replies_and_transport_errors() {
        let matcher = frontdesk_reply_matcher();
        assert_eq!(matcher.success, vec![RouteMatch::any_msg_type(USER_KIND)]);
        assert_eq!(
            matcher.terminal_error,
            vec![
                RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
                RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
            ]
        );
        assert!(matcher.invalid_response.is_empty());
    }

    #[test]
    fn frontdesk_canonical_error_is_not_reported_as_contract_corruption() {
        let err = parse_frontdesk_reply_payload(
            &json!({
                "type":"error",
                "code":"node_not_configured",
                "message":"AI node is not configured yet. Retry later."
            }),
            &frontdesk_response_contract(),
        )
        .expect_err("frontdesk error payload must fail the handoff");
        assert_eq!(err.code, "frontdesk_unavailable");
        assert!(err.detail.contains("node_not_configured"));
    }
}
