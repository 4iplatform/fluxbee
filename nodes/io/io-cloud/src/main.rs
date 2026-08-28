#![forbid(unsafe_code)]

//! `IO.cloud` — the in-mesh adapter for the external system that is Fluxbee Cloud
//! (same species as `IO.linkedhelper`/`io-api`). It lives inside the mesh, talks to
//! `SY.admin` for scoped commands (its relay set), and is the only inbound path from
//! Fluxbee Cloud into the mesh (I2).
//!
//! The Cloud control-plane surface is deliberately small:
//! - it CONNECTS to the mesh as `IO.cloud`;
//! - it registers its OWN shared-secret, POST-only ICH on `SY.identity` (so a later,
//!   admin-driven `publish_cloud_endpoint` can resolve and externalize it on `SY.edge`);
//! - it accepts Cloud operations only when the router-stamped source is the CURRENTLY
//!   configured `SY.edge` (`config.io.edge_node`) and the request targets that exact ICH;
//! - it relays the bounded provisioning set (`create_tenant`, `put_token`,
//!   `provision_node`) to `SY.admin`.
//!
//! # Configuration: managed CONFIG plane, NOT env
//!
//! io.cloud is a FIRST-CLASS MANAGED IO node (like io.api / io.slack). Its single operator
//! field — `config.io.edge_node`, the `SY.edge` it trusts for inbound traffic — is read from
//! the managed CONFIG_SET/GET control plane (`IoCloudAdapterConfigContract`), never from an env
//! var. `publish_cloud_endpoint` (an admin action) normally records the edge into this node's
//! config as part of publishing; `CONFIG_SET` is the manual override. There is deliberately NO
//! secret field: the endpoint token is minted by `SY.admin` into the vault.
//!
//! Two decisions that separate io.cloud from io.api:
//! - **Motherbee-only, fail-closed in the binary.** io.cloud used to be a packaged systemd
//!   unit gated by an `ExecCondition`; as a managed runtime that gate is gone, so the binary
//!   itself refuses to start on a non-motherbee hive.
//! - **Externalize is admin-driven.** io.cloud registers its own ICH at boot but does NOT
//!   self-externalize — `publish_cloud_endpoint` owns opening the public door on `SY.edge`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::cloud::{cloud_action_catalog, is_cloud_local_op};
use fluxbee_sdk::vault::is_cloud_reserved_vault_key;
use fluxbee_sdk::rpc::AdminCommandRequest;
use fluxbee_sdk::{
    managed_node_config_path, try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode,
    OperationalRouteProfile, PendingMatcher, RouteMatch, RouteTarget, RouterDispatcher,
    RpcCommandReceiver, RpcRequestLabels, FLUXBEE_NODE_NAME_ENV,
};
use io_common::frontdesk_contract::{FRONTDESK_HANDOFF_PAYLOAD_TYPE, FRONTDESK_SCHEMA_VERSION_V1};
use io_common::frontdesk_gate::{
    frontdesk_response_contract, DEFAULT_FRONTDESK_TARGET, FRONTDESK_OPERATION_REGISTER,
};
use io_common::identity::ResolveOrCreateInput;
use io_common::io_context::{parse_structured_response_payload, set_response_envelope};
use io_common::router_message::{build_user_message, new_trace_id, DEFAULT_TTL};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_cloud_adapter_config::{trusted_edge_node, IoCloudAdapterConfigContract};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload, ensure_config_version_advances,
    parse_and_validate_io_control_plane_request, IoConfigSource, IoControlPlaneErrorInfo,
    IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_persist_error,
    log_config_set_stale_rejected, log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::persist_io_control_plane_state;
use io_common::provision::{ensure_own_ich, strict_provision_ilk, IdentityProvisionConfig};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn std::error::Error + Send + Sync>;

const RPC_CH_INCOMING: &str = "incoming";

/// io.cloud is motherbee-only. As a managed runtime there is no systemd `ExecCondition` gate
/// anymore, so the binary enforces it (fail-closed self-check at boot).
const MOTHERBEE_HIVE: &str = "motherbee";

/// FROZEN channel identity, NOT operator config (the contract deliberately omits it): the family
/// under which the edge forwards inbound Cloud requests, mirrored by the family gate in `run_loop`.
const CLOUD_INBOUND_FAMILY: &str = "user";

/// The msg_type of a mesh USER message — the kind the frontdesk replies in. Kept SEPARATE from
/// `CLOUD_INBOUND_FAMILY` (which happens to share the value today) so the frontdesk-reply matcher is
/// decoupled from the inbound-family gate. Mirrors io.api's `USER_KIND`.
const USER_KIND: &str = "user";

/// FROZEN channel identity: the `(channel_type, channel_address)` io.cloud registers its own ICH
/// under. These are the identity of the public URL, not something an operator tunes.
const CLOUD_CHANNEL_TYPE: &str = "cloud";
const CLOUD_CHANNEL_ADDRESS: &str = "demo";

/// Fallback tenant for the self-provisioned ICH when the orchestrator did not inject one.
const DEFAULT_SELF_TENANT: &str = "tnt:00000000-0000-0000-0000-000000000001";

/// Boot configuration — infra + wiring only. The one OPERATOR field (`io.edge_node`) lives in the
/// managed CONFIG plane, never here. Mirrors `io-api`'s `Config`, dropping the `IO_CLOUD_*` config
/// envs: infra comes from the managed spawn config (with the generic env overrides io.api also
/// honours), and the admin/orchestrator/identity targets are derived from the hive.
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
    admin_target: String,
    orchestrator_target: String,
}

struct SpawnConfig {
    path: PathBuf,
    doc: Value,
}

/// Shared runtime state, mirroring io-api's `RuntimeState`: the live control plane behind an
/// `RwLock` so a `CONFIG_SET` hot-applies (the trusted edge is read from here on every request).
struct RuntimeState {
    config: Config,
    own_ich: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    dispatcher: Arc<RouterDispatcher>,
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,io_cloud=debug,io_common=info,fluxbee_sdk=info")
        }))
        .init();

    // Motherbee-only backstop (fail-closed). io.cloud used to be a packaged systemd unit gated
    // by an ExecCondition; as a managed runtime that gate is gone, so the binary refuses to run on
    // any other hive rather than trusting the deploy to place it correctly.
    if config.hive_id != MOTHERBEE_HIVE {
        tracing::error!(
            node_name = %config.node_name,
            hive_id = %config.hive_id,
            "IO.cloud is motherbee-only; refusing to start on a non-motherbee hive"
        );
        return Err(format!(
            "IO.cloud must run on '{MOTHERBEE_HIVE}' (resolved hive '{}')",
            config.hive_id
        )
        .into());
    }

    // Single-config model: boot reads ONLY the node-dir config.json and validates it through the
    // adapter contract via the SDK (mirrors io.api). An io.cloud with no edge is a VALID node —
    // it serves the mesh and trusts no edge yet — so an empty config is CONFIGURED, not FAILED.
    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoCloudAdapterConfigContract);
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

    tracing::info!(
        node_name = %config.node_name,
        runtime_version = %config.node_version,
        hive_id = %config.hive_id,
        router_socket = %config.router_socket.display(),
        spawn_config_path = %config.spawn_config_path.display(),
        identity_target = %config.identity_target,
        admin_target = %config.admin_target,
        orchestrator_target = %config.orchestrator_target,
        lifecycle_state = %boot_state.current_state.as_str(),
        trusted_edge = ?trusted_edge_node(boot_state.effective_config.as_ref()),
        "IO.cloud starting (managed in-mesh Fluxbee Cloud adapter)"
    );

    // Broad catch-all: unsolicited traffic (edge-forwarded requests under any family, plus system
    // messages incl. CONFIG_GET/SET) lands on `incoming`. Admin responses to our own outbound RPCs
    // are correlated by trace_id in the pending map first, so they never leak here.
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
    let full_name = sender.full_name().to_string();
    tracing::info!(full_name = %full_name, "IO.cloud connected to router");

    // ICH self-provision (KEPT). io.cloud still needs its OWN durable channel/ICH so that the
    // admin-driven `publish_cloud_endpoint` can resolve it later and externalize it on SY.edge.
    // Boot self-externalize is intentionally GONE — publishing is now an admin action, not a boot
    // side effect. As an enabled boot node, io.cloud may come up before the mesh (SY.identity) is
    // ready, so the registration retries; if it can't land, exit non-zero so the orchestrator
    // restarts us for a fresh attempt (io.cloud with no ICH is useless anyway).
    let self_tenant =
        fluxbee_sdk::read_self_tenant_from_env().unwrap_or_else(|| DEFAULT_SELF_TENANT.to_string());
    let own_ich = match ensure_own_channel_with_retry(
        &dispatcher,
        &config.identity_target,
        &self_tenant,
        CLOUD_CHANNEL_TYPE,
        CLOUD_CHANNEL_ADDRESS,
    )
    .await
    {
        Ok(ich_id) => {
            tracing::info!(
                ich_id = %ich_id,
                tenant = %self_tenant,
                channel_type = %CLOUD_CHANNEL_TYPE,
                address = %CLOUD_CHANNEL_ADDRESS,
                "IO.cloud own channel ICH enabled — ready for admin-driven externalize on SY.edge"
            );
            ich_id
        }
        Err(err) => {
            tracing::error!(error = %err, "IO.cloud channel registration exhausted retries; exiting for restart");
            return Err(err.into());
        }
    };

    let state = Arc::new(RuntimeState {
        config,
        own_ich,
        control_plane: Arc::new(RwLock::new(boot_state.clone())),
        control_metrics: Arc::new(IoControlPlaneMetrics::with_initial_state(
            boot_state.current_state.as_str(),
            boot_state.config_version,
        )),
        adapter_contract,
        dispatcher,
    });

    let mut incoming = state
        .dispatcher
        .take_command_receiver(RPC_CH_INCOMING)
        .await?;
    run_loop(&sender, &full_name, &state, &mut incoming).await
}

/// Validate the boot effective config through the adapter contract (mirrors io.api). A candidate
/// that the contract rejects lands the node in FAILED_CONFIG with the contract's error.
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

async fn run_loop(
    sender: &NodeSender,
    full_name: &str,
    state: &Arc<RuntimeState>,
    incoming: &mut RpcCommandReceiver,
) -> Result<(), DynError> {
    loop {
        let msg = match timeout(Duration::from_secs(300), incoming.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => return Ok(()),
            Err(_) => continue,
        };

        if try_handle_default_node_status(sender, &msg).await? {
            continue;
        }

        // Control-plane branch FIRST (mirror io.api run_router_loop), BEFORE the edge gate: a
        // CONFIG_GET/SET from the configured Admin or Orchestrator is answered here and hot-applies
        // without a restart. It must precede the edge gate because it comes from SY.admin /
        // SY.orchestrator, not from the edge — the edge gate would otherwise drop it.
        if is_config_command(&msg) {
            let response = handle_control_message(state, &msg).await;
            sender.send(response).await?;
            continue;
        }

        // Trusted edge is read LIVE from the control plane on EVERY request (hot-apply): a
        // CONFIG_SET that changes `io.edge_node` takes effect on the very next message, no restart.
        let effective = state.control_plane.read().await.effective_config.clone();
        let cloud_edge_node = trusted_edge_node(effective.as_ref());
        let cloud_edge_node = cloud_edge_node.as_deref();

        // RouteMatch::Any also receives unsolicited mesh notifications (for example VAULT changes).
        // They are not Cloud requests and must not get a reply from IO.cloud.
        if !message_from_configured_edge(&msg, cloud_edge_node) {
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                src_l2_name = ?msg.routing.src_l2_name,
                "IO.cloud ignored a non-edge mesh message"
            );
            continue;
        }

        // FIX-16: family gate (mirrors io.api). The channel is registered under `inbound_family`;
        // the edge forwards legitimate requests stamped with that family, so a frame of any other
        // msg_type is not a Cloud request — skip it. Defense-in-depth on top of src_l2_name + ich.
        if !msg.meta.msg_type.eq_ignore_ascii_case(CLOUD_INBOUND_FAMILY) {
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                expected = %CLOUD_INBOUND_FAMILY,
                "IO.cloud skipped a frame whose msg_type != configured inbound_family"
            );
            continue;
        }

        // A request the edge forwarded under our channel. `meta.ich` = which channel (§4/§7.5).
        // IO.cloud is the internal Fluxbee Cloud relay (spec §3): the body is `{op, tenant_id,
        // params}` and we translate it into the matching ADMIN_COMMAND(s), returning the result.
        let ich = msg.meta.ich.clone().unwrap_or_default();
        let op = msg
            .payload
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let started = std::time::Instant::now();
        tracing::info!(
            trace_id = %msg.routing.trace_id,
            ich = %ich,
            op = %op,
            routed_from = %msg.routing.src,
            "IO.cloud received a Cloud request"
        );

        let mut response = match authorize_cloud_message(&msg, &state.own_ich, cloud_edge_node) {
            // io.cloud-LOCAL ops (CLOUD_LOCAL_OPS: register_human, list_cloud_actions) are handled
            // here — io.cloud does the work itself and NEVER touches SY.admin. The local-vs-relay
            // decision comes from the declared SDK set, not a magic string. Everything else is the
            // bounded SY.admin relay (dispatch_cloud_op, which also errors on an unknown op).
            Ok(()) if is_cloud_local_op(&op) => handle_local_cloud_op(sender, state, &msg, &op).await,
            Ok(()) => {
                dispatch_cloud_op(
                    &state.dispatcher,
                    &state.config.admin_target,
                    &state.config.hive_id,
                    &msg.payload,
                )
                .await
            }
            Err(detail) => {
                tracing::warn!(
                    trace_id = %msg.routing.trace_id,
                    src_l2_name = ?msg.routing.src_l2_name,
                    ich = ?msg.meta.ich,
                    "IO.cloud rejected request outside the authenticated edge channel"
                );
                cloud_error_code("UNAUTHORIZED", detail)
            }
        };
        if let Some(obj) = response.as_object_mut() {
            if obj
                .get("request_id")
                .map(|value| value.is_null())
                .unwrap_or(true)
            {
                if let Some(request_id) = msg
                    .payload
                    .get("request_id")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    obj.insert("request_id".to_string(), json!(request_id));
                }
            }
            obj.insert("handled_by".to_string(), json!(full_name));
            obj.insert("ich".to_string(), json!(ich));
        }

        // Egress/outcome line — the half the ingress log was missing. Every Cloud op now logs its
        // result (status, error, the id it touched, elapsed) at INFO, keyed by the SAME trace_id, so
        // the whole round-trip is greppable end-to-end (family tracing convention, cf. SY.identity).
        // NOTE: inside `tracing::*!`, a bare `Value` resolves to `tracing::Value` (a trait), so use
        // closures rather than `Value::as_str` for the serde_json reads here.
        tracing::info!(
            trace_id = %msg.routing.trace_id,
            op = %op,
            status = response.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
            error_code = ?response.get("error_code").and_then(|v| v.as_str()),
            registration_status = ?response.get("registration_status").and_then(|v| v.as_str()),
            ilk_id = ?response.get("ilk_id").and_then(|v| v.as_str()),
            tenant_id = ?response
                .get("result")
                .and_then(|r| r.get("tenant_id"))
                .and_then(|v| v.as_str()),
            elapsed_ms = %started.elapsed().as_millis(),
            "IO.cloud Cloud op completed"
        );

        let reply = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(msg.routing.src.clone()),
                ttl: 16,
                trace_id: msg.routing.trace_id.clone(),
            },
            meta: Meta {
                // reply in the same family, carry the channel back
                msg_type: msg.meta.msg_type.clone(),
                ich: msg.meta.ich.clone(),
                ..Meta::default()
            },
            payload: response,
        };
        sender.send(reply).await?;
    }
}

// ---------------------------------------------------------------------------
// Managed control plane (CONFIG_GET/SET). Mirrors io-api, REUSING io_common helpers.
// ---------------------------------------------------------------------------

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
            "IO.cloud rejected configuration command from non-authority"
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
    // Hot-apply: swap the live control plane. `run_loop` reads the trusted edge from here on every
    // request, so the new `io.edge_node` is authoritative on the next message with no restart.
    *state.control_plane.write().await = next.clone();
    state
        .control_metrics
        .record_config_set_ok(next.current_state.as_str(), next.config_version);

    let mut hot_applied = Vec::new();
    if section_changed(
        current.effective_config.as_ref(),
        &effective,
        &["io", "edge_node"],
    ) {
        hot_applied.push("io.edge_node".to_string());
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

/// Attach a small runtime block to CONFIG responses (mirrors io.api's `inject_runtime_status`,
/// minus the publication/credential machinery io.cloud no longer owns): the trusted edge read from
/// the live config, the node's own ICH, that externalize is admin-driven, and the control metrics.
async fn inject_runtime_status(payload: &mut Value, state: &RuntimeState) {
    let control = state.control_plane.read().await.clone();
    let trusted_edge = trusted_edge_node(control.effective_config.as_ref());
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "runtime".to_string(),
            json!({
                "transport":"router_socket",
                "public_frontier":"SY.edge",
                "inbound_family":CLOUD_INBOUND_FAMILY,
                "own_ich":state.own_ich,
                "trusted_edge":trusted_edge,
                "externalize":"admin_driven",
                "control_plane_metrics":state.control_metrics.snapshot(),
            }),
        );
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

/// Retry `ensure_own_channel` while the mesh is still starting (SY.identity not yet reachable).
/// Bounded so a genuinely broken deploy exits (orchestrator restarts) rather than hanging forever.
/// Tunable via `IO_CLOUD_REGISTER_ATTEMPTS` (default 30) at 2s apart.
async fn ensure_own_channel_with_retry(
    dispatcher: &Arc<RouterDispatcher>,
    identity_target: &str,
    self_tenant: &str,
    channel_type: &str,
    channel_address: &str,
) -> Result<String, String> {
    let max_attempts: u32 = env("IO_CLOUD_REGISTER_ATTEMPTS")
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30);
    let mut last_err = String::new();
    for attempt in 1..=max_attempts {
        match ensure_own_channel(
            dispatcher,
            identity_target,
            self_tenant,
            channel_type,
            channel_address,
        )
        .await
        {
            Ok(ich) => return Ok(ich),
            Err(err) => {
                tracing::warn!(attempt, max = max_attempts, error = %err, "IO.cloud channel registration failed; mesh may still be starting — retrying");
                last_err = err;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Err(format!(
        "channel registration failed after {max_attempts} attempts: {last_err}"
    ))
}

/// Ensure IO.cloud owns a channel/ICH in SY.identity, self-provisioning its ilk when the
/// orchestrator did not inject one (`FLUXBEE_NODE_ILK_ID`). Returns the ICH id — the stable
/// identity of the public URL that `publish_cloud_endpoint` will bind to an edge endpoint.
async fn ensure_own_channel(
    dispatcher: &Arc<RouterDispatcher>,
    identity_target: &str,
    self_tenant: &str,
    channel_type: &str,
    channel_address: &str,
) -> Result<String, String> {
    let cfg = IdentityProvisionConfig {
        target: identity_target.to_string(),
        timeout: Duration::from_secs(10),
    };
    // Our own ilk: injected by the orchestrator at spawn, else self-provisioned. IO nodes
    // are authorized for ILK_PROVISION (`IO.` prefix), so an unmanaged run can still stand
    // up a real registered ilk to hang the owned channel on.
    let self_ilk = match fluxbee_sdk::read_self_ilk_from_env() {
        Some(ilk) => {
            tracing::info!(ilk = %ilk, "IO.cloud using orchestrator-provided self ilk");
            ilk
        }
        None => {
            let input = ResolveOrCreateInput {
                channel: channel_type.to_string(),
                external_id: format!("{channel_address}-owner"),
                src_ilk_override: None,
                tenant_id: Some(self_tenant.to_string()),
                tenant_hint: None,
                attributes: json!({ "source": "io.cloud", "role": "self" }),
                ilk_type: Some("agent".to_string()),
            };
            let ilk = strict_provision_ilk(dispatcher, &cfg, identity_target, &input)
                .await
                .map_err(|e| format!("self-provision ilk failed: {e}"))?;
            tracing::info!(ilk = %ilk, "IO.cloud self-provisioned its ilk");
            ilk
        }
    };
    let result = ensure_own_ich(
        dispatcher,
        &cfg,
        identity_target,
        &self_ilk,
        self_tenant,
        channel_type,
        channel_address,
    )
    .await
    .map_err(|e| format!("ensure_own_ich failed: {e}"))?;
    tracing::info!(
        ich_id = %result.ich_id,
        owner_l2_name = ?result.owner_l2_name,
        enabled = result.enabled,
        "IO.cloud own ICH ensured"
    );
    Ok(result.ich_id)
}

/// The bearer is verified by SY.edge, so the exact configured edge origin and ICH are the internal
/// proof that this request passed through that authenticated door.
fn authorize_cloud_message(
    msg: &Message,
    own_ich: &str,
    cloud_edge_node: Option<&str>,
) -> Result<(), &'static str> {
    let Some(trusted_edge) = cloud_edge_node
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err("Cloud endpoint is not configured");
    };
    if msg.routing.src_l2_name.as_deref().map(str::trim) != Some(trusted_edge) {
        return Err("Cloud operation did not originate from the configured SY.edge");
    }
    if msg.meta.ich.as_deref() != Some(own_ich) {
        return Err("Cloud operation targeted an unexpected ICH");
    }
    Ok(())
}

fn message_from_configured_edge(msg: &Message, cloud_edge_node: Option<&str>) -> bool {
    let Some(trusted_edge) = cloud_edge_node
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    msg.routing.src_l2_name.as_deref().map(str::trim) == Some(trusted_edge)
}

/// Error response payload for a rejected/malformed Cloud request.
fn cloud_error(detail: &str) -> serde_json::Value {
    json!({ "status": "error", "error_detail": detail })
}

fn cloud_error_code(code: &str, detail: &str) -> serde_json::Value {
    json!({ "status": "error", "error_code": code, "error_detail": detail })
}

fn required_string(params: &serde_json::Value, field: &str) -> Result<String, serde_json::Value> {
    params
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| cloud_error(&format!("missing or invalid params.{field}")))
}

/// `params.{field}` as a trimmed non-empty string, or `None` (absent/empty/not-a-string).
fn optional_trimmed(params: &serde_json::Value, field: &str) -> Option<String> {
    params
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn require_tenant_id(tenant_id: Option<&str>, op: &str) -> Result<String, serde_json::Value> {
    let tenant_id = tenant_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| cloud_error(&format!("{op} requires tenant_id")))?;
    let valid = tenant_id
        .strip_prefix("tnt:")
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .is_some();
    if !valid {
        return Err(cloud_error(&format!(
            "{op} requires canonical tenant_id tnt:<uuid>"
        )));
    }
    Ok(tenant_id.to_string())
}

fn copy_optional_field(
    src: &serde_json::Value,
    dst: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) {
    if let Some(value) = src.get(field) {
        dst.insert(field.to_string(), value.clone());
    }
}

/// Pure translation of a Cloud op into the admin `(action, params)`, injecting the tenant claim.
/// Returns `Err(error_payload)` for a missing/unknown op or a missing tenant. Kept pure (no admin
/// call) so it is unit-testable; the op→action pairs are pinned to the shared source
/// `fluxbee_sdk::cloud::CLOUD_OP_ACTIONS` (which SY.admin's relay allowlist also derives from) by
/// `translate_cloud_op_actions_match_the_shared_sdk_vocabulary`.
fn translate_cloud_op(
    op: &str,
    tenant_id: Option<&str>,
    params: &serde_json::Value,
) -> Result<(&'static str, serde_json::Value), serde_json::Value> {
    match op {
        "create_tenant" => {
            let name = required_string(params, "name")?;
            let mut tenant = serde_json::Map::new();
            tenant.insert("name".to_string(), json!(name));
            // Cloud's service token is the authority in the alpha contract, so Cloud-created
            // tenants are immediately usable unless it explicitly requests another state.
            tenant.insert(
                "status".to_string(),
                params
                    .get("status")
                    .cloned()
                    .unwrap_or_else(|| json!("active")),
            );
            for field in ["domain", "settings", "sponsor_tenant_id"] {
                copy_optional_field(params, &mut tenant, field);
            }
            Ok(("create_tenant", serde_json::Value::Object(tenant)))
        }
        "put_token" => {
            let tenant_id = require_tenant_id(tenant_id, "put_token")?;
            let key = required_string(params, "key")?;
            // put_token stores PROVIDER tokens; the keys that protect the mesh itself (endpoint
            // bearers, edge TLS, spoke recovery keys) are reserved for SY.* internals. Reject early
            // with a clear error — SY.admin re-enforces this server-side (single source in the SDK).
            if is_cloud_reserved_vault_key(&key) {
                return Err(cloud_error(
                    "put_token may not write a reserved infrastructure key (edge_channel_secret:*, edge_tls, ssh:*)",
                ));
            }
            let value = params
                .get("value")
                .filter(|value| !value.is_null())
                .cloned()
                .ok_or_else(|| cloud_error("missing params.value"))?;
            let mut metadata = params
                .get("metadata")
                .and_then(|value| value.as_object())
                .cloned()
                .unwrap_or_default();
            let owner_node = params
                .get("owner_node")
                .or_else(|| metadata.get("owner_node"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            // Cloud may add descriptive metadata, but tenant and ownership are authority fields.
            // In particular, accepting metadata.ilk would bypass admin's owner_node resolution.
            for authority_field in ["tenant_id", "ilk", "owner_ilk", "owner_l2", "owner_node"] {
                metadata.remove(authority_field);
            }
            if let Some(resource_type) = params.get("resource_type") {
                metadata.insert("resource_type".to_string(), resource_type.clone());
            }
            let resource_type = metadata
                .get("resource_type")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    cloud_error(
                        "put_token requires params.resource_type or params.metadata.resource_type",
                    )
                })?;
            metadata.insert("resource_type".to_string(), json!(resource_type));
            metadata.insert("tenant_id".to_string(), json!(tenant_id));
            if let Some(owner_node) = owner_node {
                if !owner_node.starts_with("IO.") {
                    return Err(cloud_error("put_token owner_node must be an IO.* node"));
                }
                metadata.insert("owner_node".to_string(), json!(owner_node));
            } else {
                metadata.remove("owner_node");
            }
            Ok((
                "vault_put",
                json!({ "key": key, "value": value, "metadata": metadata }),
            ))
        }
        "provision_node" => {
            let tenant_id = require_tenant_id(tenant_id, "provision_node")?;
            let node_name = required_string(params, "node_name")?;
            if !node_name.starts_with("IO.") {
                return Err(cloud_error("provision_node may launch only IO.* nodes"));
            }
            let mut provision = serde_json::Map::new();
            provision.insert("node_name".to_string(), json!(node_name));
            provision.insert("tenant_id".to_string(), json!(tenant_id));
            for field in [
                "runtime",
                "runtime_version",
                "add_channels",
                "identity_change_reason",
            ] {
                copy_optional_field(params, &mut provision, field);
            }
            if let Some(config) = params.get("config") {
                let mut config = config
                    .as_object()
                    .cloned()
                    .ok_or_else(|| cloud_error("params.config must be an object"))?;
                config.insert("tenant_id".to_string(), json!(tenant_id));
                provision.insert("config".to_string(), serde_json::Value::Object(config));
            }
            Ok(("run_node", serde_json::Value::Object(provision)))
        }
        "get_ilk_details" => {
            // FULL identity read — relayed to SY.admin's `get_ilk` (reads the DB record: all
            // identification PII + channels + tenant). The FAST existence/subset read is the LOCAL
            // `get_ilk` (SHM); this is the "give me everything" path. An email selector is
            // accepted too: `dispatch_cloud_op` pre-resolves email -> ilk_id from the SHM BEFORE
            // this translation runs, so by the time we get here a valid request always carries
            // ilk_id (translation stays pure; admin/identity need no email selector).
            let ilk_id = match required_string(params, "ilk_id") {
                Ok(v) => v,
                Err(_) => {
                    return Err(cloud_error(
                        "get_ilk_details requires params.ilk_id (ilk:<uuid>) or params.email + params.tenant_id",
                    ))
                }
            };
            if !ilk_id.starts_with("ilk:") {
                return Err(cloud_error(
                    "get_ilk_details requires params.ilk_id as a canonical ilk:<uuid>",
                ));
            }
            Ok(("get_ilk", json!({ "ilk_id": ilk_id })))
        }
        "" => Err(cloud_error("missing 'op'")),
        other => Err(cloud_error(&format!("unknown op '{other}'"))),
    }
}

/// Translate a Cloud request (`{op, tenant_id, params}`) into the matching ADMIN_COMMAND and
/// return the response payload. IO.cloud is the internal Fluxbee Cloud relay (spec §3): it injects
/// the (trusted for the MVP — §1.2) tenant claim into each admin call and relays over the same
/// `send_admin_rpc` seam it already uses. The op→action pairs come from the shared
/// `fluxbee_sdk::cloud::CLOUD_OP_ACTIONS` (SY.admin's relay allowlist derives from the same table):
///   create_tenant   -> create_tenant (Cloud-created tenants default to active)
///   put_token       -> vault_put     (store a provider token in the tenant pool or for one IO node)
///   provision_node  -> run_node      (spawn IO.<provider>@<tenant>; requires tenant_id)
/// For an owner-scoped token the caller must provision_node BEFORE put_token (Caveat B: the
/// `owner_node -> ilk` resolution needs the target node registered in identity SHM first). Tokens
/// needed during first boot must be stored in the tenant pool by omitting owner_node.
async fn dispatch_cloud_op(
    dispatcher: &Arc<RouterDispatcher>,
    admin_target: &str,
    hive_id: &str,
    request: &serde_json::Value,
) -> serde_json::Value {
    let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let tenant_id = request.get("tenant_id").and_then(|v| v.as_str());
    let mut params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    // get_ilk_details accepts an email selector: pre-resolve email -> canonical ilk_id from the
    // local identity SHM (O(1) probe of the (cloud, email, tenant) index — the same resolve the
    // LOCAL `get_ilk` email path uses), then relay by ilk_id exactly as before. Admin/identity
    // never learn about the email selector; the translation below stays pure.
    if op == "get_ilk_details" {
        // Exactly-one selector, mirroring the local get_ilk arm: both present is ambiguous
        // (a stale ilk_id next to a fresh email would silently win) — fail loud instead.
        if optional_trimmed(&params, "ilk_id").is_some() && optional_trimmed(&params, "email").is_some() {
            return cloud_error(
                "get_ilk_details requires exactly one of params.ilk_id (ilk:<uuid>) or params.email (+ params.tenant_id)",
            );
        }
        if let Some(email) = optional_trimmed(&params, "email") {
            // Canonical-format check (see the local get_ilk email arm): fail loud on a
            // malformed tenant instead of a silent hash-miss.
            let tenant = match require_tenant_id(
                optional_trimmed(&params, "tenant_id").as_deref(),
                "get_ilk_details by email",
            ) {
                Ok(v) => v,
                Err(e) => return e,
            };
            if !email.contains('@') || email.len() < 3 {
                return cloud_error("params.email must be a valid email address");
            }
            match fluxbee_sdk::identity::resolve_identity_option_from_hive_id_strict(
                hive_id,
                CLOUD_CHANNEL_TYPE,
                &email,
                &tenant,
            ) {
                Ok(Some(resolved)) => {
                    params["ilk_id"] = json!(resolved.ilk.ilk_id);
                }
                Ok(None) => {
                    return json!({
                        "status": "error",
                        "op": op,
                        "error_code": "ILK_NOT_FOUND",
                        "error_detail": "no ilk with a cloud channel for that email in that tenant",
                    })
                }
                Err(err) => {
                    return cloud_error_code(
                        "IDENTITY_SHM_UNAVAILABLE",
                        &format!("identity SHM read failed: {err}"),
                    )
                }
            }
        }
    }
    let request_id = request
        .get("request_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // The hive the admin action targets (MVP: the admin's own hive, e.g. motherbee).
    let hive = admin_target.split_once('@').map(|(_, h)| h.to_string());

    let (action, admin_params) = match translate_cloud_op(op, tenant_id, &params) {
        Ok(pair) => pair,
        Err(error_payload) => return error_payload,
    };

    match dispatcher
        .send_admin_rpc(AdminCommandRequest {
            admin_target,
            action,
            target: hive.as_deref(),
            params: admin_params,
            request_id,
            timeout: Duration::from_secs(if action == "run_node" { 25 } else { 20 }),
        })
        .await
    {
        Ok(res) if res.status.eq_ignore_ascii_case("ok") => {
            json!({
                "status": "ok",
                "op": op,
                "request_id": res.request_id,
                "result": res.payload,
            })
        }
        Ok(res) => json!({
            "status": "error",
            "op": op,
            "request_id": res.request_id,
            "error_code": res.error_code,
            "error_detail": res.error_detail.unwrap_or(res.payload),
        }),
        Err(e) => cloud_error(&format!("admin call failed: {e}")),
    }
}

/// Dispatch an io.cloud-LOCAL Cloud op (one of `CLOUD_LOCAL_OPS`) — io.cloud does the work itself;
/// these NEVER relay to SY.admin. `op` was already confirmed local by `is_cloud_local_op` upstream.
async fn handle_local_cloud_op(
    sender: &NodeSender,
    state: &Arc<RuntimeState>,
    msg: &Message,
    op: &str,
) -> Value {
    match op {
        "register_human" => handle_register_human(sender, state, msg).await,
        // FAST identity reads straight from the local identity SHM — NO mesh round-trip.
        "get_ilk" | "get_tenant" | "list_ilks" => handle_shm_read(state, msg, op),
        // Discovery: return the shared catalog (relay + local, with help) so Cloud knows its surface.
        "list_cloud_actions" => json!({
            "status": "ok",
            "op": "list_cloud_actions",
            "result": { "actions": cloud_action_catalog() },
        }),
        // Unreachable while CLOUD_LOCAL_OPS and this match stay in lock-step; fail-closed if a new
        // local op is declared without a handler here.
        other => cloud_error_code("INVALID_REQUEST", &format!("no handler for local op '{other}'")),
    }
}

/// The SHM subset of one ilk returned by the fast reads (get_ilk / list_ilks). The full
/// `identification` PII + channels live behind the `get_ilk_details` relay, not in this SHM view.
fn ilk_shm_subset(i: &fluxbee_sdk::identity::IdentityIlkOption) -> Value {
    json!({
        "ilk_id": i.ilk_id,
        "ilk_type": i.ilk_type,
        "registration_status": i.registration_status,
        "tenant_id": i.tenant_id,
        "display_name": i.display_name,
    })
}

/// FAST io.cloud-LOCAL identity reads served straight from the local identity SHM (io.cloud
/// co-resides with SY.identity on motherbee) — the io.api read pattern, NO mesh round-trip. They
/// return the SHM subset (identity + tenant + status + name); the full `identification` PII lives
/// behind the `get_ilk_details` relay.
fn handle_shm_read(state: &Arc<RuntimeState>, msg: &Message, op: &str) -> Value {
    use fluxbee_sdk::identity::{list_ilks_from_hive_id, tenant_exists_in_hive_id};
    let params = msg.payload.get("params").cloned().unwrap_or_else(|| json!({}));
    let hive = state.config.hive_id.as_str();
    match op {
        "get_ilk" => {
            // Two selectors, exactly one: params.ilk_id (canonical id), or params.email. The
            // email IS the address of the ilk's `cloud` channel (register_human provisions it
            // that way). With params.tenant_id the email path is an O(1) probe of the SHM
            // (channel_type, address, tenant) hash index — the io.api inbound-resolve pattern;
            // WITHOUT tenant_id it is a cross-tenant scan (the website's first-login case: the
            // email is the only datum) answering one match per tenant. The same email can exist
            // as different ilks in two tenants (identity uniqueness is per (channel, address,
            // tenant), never global) — that is what `matches` is for.
            let ilk_id = optional_trimmed(&params, "ilk_id");
            let email = optional_trimmed(&params, "email");
            match (ilk_id, email) {
                (Some(ilk_id), None) => match list_ilks_from_hive_id(hive) {
                    Ok(snap) => {
                        let found = snap.ilks.iter().find(|i| i.ilk_id == ilk_id);
                        json!({
                            "status": "ok",
                            "op": "get_ilk",
                            "result": { "exists": found.is_some(), "ilk": found.map(ilk_shm_subset) },
                        })
                    }
                    Err(err) => cloud_error_code(
                        "IDENTITY_SHM_UNAVAILABLE",
                        &format!("identity SHM read failed: {err}"),
                    ),
                },
                (None, Some(email)) => {
                    if !email.contains('@') || email.len() < 3 {
                        return cloud_error("params.email must be a valid email address");
                    }
                    // Branch on KEY PRESENCE, not on parse success: a PRESENT-but-malformed
                    // tenant_id (a number, an empty string, an object) must fail loud via
                    // require_tenant_id — never silently widen a tenant-scoped probe into the
                    // hive-wide scan (the caller meant to scope it). Only a truly absent (or
                    // null) tenant_id selects the cross-tenant mode.
                    let tenant_key_present = params
                        .get("tenant_id")
                        .is_some_and(|value| !value.is_null());
                    match tenant_key_present {
                        // TENANT-SCOPED (0.1.32): O(1) probe of the (cloud, email, tenant) SHM
                        // index. Canonical-format check first — a typo'd tenant would silently
                        // hash-miss and report exists:false; fail loud instead.
                        true => {
                            let tenant_id = match require_tenant_id(
                                optional_trimmed(&params, "tenant_id").as_deref(),
                                "get_ilk by email",
                            ) {
                                Ok(v) => v,
                                Err(e) => return e,
                            };
                            // The resolver trims + lowercases internally — same normalization
                            // the provision path applied when the channel was written.
                            match fluxbee_sdk::identity::resolve_identity_option_from_hive_id_strict(
                                hive,
                                CLOUD_CHANNEL_TYPE,
                                &email,
                                &tenant_id,
                            ) {
                                Ok(resolved) => json!({
                                    "status": "ok",
                                    "op": "get_ilk",
                                    "result": {
                                        "exists": resolved.is_some(),
                                        "ilk": resolved.map(|r| ilk_shm_subset(&r.ilk)),
                                    },
                                }),
                                Err(err) => cloud_error_code(
                                    "IDENTITY_SHM_UNAVAILABLE",
                                    &format!("identity SHM read failed: {err}"),
                                ),
                            }
                        }
                        // EMAIL-ONLY (the website's first-login case: the email is the ONLY
                        // datum it has). Cross-tenant scan of the cloud channels — the tenant
                        // is part of the SHM index key, so no O(1) probe exists without it.
                        // Shape: `matches` lists ONE ilk per tenant where that email exists
                        // (the same person can be an ilk in two companies); `ilk` is populated
                        // only when the match is unambiguous (exactly one), the common case.
                        // list_ich_options propagates SHM errors (no EACCES laundering) —
                        // fail-loud like the rest of this authoritative read API.
                        false => match fluxbee_sdk::identity::list_ich_options_from_hive_id(hive) {
                            Ok(options) => {
                                let needle = email.to_ascii_lowercase();
                                let mut candidates: Vec<fluxbee_sdk::identity::IdentityIlkOption> =
                                    Vec::new();
                                for opt in &options {
                                    if opt.channel_type != CLOUD_CHANNEL_TYPE
                                        || opt.address != needle
                                    {
                                        continue;
                                    }
                                    for ilk in &opt.ilks {
                                        if !candidates.iter().any(|c| c.ilk_id == ilk.ilk_id) {
                                            candidates.push(ilk.clone());
                                        }
                                    }
                                }
                                // ONE match per tenant, always. Transient identity states (a
                                // channel-merge alias window, an address takeover) can leave
                                // TWO active ilks on the same (cloud, email, tenant); the scan
                                // would surface both, but the tenant-scoped probe answers only
                                // the mapping winner — so re-probe each ambiguous tenant and
                                // keep the winner, making this mode return exactly what the
                                // tenant-scoped call would for every tenant listed.
                                let mut matches: Vec<Value> = Vec::new();
                                let mut tenants_done: Vec<String> = Vec::new();
                                for cand in &candidates {
                                    if tenants_done.iter().any(|t| t == &cand.tenant_id) {
                                        continue;
                                    }
                                    tenants_done.push(cand.tenant_id.clone());
                                    let dup = candidates
                                        .iter()
                                        .filter(|c| c.tenant_id == cand.tenant_id)
                                        .count()
                                        > 1;
                                    if !dup {
                                        matches.push(ilk_shm_subset(cand));
                                        continue;
                                    }
                                    match fluxbee_sdk::identity::resolve_identity_option_from_hive_id_strict(
                                        hive,
                                        CLOUD_CHANNEL_TYPE,
                                        &email,
                                        &cand.tenant_id,
                                    ) {
                                        Ok(Some(winner)) => matches.push(ilk_shm_subset(&winner.ilk)),
                                        // Mapping probe missed mid-transition: fall back to the
                                        // first scanned candidate rather than dropping the tenant.
                                        Ok(None) => matches.push(ilk_shm_subset(cand)),
                                        Err(err) => {
                                            return cloud_error_code(
                                                "IDENTITY_SHM_UNAVAILABLE",
                                                &format!("identity SHM read failed: {err}"),
                                            )
                                        }
                                    }
                                }
                                let single = (matches.len() == 1).then(|| matches[0].clone());
                                json!({
                                    "status": "ok",
                                    "op": "get_ilk",
                                    "result": {
                                        "exists": !matches.is_empty(),
                                        "ilk": single,
                                        "matches": matches,
                                    },
                                })
                            }
                            Err(err) => cloud_error_code(
                                "IDENTITY_SHM_UNAVAILABLE",
                                &format!("identity SHM read failed: {err}"),
                            ),
                        },
                    }
                }
                _ => cloud_error(
                    "get_ilk requires exactly one of params.ilk_id (ilk:<uuid>) or params.email (params.tenant_id optional: omit it to search across tenants)",
                ),
            }
        }
        "get_tenant" => {
            let tenant_id = match required_string(&params, "tenant_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let exists = tenant_exists_in_hive_id(hive, &tenant_id).unwrap_or(false);
            let ilk_count = list_ilks_from_hive_id(hive)
                .map(|snap| snap.ilks.iter().filter(|i| i.tenant_id == tenant_id).count())
                .unwrap_or(0);
            json!({
                "status": "ok",
                "op": "get_tenant",
                "result": { "exists": exists, "tenant_id": tenant_id, "ilk_count": ilk_count },
            })
        }
        "list_ilks" => {
            let tenant_id = match required_string(&params, "tenant_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            match list_ilks_from_hive_id(hive) {
                Ok(snap) => {
                    let ilks: Vec<Value> = snap
                        .ilks
                        .iter()
                        .filter(|i| i.tenant_id == tenant_id)
                        .map(ilk_shm_subset)
                        .collect();
                    json!({
                        "status": "ok",
                        "op": "list_ilks",
                        "result": { "tenant_id": tenant_id, "count": ilks.len(), "ilks": ilks },
                    })
                }
                Err(err) => cloud_error_code(
                    "IDENTITY_SHM_UNAVAILABLE",
                    &format!("identity SHM read failed: {err}"),
                ),
            }
        }
        other => cloud_error_code("INVALID_REQUEST", &format!("no SHM handler for '{other}'")),
    }
}

/// The two fields io.cloud must read off the Cloud request to relay a human registration: the
/// verbatim `frontdesk_handoff` payload it forwards, plus the tenant + email it provisions on.
#[derive(Debug)]
struct RegisterHumanRequest {
    handoff: Value,
    tenant_id: String,
    email: String,
    request_id: Option<String>,
}

/// Validate a `register_human` Cloud request and extract what io.cloud needs. io.cloud owns NO
/// format decision: it only checks the payload will reach the deterministic frontdesk path
/// (`type == frontdesk_handoff`) and carries the tenant + email required to provision. On failure it
/// returns the JSON error to relay straight back to Cloud (the flow dies there).
fn parse_register_human_request(payload: &Value) -> Result<RegisterHumanRequest, Value> {
    let Some(mut handoff) = payload.get("params").filter(|v| v.is_object()).cloned() else {
        return Err(cloud_error_code(
            "INVALID_REQUEST",
            "register_human requires an object 'params' (the frontdesk_handoff payload)",
        ));
    };
    // The gate MUST match EVERYTHING the deterministic frontdesk path requires. FrontdeskHandoffPayload
    // has no serde defaults for type / schema_version / operation, so a payload missing any of them
    // would pass a laxer gate, mint a temporary ilk, then FAIL to deserialize at the frontdesk and
    // fall silently to the conversational path (stranding the ilk). Reject up front instead.
    if handoff.get("type").and_then(Value::as_str) != Some(FRONTDESK_HANDOFF_PAYLOAD_TYPE) {
        return Err(cloud_error_code(
            "INVALID_REQUEST",
            "params.type must be \"frontdesk_handoff\"",
        ));
    }
    if handoff.get("schema_version").and_then(Value::as_u64)
        != Some(u64::from(FRONTDESK_SCHEMA_VERSION_V1))
    {
        return Err(cloud_error_code(
            "INVALID_REQUEST",
            "params.schema_version must be 1",
        ));
    }
    if handoff.get("operation").and_then(Value::as_str) != Some(FRONTDESK_OPERATION_REGISTER) {
        return Err(cloud_error_code(
            "INVALID_REQUEST",
            "params.operation must be \"complete_registration\"",
        ));
    }
    // tenant_id lives in the ROOT of the envelope, like the relay ops (io-cloud-api.md §4) — NOT in
    // params — and is validated (canonical tnt:<uuid>) BEFORE any provisioning so a bad tenant never
    // mints an orphan ilk. io.cloud injects it into the handoff below so the frontdesk gets it.
    let tenant_id = require_tenant_id(
        payload.get("tenant_id").and_then(Value::as_str),
        "register_human",
    )?;
    // Email is the human's stable unique key; identity owns idempotency on (cloud, email, tenant).
    // Require a real address so a bare token can never collide with io.cloud's own channel keys.
    let Some(email) = handoff
        .get("subject")
        .and_then(|s| s.get("email"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| v.contains('@') && v.len() >= 3)
        .map(ToString::to_string)
    else {
        return Err(cloud_error_code(
            "INVALID_REQUEST",
            "params.subject.email is required and must be an email address",
        ));
    };
    let request_id = payload
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    // Inject the root tenant_id into the handoff so the frontdesk (which reads handoff.tenant_id) gets
    // it: the Cloud envelope carries tenant_id at the root; the frontdesk_handoff contract carries it
    // inside. This is the same "root tenant → injected downstream" pattern the relay ops use.
    if let Some(obj) = handoff.as_object_mut() {
        obj.insert("tenant_id".to_string(), Value::String(tenant_id.clone()));
    }
    Ok(RegisterHumanRequest {
        handoff,
        tenant_id,
        email,
        request_id,
    })
}

/// Matcher for the frontdesk's reply: a `user`-kind FrontdeskResultPayload is success; the router's
/// UNREACHABLE / TTL_EXCEEDED (a genuinely-unreachable frontdesk) is a terminal transport error,
/// surfaced as `Err`.
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

/// Op `register_human` — the automatic Fluxbee Cloud -> frontdesk human-registration relay. UNLIKE
/// the admin-relay ops (create_tenant / put_token / provision_node), this NEVER touches SY.admin:
///   1. io.cloud mints a TEMPORARY human ilk (ILK_PROVISION — IO nodes are authorized for it), then
///   2. hands the Cloud `frontdesk_handoff` payload (VERBATIM) to the frontdesk via UNICAST to the
///      CONFIGURED registrar (`DEFAULT_FRONTDESK_TARGET` = `government.identity_frontdesk`). Unicast —
///      not the router's temporary-only Resolve force — because an explicit handoff must reach the
///      frontdesk for ANY ilk status: a repeat/idempotent registration of an already-`complete` human
///      still lands, and `ILK_REGISTER` no-ops to success. io.cloud has no target discretion (it uses
///      the one configured frontdesk); the force rule stays the safety net for IMPLICIT first-contact
///      senders (io.slack/io.wapp). The `response_envelope` makes the frontdesk emit its STRUCTURED
///      `{success, human_message, error_code}` verdict instead of plain text; a synthetic thread_id
///      satisfies the frontdesk's user-message pre-gate (there is no conversation).
/// io.cloud CONSTRUCTS no handoff — the JSON is a Cloud<->frontdesk contract; it reads only the tenant
/// and email it provisions on, and stamps the verdict it relays with the ilk_id it minted.
async fn handle_register_human(sender: &NodeSender, state: &Arc<RuntimeState>, msg: &Message) -> Value {
    // Validate the request (fail -> JSON error straight back to Cloud, flow dies) and extract the
    // fields io.cloud needs. Cloud owns the format.
    let RegisterHumanRequest {
        handoff,
        tenant_id,
        email,
        request_id,
    } = match parse_register_human_request(&msg.payload) {
        Ok(req) => req,
        Err(error) => return error,
    };

    // (1) Provision a temporary HUMAN ilk — the same primitive every IO node uses for first contact.
    let cfg = IdentityProvisionConfig {
        target: state.config.identity_target.clone(),
        timeout: Duration::from_secs(10),
    };
    let input = ResolveOrCreateInput {
        channel: CLOUD_CHANNEL_TYPE.to_string(),
        external_id: email,
        src_ilk_override: None,
        tenant_id: Some(tenant_id),
        tenant_hint: None,
        attributes: json!({ "source": "io.cloud", "role": "human" }),
        ilk_type: Some("human".to_string()),
    };
    let temp_ilk =
        match strict_provision_ilk(&state.dispatcher, &cfg, &state.config.identity_target, &input)
            .await
        {
            Ok(ilk) => ilk,
            Err(err) => {
                return cloud_error_code(
                    "IDENTITY_UNAVAILABLE",
                    &format!("could not provision the human ilk: {err}"),
                )
            }
        };

    // (2) Unicast the handoff to the configured frontdesk, asking for its structured verdict.
    let thread_id = format!(
        "thread:cloud:{}",
        request_id.as_deref().unwrap_or(temp_ilk.as_str())
    );
    let context = match set_response_envelope(
        Some(json!({ "io": { "conversation": { "thread_id": thread_id } } })),
        frontdesk_response_contract(),
    ) {
        Ok(ctx) => ctx,
        Err(err) => {
            return cloud_error_code("INTERNAL", &format!("could not build response envelope: {err}"))
        }
    };
    let message = build_user_message(
        sender.uuid(),
        Some(DEFAULT_FRONTDESK_TARGET.to_string()),
        DEFAULT_TTL,
        new_trace_id(),
        Some(temp_ilk.clone()),
        None,
        context,
        handoff,
    );
    let reply = match state
        .dispatcher
        .send_with_matcher(
            message,
            frontdesk_reply_matcher(),
            RpcRequestLabels::new(DEFAULT_FRONTDESK_TARGET, "FRONTDESK_HANDOFF", "FRONTDESK_REPLY"),
            Duration::from_secs(20),
        )
        .await
    {
        Ok(reply) => reply,
        Err(err) => {
            return cloud_error_code(
                "FRONTDESK_UNAVAILABLE",
                &format!("frontdesk handoff failed: {err}"),
            )
        }
    };

    // (3) Parse the frontdesk's structured verdict and relay it to Cloud, stamped with the ilk_id
    // io.cloud minted (the frontdesk's minimal contract omits it, but io.cloud knows it).
    if reply.payload.get("type").and_then(Value::as_str) == Some("error") {
        let detail = reply
            .payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("frontdesk rejected the request");
        return json!({
            "status": "error",
            "op": "register_human",
            "ilk_id": temp_ilk,
            "error_code": "FRONTDESK_REJECTED",
            "error_detail": detail,
        });
    }
    let structured =
        match parse_structured_response_payload(&reply.payload, &frontdesk_response_contract()) {
            Ok(map) => map,
            Err(err) => {
                return cloud_error_code(
                    "INVALID_FRONTDESK_RESPONSE",
                    &format!("could not parse frontdesk reply: {err}"),
                )
            }
        };
    let success = structured
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let human_message = structured
        .get("human_message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let error_code = structured
        .get("error_code")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    json!({
        "status": if success { "ok" } else { "error" },
        "op": "register_human",
        "ilk_id": temp_ilk,
        "registration_status": if success { "complete" } else { "temporary" },
        "success": success,
        "human_message": human_message,
        "error_code": error_code,
    })
}

impl Config {
    /// Build the boot config the io.api way: node_name + identity from the managed spawn, infra from
    /// the managed spawn config (with the generic env overrides io.api also honours), and the
    /// admin/orchestrator/identity targets derived from the hive. NO `IO_CLOUD_*` config envs.
    fn from_env() -> Result<Self, DynError> {
        let node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            format!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let hive_id = hive_from_node_name(&node_name).ok_or_else(|| {
            format!("invalid {FLUXBEE_NODE_NAME_ENV}='{node_name}': expected <name>@<hive>")
        })?;
        let spawn_cfg = load_spawn_config(&node_name)?;
        tracing::info!(path = %spawn_cfg.path.display(), "io-cloud loaded managed spawn config");
        let spawn_doc = &spawn_cfg.doc;

        Ok(Self {
            node_name,
            hive_id: hive_id.clone(),
            node_version: env("NODE_VERSION")
                .or_else(|| json_get_string(spawn_doc, "_system.runtime_version"))
                .unwrap_or_else(|| "0.1.0".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| json_get_string(spawn_doc, "node.router_socket"))
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| json_get_string(spawn_doc, "node.uuid_persistence_dir"))
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string(spawn_doc, "node.config_dir"))
                    .unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            spawn_config_path: spawn_cfg.path,
            identity_target: json_get_string(spawn_doc, "node.identity_target")
                .unwrap_or_else(|| format!("SY.identity@{hive_id}")),
            admin_target: json_get_string(spawn_doc, "node.admin_target")
                .unwrap_or_else(|| format!("SY.admin@{hive_id}")),
            orchestrator_target: json_get_string(spawn_doc, "node.orchestrator_target")
                .unwrap_or_else(|| format!("SY.orchestrator@{hive_id}")),
        })
    }
}

fn load_spawn_config(node_name: &str) -> Result<SpawnConfig, DynError> {
    let path = managed_node_config_path(node_name)
        .map_err(|err| format!("failed to resolve managed config path: {err}"))?;
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        format!(
            "failed to read managed config file {}: {err}",
            path.display()
        )
    })?;
    let doc = serde_json::from_str::<Value>(&raw).map_err(|err| {
        format!(
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
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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

    #[test]
    fn translate_cloud_op_actions_match_the_shared_sdk_vocabulary() {
        // FIX-12: io.cloud's op→action translation must agree with the single source in
        // fluxbee_sdk::cloud (which SY.admin's relay gate also derives from). Every exposed op maps
        // to its shared action, and every produced action is in the enforced allowlist.
        use fluxbee_sdk::cloud::{
            admin_action_for_cloud_op, cloud_exposed_actions, CLOUD_OP_ACTIONS,
        };
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        // Superset of every exposed op's required params, so one loop body drives all three.
        let params = json!({
            "name": "acme",
            "key": "k", "value": {"t": "1"}, "resource_type": "bearer_token",
            "node_name": "IO.wapp@motherbee",
            "ilk_id": "ilk:22222222-2222-4222-8222-222222222222",
        });
        for (op, expected_action) in CLOUD_OP_ACTIONS {
            let (action, _) = translate_cloud_op(op, Some(tenant), &params)
                .unwrap_or_else(|e| panic!("op {op} should translate: {e:?}"));
            assert_eq!(&action, expected_action, "op {op} action drift");
            assert_eq!(admin_action_for_cloud_op(op), Some(action));
            assert!(cloud_exposed_actions().contains(&action), "action {action} not exposed");
        }
        // An op outside the vocabulary is rejected, not silently relayed.
        assert!(translate_cloud_op("kill_node", Some(tenant), &params).is_err());
    }

    #[test]
    fn register_human_gate_uses_root_tenant_and_injects_it() {
        let tnt = "tnt:11111111-1111-4111-8111-111111111111";
        // The frontdesk_handoff params (tenant_id is NOT here — it lives at the envelope ROOT).
        let params = |over: Value| {
            let mut p = serde_json::Map::new();
            p.insert("type".into(), json!("frontdesk_handoff"));
            p.insert("schema_version".into(), json!(1));
            p.insert("operation".into(), json!("complete_registration"));
            p.insert(
                "subject".into(),
                json!({"display_name": "Juan Perez", "email": "juan@acme.com"}),
            );
            if let Value::Object(o) = over {
                for (k, v) in o {
                    p.insert(k, v);
                }
            }
            Value::Object(p)
        };
        let env = |tenant: Value, over: Value| {
            json!({"op": "register_human", "tenant_id": tenant, "params": params(over)})
        };

        // Non-object params.
        assert!(
            parse_register_human_request(&json!({"op": "register_human", "tenant_id": tnt})).is_err()
        );
        // Wrong type / schema_version / operation → rejected up front (frontdesk has no serde defaults).
        assert!(parse_register_human_request(&env(json!(tnt), json!({"type": "text"}))).is_err());
        assert!(parse_register_human_request(&env(json!(tnt), json!({"schema_version": 2}))).is_err());
        assert!(parse_register_human_request(&env(json!(tnt), json!({"operation": "x"}))).is_err());
        // tenant_id at the ROOT: missing or non-canonical is rejected before provisioning.
        assert!(
            parse_register_human_request(&json!({"op": "register_human", "params": params(json!({}))}))
                .is_err()
        );
        assert!(parse_register_human_request(&env(json!("tnt:acme"), json!({}))).is_err());
        // subject.email must be a real address.
        assert!(
            parse_register_human_request(&env(json!(tnt), json!({"subject": {"email": "demo"}})))
                .is_err()
        );

        // Well-formed: tenant from the root, extracted + INJECTED into the forwarded handoff.
        let ok = parse_register_human_request(&json!({
            "op": "register_human", "request_id": "req-1", "tenant_id": tnt,
            "params": {
                "type": "frontdesk_handoff", "schema_version": 1, "operation": "complete_registration",
                "subject": {"display_name": "Juan Perez", "email": "  juan@acme.com  "}
            }
        }))
        .expect("well-formed register_human parses");
        assert_eq!(ok.tenant_id, tnt);
        assert_eq!(ok.email, "juan@acme.com"); // trimmed
        assert_eq!(ok.request_id.as_deref(), Some("req-1"));
        // The root tenant_id is injected into the handoff io.cloud forwards to the frontdesk.
        assert_eq!(ok.handoff.get("tenant_id").and_then(Value::as_str), Some(tnt));
        assert_eq!(
            ok.handoff.get("operation").and_then(Value::as_str),
            Some("complete_registration")
        );
    }

    #[test]
    fn translate_put_token_injects_tenant_and_builds_vault_put() {
        let params = json!({
            "key": "wapp_token:acme",
            "value": {"token": "abc"},
            "resource_type": "bearer_token",
            "owner_node": "IO.wapp@motherbee",
            "metadata": {
                "tenant_id": "tnt:22222222-2222-4222-8222-222222222222",
                "ilk": "ilk:22222222-2222-4222-8222-222222222222",
                "owner_ilk": "ilk:33333333-3333-4333-8333-333333333333",
                "description": "WhatsApp token"
            }
        });
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let (action, p) = translate_cloud_op("put_token", Some(tenant), &params).unwrap();
        assert_eq!(action, "vault_put");
        assert_eq!(p["key"], "wapp_token:acme");
        assert_eq!(p["value"]["token"], "abc");
        assert_eq!(p["metadata"]["resource_type"], "bearer_token");
        assert_eq!(p["metadata"]["owner_node"], "IO.wapp@motherbee");
        assert_eq!(p["metadata"]["tenant_id"], tenant); // injected from the claim
        assert_eq!(p["metadata"]["description"], "WhatsApp token");
        assert!(p["metadata"].get("ilk").is_none());
        assert!(p["metadata"].get("owner_ilk").is_none());
    }

    #[test]
    fn translate_provision_node_requires_tenant_and_passes_runtime() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let params = json!({
            "node_name": "IO.wapp@motherbee",
            "runtime": "io.generic",
            "runtime_version": "current",
            "config": {"provider": "wapp", "tenant_id": "tnt:spoofed"}
        });
        let (action, p) = translate_cloud_op("provision_node", Some(tenant), &params).unwrap();
        assert_eq!(action, "run_node");
        assert_eq!(p["node_name"], "IO.wapp@motherbee");
        assert_eq!(p["tenant_id"], tenant);
        assert_eq!(p["runtime"], "io.generic");
        assert_eq!(p["runtime_version"], "current");
        assert_eq!(p["config"]["provider"], "wapp");
        assert_eq!(p["config"]["tenant_id"], tenant);
    }

    #[test]
    fn translate_create_tenant_defaults_to_active() {
        let (action, p) = translate_cloud_op(
            "create_tenant",
            None,
            &json!({"name": "Acme", "domain": "acme.example"}),
        )
        .unwrap();
        assert_eq!(action, "create_tenant");
        assert_eq!(p["name"], "Acme");
        assert_eq!(p["status"], "active");
        assert_eq!(p["domain"], "acme.example");
    }

    #[test]
    fn translate_get_ilk_details_relays_to_admin_get_ilk() {
        let (action, p) = translate_cloud_op(
            "get_ilk_details",
            None,
            &json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222"}),
        )
        .unwrap();
        assert_eq!(action, "get_ilk");
        assert_eq!(p["ilk_id"], "ilk:22222222-2222-4222-8222-222222222222");
        // Needs a canonical ilk:<uuid>.
        assert!(translate_cloud_op("get_ilk_details", None, &json!({"ilk_id": "nope"})).is_err());
        assert!(translate_cloud_op("get_ilk_details", None, &json!({})).is_err());
    }

    #[test]
    fn translate_put_token_rejects_reserved_infra_keys() {
        // A Cloud relay must not be able to overwrite an endpoint's own bearer via put_token.
        for reserved in [
            "edge_channel_secret:ich:14b66389-d425-531c-a140-a591d25e8f39",
            "edge_tls",
            "ssh:motherbee",
        ] {
            let err = translate_cloud_op(
                "put_token",
                Some("tnt:00000000-0000-0000-0000-000000000001"),
                &json!({"key": reserved, "value": {"secret": "x"}, "resource_type": "bearer_token"}),
            )
            .unwrap_err();
            assert!(
                err["error_detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("reserved infrastructure key"),
                "expected reserved-key rejection for {reserved:?}, got {err}"
            );
        }
        // A normal provider token still translates fine.
        let (action, _p) = translate_cloud_op(
            "put_token",
            Some("tnt:00000000-0000-0000-0000-000000000001"),
            &json!({"key": "slack:auth:bot", "value": {"token": "xoxb"}, "resource_type": "slack"}),
        )
        .unwrap();
        assert_eq!(action, "vault_put");
    }

    #[test]
    fn cloud_message_requires_edge_origin_and_own_ich() {
        let mut msg = Message {
            routing: Routing {
                src: "edge-uuid".to_string(),
                src_l2_name: Some("SY.edge@ingress1".to_string()),
                dst: Destination::Unicast("IO.cloud@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace".to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                ich: Some("ich:own".to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        };
        assert!(message_from_configured_edge(&msg, Some("SY.edge@ingress1")));
        assert!(authorize_cloud_message(&msg, "ich:own", Some("SY.edge@ingress1")).is_ok());
        msg.routing.src_l2_name = Some("SY.edge@ingress2".to_string());
        assert!(!message_from_configured_edge(
            &msg,
            Some("SY.edge@ingress1")
        ));
        assert!(authorize_cloud_message(&msg, "ich:own", Some("SY.edge@ingress1")).is_err());
        msg.routing.src_l2_name = Some("IO.other@motherbee".to_string());
        assert!(!message_from_configured_edge(
            &msg,
            Some("SY.edge@ingress1")
        ));
        assert!(authorize_cloud_message(&msg, "ich:own", Some("SY.edge@ingress1")).is_err());
        msg.routing.src_l2_name = Some("SY.edge@ingress1".to_string());
        msg.meta.ich = Some("ich:other".to_string());
        assert!(authorize_cloud_message(&msg, "ich:own", Some("SY.edge@ingress1")).is_err());
        assert!(!message_from_configured_edge(&msg, None));
        assert!(authorize_cloud_message(&msg, "ich:other", None).is_err());
    }

    #[test]
    fn translate_rejects_missing_tenant_unknown_and_empty_op() {
        assert!(translate_cloud_op("put_token", None, &json!({})).is_err());
        assert!(translate_cloud_op("provision_node", None, &json!({})).is_err());
        assert!(translate_cloud_op("provision_node", Some("tnt:not-a-uuid"), &json!({})).is_err());
        assert!(translate_cloud_op("", None, &json!({})).is_err());
        let err = translate_cloud_op("frobnicate", None, &json!({})).unwrap_err();
        assert_eq!(err["status"], "error");
    }
}
