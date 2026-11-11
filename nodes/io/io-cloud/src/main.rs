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
//! - it relays the bounded Cloud set (`fluxbee_sdk::cloud::CLOUD_OP_ACTIONS`) to `SY.admin` and
//!   answers the identity reads in `CLOUD_LOCAL_OPS` itself;
//! - every op on a tenant acts on the tenant at the envelope root, never the root tenant
//!   (`claimed_tenant`), and SY.admin re-checks that server-side.
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
//! Both reach the running node. `publish_cloud_endpoint` records the edge with `set_node_config`,
//! which writes the node's `config.json` (read when io.cloud starts), and then applies it with a
//! `CONFIG_SET` of the live config with that edge, at its `config_version` + 1, unless io.cloud
//! trusts that edge already (FINDINGS A-255). A `CONFIG_SET` of `io.edge_node` reaches the live
//! control plane, and the next request is judged by it.
//!
//! Its defaults do not cover its work: with no edge to trust it drops every Cloud request. So it
//! is CONFIGURED only while its config names an edge (`require_trusted_edge`, FINDINGS A-91); the
//! orchestrator's seed alone, or a config without `io.edge_node`, is UNCONFIGURED with
//! `missing_edge_node` as its reason, which NODE_STATUS_GET (DEGRADED) and CONFIG_GET both give,
//! and so does the answer to a CONFIG_SET that leaves it so.
//!
//! Two decisions that separate io.cloud from io.api:
//! - **Motherbee-only, fail-closed in the binary.** io.cloud used to be a packaged systemd
//!   unit gated by an `ExecCondition`; as a managed runtime that gate is gone, so the binary
//!   itself refuses to start on a non-motherbee hive.
//! - **Externalize is admin-driven.** io.cloud registers its own ICH at boot but does NOT
//!   self-externalize — `publish_cloud_endpoint` owns opening the public door on `SY.edge`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fluxbee_sdk::cloud::{cloud_action_catalog, is_cloud_local_op};
use fluxbee_sdk::managed_control_plane::{
    build_node_status_response_payload, ControlPlaneErrorInfo, ManagedNodeLifecycleState,
    DEFAULT_MANAGED_NODES_ROOT,
};
use fluxbee_sdk::node_config::NodeConfigSetPayload;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, Message, Meta, Routing, MSG_NODE_STATUS_GET,
    MSG_NODE_STATUS_GET_RESPONSE, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::rpc::AdminCommandRequest;
use fluxbee_sdk::vault::is_cloud_reserved_vault_key;
use fluxbee_sdk::{
    managed_node_config_path, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    PendingMatcher, RouteMatch, RouteTarget, RouterDispatcher, RpcCommandReceiver,
    RpcRequestLabels, FLUXBEE_NODE_NAME_ENV,
};
use io_common::frontdesk_contract::{
    FRONTDESK_HANDOFF_MSG, FRONTDESK_HANDOFF_PAYLOAD_TYPE, FRONTDESK_SCHEMA_VERSION_V1,
};
use io_common::frontdesk_gate::{
    frontdesk_response_contract, DEFAULT_FRONTDESK_TARGET, FRONTDESK_OPERATION_REGISTER,
};
use io_common::identity::ResolveOrCreateInput;
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_cloud_adapter_config::{trusted_edge_node, IoCloudAdapterConfigContract};
use io_common::io_context::{parse_structured_response_payload, set_response_envelope};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload,
    ensure_config_version_advances, parse_and_validate_io_control_plane_request, IoConfigSource,
    IoControlPlaneErrorInfo, IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state_with_root;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_persist_error,
    log_config_set_stale_rejected, log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::persist_io_control_plane_state;
use io_common::provision::{
    ensure_own_ich, provision_ilk_in_scope, strict_provision_ilk, IdentityProvisionConfig,
    ProvisionOutcome,
};
use io_common::router_message::{build_user_message, new_trace_id, DEFAULT_TTL};
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
    fluxbee_sdk::logging::fmt()
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
    // adapter contract via the SDK (mirrors io.api). An io.cloud with no edge is a valid node that
    // cannot do its work yet: it is UNCONFIGURED, and says it misses `io.edge_node`
    // (`boot_control_plane`, FINDINGS A-91).
    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoCloudAdapterConfigContract);
    let boot_state = boot_control_plane(
        &config.node_name,
        adapter_contract.as_ref(),
        Path::new(DEFAULT_MANAGED_NODES_ROOT),
    );

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
        last_error = ?boot_state.last_error,
        "IO.cloud starting (managed in-mesh Fluxbee Cloud adapter)"
    );

    let profile = io_cloud_route_profile()?;
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

/// The dispatcher's routes. Broad catch-all: unsolicited traffic (edge-forwarded requests under any
/// family, plus system messages incl. CONFIG_GET/SET) lands on `incoming`. Admin responses to our
/// own outbound RPCs are correlated by trace_id in the pending map first, so they never leak here.
fn io_cloud_route_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_INCOMING)
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command(RPC_CH_INCOMING))
        .build()
}

/// The control plane at boot, from the node's `config.json`: the SDK validates operator content
/// through the contract (FAILED_CONFIG, with no config in use, when it refuses it), then
/// `require_trusted_edge` decides whether io.cloud can do its work.
fn boot_control_plane(
    node_name: &str,
    contract: &dyn IoAdapterConfigContract,
    nodes_root: &Path,
) -> IoControlPlaneState {
    let mut state = bootstrap_io_control_plane_state_with_root(node_name, contract, nodes_root)
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
    require_trusted_edge(&mut state);
    state
}

/// The reason io.cloud gives while it trusts no edge (FINDINGS A-91).
const MISSING_EDGE_NODE: &str = "missing_edge_node";

/// io.cloud's rule on top of its contract (FINDINGS A-91, the operator's call: a node that does not
/// work on its defaults says what it misses). The one thing it cannot do without its config is its
/// work, relaying Cloud's requests, which it drops while no edge is trusted. So it is CONFIGURED
/// only while its config names the edge to trust (`io.edge_node`); with none, the orchestrator's
/// seed alone or a config the contract accepts without it, it is UNCONFIGURED, with
/// `missing_edge_node` as its reason. A config the contract refused stays FAILED_CONFIG, with the
/// contract's.
///
/// The reason says how the edge gets there: `publish_cloud_endpoint` records the edge it publishes
/// on and applies it to the running io.cloud at once (FINDINGS A-255), and so does a CONFIG_SET of
/// `io.edge_node`.
fn require_trusted_edge(state: &mut IoControlPlaneState) {
    if state.current_state == IoNodeLifecycleState::FailedConfig {
        return;
    }
    if trusted_edge_node(state.effective_config.as_ref()).is_some() {
        state.current_state = IoNodeLifecycleState::Configured;
        state.last_error = None;
    } else {
        state.current_state = IoNodeLifecycleState::Unconfigured;
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: MISSING_EDGE_NODE.to_string(),
            message: "IO.cloud trusts no SY.edge (config io.edge_node), so it drops every Cloud \
                      request. publish_cloud_endpoint publishes its channel on an edge and applies \
                      that edge here at once; or set the edge with CONFIG_SET io.edge_node \
                      (node_control_config_set)"
                .to_string(),
        });
    }
}

fn is_node_status_request(message: &Message) -> bool {
    is_system_kind(&message.meta.msg_type)
        && message.meta.msg.as_deref() == Some(MSG_NODE_STATUS_GET)
}

/// NODE_STATUS_GET: the SDK's managed answer for the config (FINDINGS A-91), as io.web's and
/// io.blob's: HEALTHY only while CONFIGURED, which io.cloud is while it trusts an edge; otherwise
/// DEGRADED, never ERROR, with the reason in `last_error` (`missing_edge_node`, or the contract's
/// for a refused config). The orchestrator's `get_node_status` reports `health_state` and
/// `last_error`.
fn node_status_payload(control: &IoControlPlaneState) -> Value {
    let lifecycle = match control.current_state {
        IoNodeLifecycleState::Unconfigured => ManagedNodeLifecycleState::Unconfigured,
        IoNodeLifecycleState::Configured => ManagedNodeLifecycleState::Configured,
        IoNodeLifecycleState::FailedConfig => ManagedNodeLifecycleState::FailedConfig,
    };
    let last_error = control
        .last_error
        .as_ref()
        .map(|error| ControlPlaneErrorInfo {
            code: error.code.clone(),
            message: error.message.clone(),
        });
    build_node_status_response_payload(lifecycle, last_error.as_ref())
}

/// The answer to NODE_STATUS_GET (FINDINGS A-91): `node_status_payload` of the live control plane,
/// to whoever asked, on its trace. The receive loop gives it before anything else, never through
/// the SDK's default handler, which says HEALTHY whatever the state
/// (`the_receive_loop_answers_node_status_from_the_config_state`).
fn node_status_reply(own_uuid: &str, request: &Message, control: &IoControlPlaneState) -> Message {
    Message {
        routing: Routing {
            src: own_uuid.to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request.routing.src.clone()),
            ttl: request.routing.ttl.max(1),
            trace_id: request.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_NODE_STATUS_GET_RESPONSE.to_string()),
            action: Some(MSG_NODE_STATUS_GET_RESPONSE.to_string()),
            ..Meta::default()
        },
        payload: node_status_payload(control),
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

        if is_node_status_request(&msg) {
            let reply = node_status_reply(sender.uuid(), &msg, &*state.control_plane.read().await);
            sender.send(reply).await?;
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
        // CONFIG_SET that changes `io.edge_node` takes effect on the very next message, no restart,
        // and so does the edge `publish_cloud_endpoint` publishes on, which SY.admin applies with
        // one (FINDINGS A-255).
        let effective = state.control_plane.read().await.effective_config.clone();
        let cloud_edge_node = trusted_edge_node(effective.as_ref());
        let cloud_edge_node = cloud_edge_node.as_deref();

        // RouteMatch::Any also receives unsolicited mesh notifications (for example VAULT changes).
        // They are not Cloud requests and must not get a reply from IO.cloud.
        if !message_from_configured_edge(&msg, cloud_edge_node) {
            if msg.meta.ich.as_deref() == Some(state.own_ich.as_str()) {
                // A request for its own channel from an edge it does not trust, or while it
                // trusts none (FINDINGS A-91): Cloud gets nothing and waits out the edge's 30 s,
                // so the drop is said where the operator looks (its status is HEALTHY while it
                // trusts another edge).
                tracing::warn!(
                    trace_id = %msg.routing.trace_id,
                    src_l2_name = ?msg.routing.src_l2_name,
                    trusted_edge = ?cloud_edge_node,
                    "IO.cloud dropped a request for its channel: its live config io.edge_node does \
                     not name the SY.edge it came through"
                );
            } else {
                tracing::debug!(
                    trace_id = %msg.routing.trace_id,
                    src_l2_name = ?msg.routing.src_l2_name,
                    "IO.cloud ignored a non-edge mesh message"
                );
            }
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
            // Rule 3: the tenant travels only at the envelope root. A tenant_id inside params is
            // refused rather than ignored: in get_ilk its presence picks the mode, and a client
            // still sending it there must find out at once.
            Ok(()) if tenant_in_params(&msg.payload) => {
                cloud_error("tenant_id goes at the envelope root, next to op, never inside params")
            }
            // io.cloud-LOCAL ops (CLOUD_LOCAL_OPS: register_human, list_cloud_actions) are handled
            // here — io.cloud does the work itself and NEVER touches SY.admin. The local-vs-relay
            // decision comes from the declared SDK set, not a magic string. Everything else is the
            // bounded SY.admin relay (dispatch_cloud_op, which also errors on an unknown op).
            Ok(()) if is_cloud_local_op(&op) => {
                handle_local_cloud_op(sender, state, &msg, &op).await
            }
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
            if !obj.contains_key("op") && !op.is_empty() {
                obj.insert("op".to_string(), json!(op));
            }
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
    log_config_get_served(
        &message.routing.trace_id,
        &state.config.node_name,
        &snapshot,
    );
    let mut payload = config_get_answer(
        &state.config.node_name,
        &snapshot,
        state.adapter_contract.as_ref(),
    );
    inject_runtime_status(&mut payload, state).await;
    payload
}

/// The CONFIG_GET answer: the IO family's, `ok` only while io.cloud can do its work (CONFIGURED),
/// and otherwise with the reason its status gives as `error`, `missing_edge_node` or the contract's
/// for a refused config, in place of the family's bare `node_not_configured` (FINDINGS A-91).
/// SY.admin turns a CONFIG_GET that is not `ok` into its error (`error_code`, `error_detail`).
fn config_get_answer(
    node_name: &str,
    snapshot: &IoControlPlaneState,
    contract: &dyn IoAdapterConfigContract,
) -> Value {
    let mut payload = build_io_config_get_response_payload(
        node_name,
        snapshot,
        build_io_adapter_contract_payload(contract, snapshot.effective_config.as_ref()),
    );
    if snapshot.current_state != IoNodeLifecycleState::Configured {
        payload["ok"] = json!(false);
        if let Some(error) = snapshot.last_error.as_ref() {
            payload["error"] = json!(error);
        }
    }
    payload
}

async fn apply_config_set(state: &Arc<RuntimeState>, payload: &NodeConfigSetPayload) -> Value {
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
    let next = config_set_state(payload, effective.clone());
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
    if let Some(error) = next.last_error.as_ref() {
        // Applied, and io.cloud cannot work on it: said where the operator looks, with the reason
        // its status and this answer give (FINDINGS A-91's review).
        tracing::warn!(
            node_name = %state.config.node_name,
            config_version = next.config_version,
            reason = %error.code,
            "IO.cloud applied a config that names no SY.edge (io.edge_node): it drops every \
             Cloud request until it trusts one"
        );
    }

    let mut response = config_set_answer(&state.config.node_name, &next, &hot_applied);
    inject_runtime_status(&mut response, state).await;
    response
}

/// What a CONFIG_SET leaves: the config it carries, as the contract materialized it, in use whatever
/// it names, and CONFIGURED only with an edge to trust (`require_trusted_edge`, FINDINGS A-91).
fn config_set_state(payload: &NodeConfigSetPayload, effective: Value) -> IoControlPlaneState {
    let mut next = IoControlPlaneState {
        current_state: IoNodeLifecycleState::Configured,
        config_source: IoConfigSource::Dynamic,
        schema_version: payload.schema_version,
        config_version: payload.config_version,
        effective_config: Some(effective),
        last_error: None,
    };
    require_trusted_edge(&mut next);
    next
}

/// The CONFIG_SET answer: the IO family's `ok`, since the config is applied and persisted, with its
/// hot apply; and, when that config leaves io.cloud unable to work, the reason as `error`
/// (`missing_edge_node`, what its status and CONFIG_GET give), where the family's is null (FINDINGS
/// A-91's review). SY.admin's `node_control_config_set` answers an `ok` with its 200, and gives this
/// `error` as `node_error`.
fn config_set_answer(node_name: &str, next: &IoControlPlaneState, hot_applied: &[String]) -> Value {
    let mut answer = build_io_config_set_ok_payload(node_name, next);
    answer["apply"] = json!({
        "mode": "hot_reload",
        "hot_applied": hot_applied,
        "reinit_performed": [],
        "restart_required": [],
    });
    if let Some(error) = next.last_error.as_ref() {
        answer["error"] = json!(error);
    }
    answer
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

/// Error response payload for a rejected/malformed Cloud request. Every error carries an
/// `error_code`, so Cloud maps codes and never parses details.
fn cloud_error(detail: &str) -> serde_json::Value {
    cloud_error_code("INVALID_REQUEST", detail)
}

fn cloud_error_code(code: &str, detail: &str) -> serde_json::Value {
    json!({ "status": "error", "error_code": code, "error_detail": detail })
}

/// `params.limit`, the rows of one page (FINDINGS A-198): none when absent or null; anything but a
/// whole number from 1 is refused.
fn page_limit(params: &serde_json::Value) -> Result<Option<u64>, serde_json::Value> {
    match params.get("limit").filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(limit) => limit
            .as_u64()
            .filter(|limit| *limit >= 1)
            .map(Some)
            .ok_or_else(|| cloud_error("params.limit must be an integer from 1")),
    }
}

/// `params.cursor`, the `next_cursor` of the page before (FINDINGS A-198): none when absent, null
/// or empty; anything but a string is refused.
fn page_cursor(params: &serde_json::Value) -> Result<Option<String>, serde_json::Value> {
    match params.get("cursor") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(cursor)) => {
            Ok(Some(cursor.trim().to_string()).filter(|cursor| !cursor.is_empty()))
        }
        Some(_) => Err(cloud_error(
            "params.cursor must be the next_cursor of the page before, a string",
        )),
    }
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

/// The envelope's `tenant_id`, canonical `tnt:<uuid>`: the UUID hyphenated and lowercase, the one
/// spelling the stores compare (a braced, simple or uppercase UUID parses too, but names a tenant
/// that does not exist, and slips past an exact root-tenant check).
fn require_tenant_id(tenant_id: Option<&str>, op: &str) -> Result<String, serde_json::Value> {
    let tenant_id = tenant_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| cloud_error(&format!("{op} requires tenant_id")))?;
    let valid = tenant_id
        .strip_prefix("tnt:")
        .and_then(|raw| {
            Uuid::parse_str(raw)
                .ok()
                .filter(|uuid| uuid.to_string() == raw)
        })
        .is_some();
    if !valid {
        return Err(cloud_error(&format!(
            "{op} requires canonical tenant_id tnt:<uuid>"
        )));
    }
    Ok(tenant_id.to_string())
}

/// Rule 3 of the Cloud contract: the tenant an op acts on is the one at the envelope root, and it
/// is never the root tenant, which holds the system and its nodes. Everything the op reads or
/// writes must belong to it; the stores answer anything of another tenant as missing (SY.admin
/// re-checks all of this server-side).
fn claimed_tenant(tenant_id: Option<&str>, op: &str) -> Result<String, serde_json::Value> {
    let tenant_id = require_tenant_id(tenant_id, op)?;
    if tenant_id == fluxbee_sdk::DEFAULT_ROOT_TENANT_ID {
        return Err(cloud_error_code(
            "TENANT_ROOT_PROTECTED",
            &format!("{op} cannot act on the root tenant"),
        ));
    }
    Ok(tenant_id)
}

/// Whether a request carries `tenant_id` inside `params` (it belongs at the envelope root).
fn tenant_in_params(request: &Value) -> bool {
    request
        .get("params")
        .and_then(|params| params.get("tenant_id"))
        .is_some()
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

/// The account an email belongs to (docs/ilk-tenant-model-v1.md §2.1): first the installation-wide
/// scope, where a Fluxbee Cloud account lives; then, when the request names a tenant, that tenant's
/// own scope, where an email the scope migration left ambiguous (or a core older than this node)
/// still is.
fn account_by_email(
    hive: &str,
    email: &str,
    tenant: Option<&str>,
) -> Result<
    Option<fluxbee_sdk::identity::ResolvedIdentityOption>,
    fluxbee_sdk::identity::IdentityShmError,
> {
    if let Some(found) = fluxbee_sdk::identity::resolve_identity_option_from_hive_id_strict(
        hive,
        CLOUD_CHANNEL_TYPE,
        email,
        fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
    )? {
        return Ok(Some(found));
    }
    match tenant {
        Some(tenant) if tenant != fluxbee_sdk::DEFAULT_ROOT_TENANT_ID => {
            fluxbee_sdk::identity::resolve_identity_option_from_hive_id_strict(
                hive,
                CLOUD_CHANNEL_TYPE,
                email,
                tenant,
            )
        }
        _ => Ok(None),
    }
}

/// `params.nodes`: the nodes a credential is assigned to, each a node name (`IO.slack.sales@motherbee`)
/// or an ILK id (`ilk:<uuid>`, as `list_ilks` shows them). Split for SY.admin, which resolves the
/// names and checks every node against the tenant.
fn split_nodes(
    params: &serde_json::Value,
) -> Result<(Vec<String>, Vec<String>), serde_json::Value> {
    let Some(nodes) = params.get("nodes").filter(|value| !value.is_null()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let nodes = nodes
        .as_array()
        .ok_or_else(|| cloud_error("params.nodes must be a list of node names or ilk ids"))?;
    let mut names = Vec::new();
    let mut ilks = Vec::new();
    for node in nodes {
        let node = node
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| cloud_error("params.nodes must hold non-empty strings"))?;
        if node.starts_with("ilk:") {
            ilks.push(node.to_string());
        } else {
            names.push(node.to_string());
        }
    }
    Ok((names, ilks))
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
            let tenant_id = claimed_tenant(tenant_id, "put_token")?;
            let key = required_string(params, "key")?;
            // put_token stores PROVIDER tokens; the keys that protect the mesh itself (endpoint
            // bearers, edge TLS, spoke recovery keys) are reserved for SY.* internals. Reject early
            // with a clear error — SY.admin re-enforces this server-side (single source in the SDK).
            if is_cloud_reserved_vault_key(&key) {
                return Err(cloud_error(
                    "put_token may not write a reserved infrastructure key (edge_channel_secret:*, edge_tls*, ssh:*, edge_fanout_verify:*)",
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
            // Cloud may add descriptive metadata, but tenant, ownership and assignment are authority
            // fields. In particular, accepting metadata.ilk would bypass admin's owner_node
            // resolution; the nodes come only from params.nodes, which admin checks against the
            // tenant.
            for authority_field in [
                "tenant_id",
                "ilk",
                "owner_ilk",
                "owner_l2",
                "owner_node",
                "assigned_nodes",
                "assigned_ilks",
            ] {
                metadata.remove(authority_field);
            }
            let (assigned_nodes, assigned_ilks) = split_nodes(params)?;
            if !assigned_nodes.is_empty() {
                metadata.insert("assigned_nodes".to_string(), json!(assigned_nodes));
            }
            if !assigned_ilks.is_empty() {
                metadata.insert("assigned_ilks".to_string(), json!(assigned_ilks));
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
        "assign_token" => {
            let tenant_id = claimed_tenant(tenant_id, "assign_token")?;
            let key = required_string(params, "key")?;
            if is_cloud_reserved_vault_key(&key) {
                return Err(cloud_error(
                    "assign_token may not touch a reserved infrastructure key (edge_channel_secret:*, edge_tls*, ssh:*, edge_fanout_verify:*)",
                ));
            }
            // A list, always: a missing or null one must never unassign everything by accident.
            if !params.get("nodes").is_some_and(Value::is_array) {
                return Err(cloud_error(
                    "assign_token requires params.nodes (a list; [] leaves the credential unassigned)",
                ));
            }
            let (assigned_nodes, assigned_ilks) = split_nodes(params)?;
            Ok((
                "vault_assign",
                json!({
                    "key": key,
                    "tenant_id": tenant_id,
                    "assigned_nodes": assigned_nodes,
                    "assigned_ilks": assigned_ilks,
                }),
            ))
        }
        "provision_node" => {
            let tenant_id = claimed_tenant(tenant_id, "provision_node")?;
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
            // ilk_id (translation stays pure; admin/identity need no email selector). Not
            // tenant-filtered: a tenant is a cost center, not a boundary
            // (docs/ilk-tenant-model-v1.md).
            let ilk_id = match required_string(params, "ilk_id") {
                Ok(v) => v,
                Err(_) => {
                    return Err(cloud_error(
                        "get_ilk_details requires params.ilk_id (ilk:<uuid>) or params.email",
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
        // Lifecycle: mark / restore only (purge is operator-only). Deleting an account is not
        // tenant-scoped; Cloud deletes and restores people only (operator decision 2026-10-14):
        // SY.identity answers ILK_TYPE_MISMATCH for a node's identity.
        "delete_ilk" | "restore_ilk" => {
            let ilk_id = required_string(params, "ilk_id")?;
            if !ilk_id.starts_with("ilk:") {
                return Err(cloud_error(&format!(
                    "{op} requires params.ilk_id as a canonical ilk:<uuid>"
                )));
            }
            let action = if op == "delete_ilk" {
                "delete_ilk"
            } else {
                "restore_ilk"
            };
            Ok((action, json!({ "ilk_id": ilk_id, "ilk_type": "human" })))
        }
        // Charge a person to the claimed tenant, their cost center. It grants and revokes
        // nothing; SY.identity checks the rest (a person, a temporary only out of a deleted
        // tenant, an active target, the email).
        "set_ilk_tenant" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            let ilk_id = required_string(params, "ilk_id")?;
            if !ilk_id.starts_with("ilk:") {
                return Err(cloud_error(
                    "set_ilk_tenant requires params.ilk_id as a canonical ilk:<uuid>",
                ));
            }
            Ok((
                "set_ilk_tenant",
                json!({ "ilk_id": ilk_id, "tenant_id": tenant_id }),
            ))
        }
        "delete_tenant" | "restore_tenant" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            let action = if op == "delete_tenant" {
                "delete_tenant"
            } else {
                "restore_tenant"
            };
            Ok((action, json!({ "tenant_id": tenant_id })))
        }
        // The tenant's credentials: metadata, version and use, never the value. Always filtered by
        // the claimed tenant, so the listing holds that tenant's keys and nothing else.
        "list_tokens" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            let mut filter = serde_json::Map::new();
            filter.insert("tenant_id".to_string(), json!(tenant_id));
            for field in ["resource_type", "prefix"] {
                if let Some(value) = params.get(field).filter(|value| !value.is_null()) {
                    let text = value
                        .as_str()
                        .ok_or_else(|| cloud_error(&format!("params.{field} must be a string")))?;
                    filter.insert(field.to_string(), json!(text));
                }
            }
            if let Some(limit) = params.get("limit").filter(|value| !value.is_null()) {
                let limit = limit
                    .as_u64()
                    .filter(|limit| (1..=1000).contains(limit))
                    .ok_or_else(|| cloud_error("params.limit must be an integer from 1 to 1000"))?;
                filter.insert("limit".to_string(), json!(limit));
            }
            let mut admin_params = json!({ "filter": filter });
            // The next page: the next_cursor of the page before (FINDINGS A-198), beside the filter.
            if let Some(cursor) = page_cursor(params)? {
                admin_params["cursor"] = json!(cursor);
            }
            Ok(("vault_list", admin_params))
        }
        // One credential of the claimed tenant. SY.vault answers KEY_NOT_FOUND for a key of
        // another tenant, exactly like a missing one.
        "delete_token" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            let key = required_string(params, "key")?;
            if is_cloud_reserved_vault_key(&key) {
                return Err(cloud_error(
                    "delete_token may not delete a reserved infrastructure key (edge_channel_secret:*, edge_tls*, ssh:*, edge_fanout_verify:*)",
                ));
            }
            Ok((
                "vault_delete",
                json!({ "key": key, "tenant_id": tenant_id }),
            ))
        }
        // Everything about the claimed tenant (docs/ilk-tenant-model-v1.md §2.4): name, domain,
        // status (active or suspended; pending stays the operator's), settings and sponsor.
        // Anything else is refused, never dropped in silence.
        "update_tenant" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            const FIELDS: &str =
                "params.name, params.domain, params.status, params.settings and/or params.sponsor_tenant_id";
            let Some(fields) = params.as_object() else {
                return Err(cloud_error(&format!("update_tenant requires {FIELDS}")));
            };
            if let Some(other) = fields.keys().find(|key| {
                !matches!(
                    key.as_str(),
                    "name" | "domain" | "status" | "settings" | "sponsor_tenant_id"
                )
            }) {
                return Err(cloud_error(&format!(
                    "update_tenant edits only {FIELDS}, not params.{other}"
                )));
            }
            let mut update = serde_json::Map::new();
            update.insert("tenant_id".to_string(), json!(tenant_id));
            if let Some(name) = fields.get("name") {
                let name = name
                    .as_str()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| cloud_error("params.name must be a non-empty string"))?;
                update.insert("name".to_string(), json!(name));
            }
            if let Some(domain) = fields.get("domain") {
                // null or "" clears the domain (SY.identity clears it on an empty string).
                let domain = match domain {
                    Value::Null => "",
                    Value::String(domain) => domain.trim(),
                    _ => return Err(cloud_error("params.domain must be a string or null")),
                };
                update.insert("domain".to_string(), json!(domain));
            }
            if let Some(status) = fields.get("status") {
                let status = status
                    .as_str()
                    .map(|status| status.trim().to_ascii_lowercase())
                    .filter(|status| matches!(status.as_str(), "active" | "suspended"))
                    .ok_or_else(|| {
                        cloud_error("params.status must be \"active\" or \"suspended\"")
                    })?;
                update.insert("status".to_string(), json!(status));
            }
            if let Some(settings) = fields.get("settings") {
                if !settings.is_object() {
                    return Err(cloud_error("params.settings must be an object"));
                }
                update.insert("settings".to_string(), settings.clone());
            }
            if let Some(sponsor) = fields.get("sponsor_tenant_id") {
                let sponsor = match sponsor {
                    Value::Null => Value::Null,
                    Value::String(raw) => {
                        json!(claimed_tenant(Some(raw), "update_tenant sponsor")?)
                    }
                    _ => {
                        return Err(cloud_error(
                            "params.sponsor_tenant_id must be a tenant id or null",
                        ))
                    }
                };
                update.insert("sponsor_tenant_id".to_string(), sponsor);
            }
            if update.len() == 1 {
                return Err(cloud_error(&format!("update_tenant requires {FIELDS}")));
            }
            Ok(("update_tenant", Value::Object(update)))
        }
        // The whole record of the claimed tenant, and every tenant but the genesis one (SY.admin
        // leaves it out and shows the deleted ones).
        "get_tenant" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            Ok(("get_tenant", json!({ "tenant_id": tenant_id })))
        }
        // Every tenant, or one page of them (FINDINGS A-198): `limit` and the `cursor` of the page
        // before.
        "list_tenants" => {
            let mut admin_params = json!({});
            if let Some(limit) = page_limit(params)? {
                admin_params["limit"] = json!(limit);
            }
            if let Some(cursor) = page_cursor(params)? {
                admin_params["cursor"] = json!(cursor);
            }
            Ok(("list_tenants", admin_params))
        }
        // The claimed tenant's nodes on every hive, or one page of them (FINDINGS A-198): `limit`
        // and the `cursor` of the page before.
        "list_nodes" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            let mut admin_params = json!({ "tenant_id": tenant_id });
            if let Some(limit) = page_limit(params)? {
                admin_params["limit"] = json!(limit);
            }
            if let Some(cursor) = page_cursor(params)? {
                admin_params["cursor"] = json!(cursor);
            }
            Ok(("list_tenant_nodes", admin_params))
        }
        // One IO node of the claimed tenant; start, stop and delete only the nodes Cloud launched
        // (SY.admin stamps that requirement, the hive that holds the node checks it).
        "get_node_status" | "start_node" | "stop_node" | "delete_node" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            let node_name = required_string(params, "node_name")?;
            if !node_name.starts_with("IO.") {
                return Err(cloud_error(&format!("{op} acts only on IO.* nodes")));
            }
            let mut request = json!({ "node_name": node_name, "tenant_id": tenant_id });
            let action = match op {
                "get_node_status" => "get_node_status",
                "start_node" => "start_node",
                // A stop lasts: the node stays down, with its config and identity, until started.
                "stop_node" => {
                    request["purge_instance"] = json!(false);
                    "kill_node"
                }
                // The teardown: its timers, the credentials it owns and its identity go.
                _ => {
                    request["purge_instance"] = json!(true);
                    "kill_node"
                }
            };
            Ok((action, request))
        }
        // The claimed tenant's LLM usage, one record per model call (docs/router-stream-v1.md
        // §7.6). Only the question's own fields: anything else is refused, never dropped in
        // silence. SY.admin checks the values, SY.storage the cursors.
        "get_usage" => {
            let tenant_id = claimed_tenant(tenant_id, op)?;
            const FIELDS: &str = "params.from, params.to, params.group_by, params.since, params.after and params.limit";
            let mut query = serde_json::Map::new();
            query.insert("tenant_id".to_string(), json!(tenant_id));
            for (field, value) in params.as_object().into_iter().flatten() {
                if value.is_null() {
                    continue;
                }
                match field.as_str() {
                    "from" | "to" | "group_by" | "since" | "after" => {
                        let text = value.as_str().ok_or_else(|| {
                            cloud_error(&format!("params.{field} must be a string"))
                        })?;
                        query.insert(field.clone(), json!(text));
                    }
                    "limit" => {
                        let limit = value
                            .as_u64()
                            .ok_or_else(|| cloud_error("params.limit must be a whole number"))?;
                        query.insert(field.clone(), json!(limit));
                    }
                    other => {
                        return Err(cloud_error(&format!(
                            "get_usage takes only {FIELDS}, not params.{other}"
                        )))
                    }
                }
            }
            Ok(("get_llm_usage", Value::Object(query)))
        }
        "" => Err(cloud_error("missing 'op'")),
        other => Err(cloud_error(&format!("unknown op '{other}'"))),
    }
}

/// Translate a Cloud request (`{op, tenant_id, params}`) into the matching ADMIN_COMMAND and
/// return the response payload. IO.cloud is the internal Fluxbee Cloud relay (spec §3): it injects
/// the (trusted for the MVP — §1.2) tenant claim into each admin call and relays over the same
/// `send_admin_rpc` seam it already uses. The op→action pairs come from the shared
/// `fluxbee_sdk::cloud::CLOUD_OP_ACTIONS` (SY.admin's relay allowlist derives from the same table);
/// `translate_cloud_op` rebuilds each admin payload from the fields that op takes.
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
        if optional_trimmed(&params, "ilk_id").is_some()
            && optional_trimmed(&params, "email").is_some()
        {
            return cloud_error(
                "get_ilk_details requires exactly one of params.ilk_id (ilk:<uuid>) or params.email",
            );
        }
        if let Some(email) = optional_trimmed(&params, "email") {
            // A tenant is optional here; when named it must be canonical and not the root one.
            let tenant = match tenant_id {
                Some(raw) => match claimed_tenant(Some(raw), "get_ilk_details") {
                    Ok(v) => Some(v),
                    Err(e) => return e,
                },
                None => None,
            };
            if !email.contains('@') || email.len() < 3 {
                return cloud_error("params.email must be a valid email address");
            }
            match account_by_email(hive_id, &email, tenant.as_deref()) {
                Ok(Some(resolved)) => {
                    params["ilk_id"] = json!(resolved.ilk.ilk_id);
                }
                // The same answer SY.identity gives by ilk_id.
                Ok(None) => {
                    return json!({
                        "status": "error",
                        "op": op,
                        "error_code": "NOT_FOUND",
                        "error_detail": "no account with that email",
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
            // Launching a node, tearing one down and reading the usage records (SY.admin waits up to
            // 20 s for SY.storage) take the longest; all stay under the edge's 30 s.
            timeout: Duration::from_secs(match action {
                "run_node" | "kill_node" | "get_llm_usage" => 25,
                _ => 20,
            }),
        })
        .await
    {
        Ok(res) if res.status.eq_ignore_ascii_case("ok") => {
            json!({
                "status": "ok",
                "op": op,
                "request_id": res.request_id,
                "result": cloud_view(op, res.payload),
            })
        }
        Ok(res) => json!({
            "status": "error",
            "op": op,
            "request_id": res.request_id,
            "error_code": res.error_code,
            "error_detail": res.error_detail.unwrap_or(res.payload),
        }),
        Err(e) => cloud_error_code("ADMIN_UNAVAILABLE", &format!("admin call failed: {e}")),
    }
}

/// What Fluxbee Cloud sees of a node: its state, not how the hive runs it (operator, 2026-10-15:
/// inventory and state, nothing operational). The hive's answer carries more (the process, the
/// host paths); `list_nodes` and `get_node_status` give Cloud this view. Every other op answers as
/// the hive does.
fn cloud_view(op: &str, payload: Value) -> Value {
    fn node_view(node: &Value) -> Value {
        let field = |key: &str| node.get(key).cloned().unwrap_or(Value::Null);
        // IO, AI, WF...: the first segment of the name when the hive does not say.
        let kind = node.get("kind").cloned().unwrap_or_else(|| {
            node.get("node_name")
                .and_then(Value::as_str)
                .and_then(|name| name.split('.').next())
                .map(|kind| json!(kind))
                .unwrap_or(Value::Null)
        });
        let mut view = json!({
            "node_name": field("node_name"),
            "hive": node.get("hive").or_else(|| node.get("hive_id")).cloned().unwrap_or(Value::Null),
            "kind": kind,
            "lifecycle_state": field("lifecycle_state"),
            "health_state": field("health_state"),
            "runtime": {
                "name": node.pointer("/runtime/name").cloned().unwrap_or(Value::Null),
                "version": node
                    .pointer("/runtime/version")
                    .or_else(|| node.pointer("/runtime/resolved_version"))
                    .cloned()
                    .unwrap_or(Value::Null),
            },
            "identity": field("identity"),
            "config_version": node.pointer("/config/config_version").cloned().unwrap_or(Value::Null),
        });
        for optional in [
            "launched_by",
            "relaunch_on_boot",
            "observed_at",
            "health_source",
            "last_error",
        ] {
            if let Some(value) = node.get(optional) {
                view[optional] = value.clone();
            }
        }
        view
    }
    match op {
        "list_nodes" => {
            let nodes: Vec<Value> = payload
                .get("nodes")
                .and_then(Value::as_array)
                .map(|nodes| nodes.iter().map(node_view).collect())
                .unwrap_or_default();
            let mut view = json!({
                "status": payload.get("status").cloned().unwrap_or_else(|| json!("ok")),
                "tenant_id": payload.get("tenant_id").cloned().unwrap_or(Value::Null),
                "count": nodes.len(),
                "nodes": nodes,
                "unreachable_hives": payload.get("unreachable_hives").cloned().unwrap_or_else(|| json!([])),
            });
            // One page of them (FINDINGS A-198): where the next starts, and how many there are.
            for key in ["next_cursor", "total"] {
                if let Some(value) = payload.get(key).filter(|value| !value.is_null()) {
                    view[key] = value.clone();
                }
            }
            view
        }
        "get_node_status" => match payload.get("node_status") {
            Some(status) => json!({
                "status": payload.get("status").cloned().unwrap_or_else(|| json!("ok")),
                "node_status": node_view(status),
            }),
            None => payload,
        },
        // What happened to the node, not how: no unit, signal, host path or timer report.
        "start_node" | "stop_node" | "delete_node" => {
            let mut view = serde_json::Map::new();
            for key in [
                "status",
                "state",
                "node_name",
                "hive",
                "relaunch_on_boot",
                "unit_was_active",
                "instance_purged",
            ] {
                if let Some(value) = payload.get(key) {
                    view.insert(key.to_string(), value.clone());
                }
            }
            if op == "delete_node" && !view.contains_key("state") {
                view.insert("state".to_string(), json!("deleted"));
            }
            Value::Object(view)
        }
        _ => payload,
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
        "get_ilk" | "list_ilks" => handle_shm_read(state, msg, op),
        // Discovery: return the shared catalog (relay + local, with help) so Cloud knows its surface.
        "list_cloud_actions" => json!({
            "status": "ok",
            "op": "list_cloud_actions",
            "result": { "actions": cloud_action_catalog() },
        }),
        // Unreachable while CLOUD_LOCAL_OPS and this match stay in lock-step; fail-closed if a new
        // local op is declared without a handler here.
        other => cloud_error_code(
            "INVALID_REQUEST",
            &format!("no handler for local op '{other}'"),
        ),
    }
}

/// The SHM subset of one ilk returned by the fast reads (get_ilk / list_ilks). The full
/// `identification` PII + channels live behind the `get_ilk_details` relay, not in this SHM view.
/// `pending` holds the pending signups (see `pending_signups`).
fn ilk_shm_subset(
    i: &fluxbee_sdk::identity::IdentityIlkOption,
    pending: &std::collections::HashSet<String>,
) -> Value {
    json!({
        "ilk_id": i.ilk_id,
        "ilk_type": i.ilk_type,
        "registration_status": i.registration_status,
        "tenant_id": i.tenant_id,
        "display_name": i.display_name,
        "pending_signup": pending.contains(&i.ilk_id),
    })
}

/// The pending signups, as SY.identity defines them: a temporary person holding a Fluxbee Cloud
/// account's key, the `cloud` channel in the installation-wide scope (the root tenant's). Nobody
/// finished registering them; the next `register_human` of that email takes one over
/// (docs/ilk-tenant-model-v1.md D11).
fn pending_signups(
    options: &[fluxbee_sdk::identity::IdentityIchOption],
) -> std::collections::HashSet<String> {
    options
        .iter()
        .filter(|option| {
            option.channel_type == CLOUD_CHANNEL_TYPE && fluxbee_sdk::is_root_tenant(&option.scope)
        })
        .flat_map(|option| option.ilks.iter())
        .filter(|ilk| ilk.ilk_type == "human" && ilk.registration_status == "temporary")
        .map(|ilk| ilk.ilk_id.clone())
        .collect()
}

/// FAST io.cloud-LOCAL identity reads served straight from the local identity SHM (io.cloud
/// co-resides with SY.identity on motherbee) — the io.api read pattern, NO mesh round-trip. They
/// return the SHM subset (identity + tenant + status + name); the full `identification` PII lives
/// behind the `get_ilk_details` relay.
fn handle_shm_read(state: &Arc<RuntimeState>, msg: &Message, op: &str) -> Value {
    use fluxbee_sdk::identity::list_ilks_from_hive_id;
    let params = msg
        .payload
        .get("params")
        .cloned()
        .unwrap_or_else(|| json!({}));
    // The envelope's tenant (rule 3); `tenant_in_params` already refused one inside params.
    let envelope_tenant = msg
        .payload
        .get("tenant_id")
        .filter(|value| !value.is_null());
    let hive = state.config.hive_id.as_str();
    // Which people are pending signups, for the summaries the reads below answer.
    let pending = if matches!(op, "get_ilk" | "list_ilks") {
        match fluxbee_sdk::identity::list_ich_options_from_hive_id(hive) {
            Ok(options) => pending_signups(&options),
            Err(err) => {
                return cloud_error_code(
                    "IDENTITY_SHM_UNAVAILABLE",
                    &format!("identity SHM read failed: {err}"),
                )
            }
        }
    } else {
        std::collections::HashSet::new()
    };
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
                // By id: any ilk, whatever tenant it is charged to. A tenant is a cost center,
                // not a boundary (docs/ilk-tenant-model-v1.md); the subset says which one.
                (Some(ilk_id), None) => match list_ilks_from_hive_id(hive) {
                    Ok(snap) => {
                        let found = snap.ilks.iter().find(|i| i.ilk_id == ilk_id);
                        json!({
                            "status": "ok",
                            "op": "get_ilk",
                            "result": {
                                "exists": found.is_some(),
                                "ilk": found.map(|ilk| ilk_shm_subset(ilk, &pending)),
                            },
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
                    let tenant_key_present = envelope_tenant.is_some();
                    match tenant_key_present {
                        // TENANT-SCOPED (0.1.32): O(1) probe of the (cloud, email, tenant) SHM
                        // index. Canonical-format check first — a typo'd tenant would silently
                        // hash-miss and report exists:false; fail loud instead.
                        true => {
                            let tenant_id = match claimed_tenant(
                                envelope_tenant.and_then(Value::as_str),
                                "get_ilk by email",
                            ) {
                                Ok(v) => v,
                                Err(e) => return e,
                            };
                            // The account in the installation-wide scope, else what the email
                            // still is in this tenant's own scope. The resolver trims and
                            // lowercases, as the provision did.
                            match account_by_email(hive, &email, Some(&tenant_id)) {
                                Ok(resolved) => json!({
                                    "status": "ok",
                                    "op": "get_ilk",
                                    "result": {
                                        "exists": resolved.is_some(),
                                        "ilk": resolved.map(|r| ilk_shm_subset(&r.ilk, &pending)),
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
                        // The account first: one email is one account, in the installation-wide
                        // scope. The scan below remains for an email the scope migration left
                        // ambiguous (docs/ilk-tenant-model-v1.md §2.1).
                        false => match account_by_email(hive, &email, None) {
                            Ok(Some(account)) => {
                                let ilk = ilk_shm_subset(&account.ilk, &pending);
                                json!({
                                    "status": "ok",
                                    "op": "get_ilk",
                                    "result": { "exists": true, "ilk": ilk.clone(), "matches": [ilk] },
                                })
                            }
                            Err(err) => cloud_error_code(
                                "IDENTITY_SHM_UNAVAILABLE",
                                &format!("identity SHM read failed: {err}"),
                            ),
                            Ok(None) => match fluxbee_sdk::identity::list_ich_options_from_hive_id(hive) {
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
                                        matches.push(ilk_shm_subset(cand, &pending));
                                        continue;
                                    }
                                    match fluxbee_sdk::identity::resolve_identity_option_from_hive_id_strict(
                                        hive,
                                        CLOUD_CHANNEL_TYPE,
                                        &email,
                                        &cand.tenant_id,
                                    ) {
                                        Ok(Some(winner)) => matches.push(ilk_shm_subset(&winner.ilk, &pending)),
                                        // Mapping probe missed mid-transition: fall back to the
                                        // first scanned candidate rather than dropping the tenant.
                                        Ok(None) => matches.push(ilk_shm_subset(cand, &pending)),
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
                        },
                    }
                }
                _ => cloud_error(
                    "get_ilk requires exactly one of params.ilk_id (ilk:<uuid>) or params.email (by email the envelope tenant_id is optional)",
                ),
            }
        }
        "list_ilks" => {
            let tenant_id = match claimed_tenant(envelope_tenant.and_then(Value::as_str), op) {
                Ok(v) => v,
                Err(e) => return e,
            };
            match list_ilks_from_hive_id(hive) {
                Ok(snap) => {
                    let ilks: Vec<Value> = snap
                        .ilks
                        .iter()
                        .filter(|i| i.tenant_id == tenant_id)
                        .map(|ilk| ilk_shm_subset(ilk, &pending))
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

/// An account that already exists answers here, with no frontdesk handoff and nothing created: a
/// complete person holds the email (charging them to another tenant is `set_ilk_tenant`), or a
/// person registered into the root tenant before 0.1.54 does, whom the operator must reassign or
/// purge first. A temporary goes on to the frontdesk: SY.identity has already charged it to the
/// requesting tenant.
fn existing_account_reply(outcome: &ProvisionOutcome) -> Option<Value> {
    if outcome.created || outcome.registration_status != "complete" {
        return None;
    }
    if outcome.tenant_id.as_deref() == Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID) {
        return Some(json!({
            "status": "error",
            "op": "register_human",
            "ilk_id": outcome.ilk_id,
            "error_code": "ACCOUNT_IN_ROOT_TENANT",
            "error_detail": "this email's account is charged to the root tenant; the operator must reassign or purge it",
        }));
    }
    Some(json!({
        "status": "ok",
        "op": "register_human",
        "ilk_id": outcome.ilk_id,
        "registration_status": "complete",
        "tenant_id": outcome.tenant_id,
        "existing": true,
        "success": true,
        "merged": false,
    }))
}

/// A provision SY.identity refused keeps its code (an ambiguous email, a reserved channel, a
/// deleted person); anything else is the identity service being unreachable.
fn provision_error_reply(err: &io_common::identity::IdentityError) -> Value {
    const PASSED_THROUGH: &[&str] = &[
        "ACCOUNT_AMBIGUOUS",
        "ILK_DELETED",
        "TENANT_DELETED",
        "CLOUD_CHANNEL_RESERVED",
        "INVALID_SCOPE",
    ];
    let refused = match err {
        io_common::identity::IdentityError::Other(message) => message
            .strip_prefix("provision rejected: ")
            .map(str::trim)
            .filter(|code| PASSED_THROUGH.contains(code)),
        _ => None,
    };
    if let Some(code) = refused {
        return cloud_error_code(code, &format!("SY.identity refused the account: {code}"));
    }
    cloud_error_code(
        "IDENTITY_UNAVAILABLE",
        &format!("could not provision the human ilk: {err}"),
    )
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
    // Nobody registers into the root tenant (operator decision 2026-10-02; SY.identity and the
    // frontdesk refuse it too). Refused here, before provisioning, so it never leaves a person's
    // temporary ilk, keyed by their email, in the root tenant.
    if tenant_id == fluxbee_sdk::DEFAULT_ROOT_TENANT_ID {
        return Err(cloud_error_code(
            "TENANT_ROOT_NOT_REGISTRABLE",
            "register_human cannot register a person into the root tenant",
        ));
    }
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
///
/// io.cloud CONSTRUCTS no handoff — the JSON is a Cloud<->frontdesk contract; it reads only the tenant
/// and email it provisions on, and stamps the verdict it relays with the ilk_id it minted.
async fn handle_register_human(
    sender: &NodeSender,
    state: &Arc<RuntimeState>,
    msg: &Message,
) -> Value {
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

    // (1) Provision the person's account: the `cloud` channel (the email) in the installation-wide
    // scope, so one email is one account, and the person charged to the requesting tenant
    // (docs/ilk-tenant-model-v1.md §2.1). The same primitive every IO node uses for first contact.
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
    let outcome = match provision_ilk_in_scope(
        &state.dispatcher,
        &cfg,
        &state.config.identity_target,
        &input,
        Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(err) => return provision_error_reply(&err),
    };
    if let Some(reply) = existing_account_reply(&outcome) {
        return reply;
    }
    let temp_ilk = outcome.ilk_id;

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
            return cloud_error_code(
                "INTERNAL",
                &format!("could not build response envelope: {err}"),
            )
        }
    };
    let mut message = build_user_message(
        sender.uuid(),
        Some(DEFAULT_FRONTDESK_TARGET.to_string()),
        DEFAULT_TTL,
        new_trace_id(),
        Some(temp_ilk.clone()),
        None,
        context,
        handoff,
    );
    message.meta.msg = Some(FRONTDESK_HANDOFF_MSG.to_string());
    let reply = match state
        .dispatcher
        .send_with_matcher(
            message,
            frontdesk_reply_matcher(),
            RpcRequestLabels::new(
                DEFAULT_FRONTDESK_TARGET,
                "FRONTDESK_HANDOFF",
                "FRONTDESK_REPLY",
            ),
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
    // io.cloud minted (the frontdesk's minimal contract omits it, but io.cloud knows it). An error
    // the frontdesk sends in its place reaches Cloud by its `retryable` (FINDINGS A-293).
    if reply.payload.get("type").and_then(Value::as_str) == Some("error") {
        let answer = frontdesk_error_reply(&temp_ilk, &reply.payload);
        // The frontdesk's journal has this handoff under its own trace_id, not the Cloud
        // request's: this line joins the two.
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            handoff_trace_id = %reply.routing.trace_id,
            ilk_id = %temp_ilk,
            frontdesk_code = ?reply.payload.get("code").and_then(|v| v.as_str()),
            retryable = ?reply.payload.get("retryable").and_then(|v| v.as_bool()),
            error_code = ?answer.get("error_code").and_then(|v| v.as_str()),
            "IO.cloud got an error from the frontdesk in place of its verdict"
        );
        return answer;
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
    register_human_reply(&temp_ilk, &structured)
}

/// Cloud's answer when the frontdesk sends an error in place of its verdict (FINDINGS A-293), in
/// the shape of every AI runtime error: `type`, `code`, `message`, `retryable`. The frontdesk's
/// `retryable` picks the code, never its text:
/// - `retryable: true`: nothing was done that sending the request again would repeat, and the
///   cause may pass (the frontdesk was busy). That is `FRONTDESK_UNAVAILABLE`, the code of a
///   frontdesk that does not answer within IO.cloud's 20 s, on which Cloud sends the request
///   again: one code for one action.
/// - anything else, `retryable` false or missing: `FRONTDESK_REJECTED`. The frontdesk failed on
///   this registration and does not say the same request would pass; a handler that failed may
///   have acted before it stopped.
///
/// Either way the answer names the person's ilk_id, and its detail the frontdesk's code.
fn frontdesk_error_reply(temp_ilk: &str, error: &Value) -> Value {
    let retryable = error.get("retryable").and_then(Value::as_bool) == Some(true);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("the frontdesk answered with an error");
    let detail = match error
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|code| !code.is_empty())
    {
        Some(code) => format!("{code}: {message}"),
        None => message.to_string(),
    };
    json!({
        "status": "error",
        "op": "register_human",
        "ilk_id": temp_ilk,
        "error_code": if retryable { "FRONTDESK_UNAVAILABLE" } else { "FRONTDESK_REJECTED" },
        "error_detail": detail,
    })
}

/// Cloud's answer from the frontdesk's structured verdict. `ilk_id` is where the person ended up:
/// after a merge by email, the ILK that already had the email (the temporary io.cloud provisioned
/// is then only an alias of it), reported with `merged` and `merged_from_ilk_id`.
fn register_human_reply(temp_ilk: &str, structured: &serde_json::Map<String, Value>) -> Value {
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
    let merged = success
        && structured
            .get("merged")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let ilk_id = structured
        .get("ilk_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| success && !id.is_empty())
        .unwrap_or(temp_ilk);
    let mut reply = json!({
        "status": if success { "ok" } else { "error" },
        "op": "register_human",
        "ilk_id": ilk_id,
        "registration_status": if success { "complete" } else { "temporary" },
        "success": success,
        "merged": merged,
        "human_message": human_message,
        "error_code": error_code,
    });
    if merged {
        reply["merged_from_ilk_id"] = json!(temp_ilk);
    }
    reply
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
        // Superset of every exposed op's required params, so one loop body drives them all
        // (update_tenant and get_usage take only their own fields).
        let params = json!({
            "name": "acme",
            "key": "k", "value": {"t": "1"}, "resource_type": "bearer_token",
            "node_name": "IO.wapp@motherbee",
            "ilk_id": "ilk:22222222-2222-4222-8222-222222222222",
            "nodes": [],
        });
        for (op, expected_action) in CLOUD_OP_ACTIONS {
            let op_params = match *op {
                "update_tenant" => json!({"name": "acme"}),
                "get_usage" => json!({"since": "2026-10-01T00:00:00Z"}),
                _ => params.clone(),
            };
            let (action, _) = translate_cloud_op(op, Some(tenant), &op_params)
                .unwrap_or_else(|e| panic!("op {op} should translate: {e:?}"));
            assert_eq!(&action, expected_action, "op {op} action drift");
            assert_eq!(admin_action_for_cloud_op(op), Some(action));
            assert!(
                cloud_exposed_actions().contains(&action),
                "action {action} not exposed"
            );
        }
        // An op outside the vocabulary is rejected, not silently relayed.
        assert!(translate_cloud_op("kill_node", Some(tenant), &params).is_err());
        // Purge is operator-only: never translatable from Cloud.
        assert!(translate_cloud_op("purge_ilk", Some(tenant), &params).is_err());
        assert!(translate_cloud_op("purge_tenant", Some(tenant), &params).is_err());
    }

    #[test]
    fn translate_lifecycle_ops_are_scoped_to_the_claimed_tenant() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let params = json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222",
                            "tenant_id": "tnt:99999999-9999-4999-8999-999999999999"});
        // Deleting an account is not tenant-scoped, and Cloud deletes people only.
        for op in ["delete_ilk", "restore_ilk"] {
            let (action, payload) = translate_cloud_op(op, None, &params).unwrap();
            assert_eq!(action, op);
            assert_eq!(
                payload,
                json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222", "ilk_type": "human"})
            );
        }
        let (action, payload) = translate_cloud_op("delete_tenant", Some(tenant), &params).unwrap();
        assert_eq!(action, "delete_tenant");
        assert_eq!(payload, json!({"tenant_id": tenant}));
        assert!(translate_cloud_op("delete_tenant", None, &params).is_err());
    }

    #[test]
    fn register_human_reports_the_ilk_the_person_ended_up_on() {
        let verdict = |v: Value| v.as_object().cloned().expect("object");
        let temp = "ilk:11111111-1111-4111-8111-111111111111";
        let existing = "ilk:22222222-2222-4222-8222-222222222222";

        let merged = register_human_reply(
            temp,
            &verdict(
                json!({"success": true, "human_message": "ok", "ilk_id": existing, "merged": true}),
            ),
        );
        assert_eq!(merged["ilk_id"], existing);
        assert_eq!(merged["merged"], true);
        assert_eq!(merged["merged_from_ilk_id"], temp);

        let registered = register_human_reply(
            temp,
            &verdict(
                json!({"success": true, "human_message": "ok", "ilk_id": temp, "merged": false}),
            ),
        );
        assert_eq!(registered["ilk_id"], temp);
        assert_eq!(registered["merged"], false);
        assert!(registered.get("merged_from_ilk_id").is_none());

        // A failure keeps the temporary: it is still the person's ILK.
        let failed = register_human_reply(
            temp,
            &verdict(
                json!({"success": false, "human_message": "no", "error_code": "register_failed"}),
            ),
        );
        assert_eq!(failed["ilk_id"], temp);
        assert_eq!(failed["registration_status"], "temporary");
        assert_eq!(failed["merged"], false);
    }

    #[test]
    fn register_human_refuses_the_root_tenant_before_provisioning() {
        let err = parse_register_human_request(&json!({
            "op": "register_human", "tenant_id": fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
            "params": {
                "type": "frontdesk_handoff", "schema_version": 1, "operation": "complete_registration",
                "subject": {"display_name": "Juan Perez", "email": "juan@acme.com"}
            }
        }))
        .expect_err("the root tenant takes no people");
        assert_eq!(err["status"], "error");
        assert_eq!(err["error_code"], "TENANT_ROOT_NOT_REGISTRABLE");
    }

    #[test]
    fn register_human_gate_takes_the_tenant_from_the_envelope_root_and_injects_it() {
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
        let env = |tenant: Value, over: Value| json!({"op": "register_human", "tenant_id": tenant, "params": params(over)});

        // Non-object params.
        assert!(
            parse_register_human_request(&json!({"op": "register_human", "tenant_id": tnt}))
                .is_err()
        );
        // Wrong type / schema_version / operation → rejected up front (frontdesk has no serde defaults).
        assert!(parse_register_human_request(&env(json!(tnt), json!({"type": "text"}))).is_err());
        assert!(
            parse_register_human_request(&env(json!(tnt), json!({"schema_version": 2}))).is_err()
        );
        assert!(parse_register_human_request(&env(json!(tnt), json!({"operation": "x"}))).is_err());
        // tenant_id at the ROOT: missing or non-canonical is rejected before provisioning.
        assert!(parse_register_human_request(
            &json!({"op": "register_human", "params": params(json!({}))})
        )
        .is_err());
        assert!(parse_register_human_request(&env(json!("tnt:acme"), json!({}))).is_err());
        // subject.email must be a real address.
        assert!(parse_register_human_request(&env(
            json!(tnt),
            json!({"subject": {"email": "demo"}})
        ))
        .is_err());

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
        assert_eq!(
            ok.handoff.get("tenant_id").and_then(Value::as_str),
            Some(tnt)
        );
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
    fn translate_assigns_a_credential_to_the_nodes_cloud_names() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        // put_token with nodes: names and ilk ids, split for admin; metadata cannot plant them.
        let params = json!({
            "key": "slack:acme:bot",
            "value": {"bot_token": "x"},
            "resource_type": "slack",
            "nodes": ["IO.slack.sales@motherbee", "ilk:44444444-4444-4444-8444-444444444444"],
            "metadata": {"assigned_ilks": ["ilk:55555555-5555-4555-8555-555555555555"]}
        });
        let (action, p) = translate_cloud_op("put_token", Some(tenant), &params).unwrap();
        assert_eq!(action, "vault_put");
        assert_eq!(
            p["metadata"]["assigned_nodes"],
            json!(["IO.slack.sales@motherbee"])
        );
        assert_eq!(
            p["metadata"]["assigned_ilks"],
            json!(["ilk:44444444-4444-4444-8444-444444444444"])
        );
        // assign_token replaces the list; [] unassigns; the list is required.
        let (action, p) = translate_cloud_op(
            "assign_token",
            Some(tenant),
            &json!({"key": "slack:acme:bot", "nodes": ["IO.slack.support@motherbee"]}),
        )
        .unwrap();
        assert_eq!(action, "vault_assign");
        assert_eq!(p["tenant_id"], tenant);
        assert_eq!(p["assigned_nodes"], json!(["IO.slack.support@motherbee"]));
        assert_eq!(p["assigned_ilks"], json!([]));
        let (_, p) = translate_cloud_op(
            "assign_token",
            Some(tenant),
            &json!({"key": "slack:acme:bot", "nodes": []}),
        )
        .unwrap();
        assert_eq!(p["assigned_nodes"], json!([]));
        assert!(translate_cloud_op(
            "assign_token",
            Some(tenant),
            &json!({"key": "slack:acme:bot"})
        )
        .is_err());
        assert!(translate_cloud_op(
            "assign_token",
            Some(tenant),
            &json!({"key": "k", "nodes": "x"})
        )
        .is_err());
        assert!(translate_cloud_op(
            "assign_token",
            Some(tenant),
            &json!({"key": "k", "nodes": null})
        )
        .is_err());
        assert!(translate_cloud_op(
            "assign_token",
            Some(tenant),
            &json!({"key": "edge_channel_secret:ich:x", "nodes": []})
        )
        .is_err());
        assert!(
            translate_cloud_op("assign_token", None, &json!({"key": "k", "nodes": []})).is_err()
        );
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
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let ilk = json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222"});
        // Not tenant-filtered: the same read with or without a tenant.
        for claimed in [Some(tenant), None] {
            let (action, p) = translate_cloud_op("get_ilk_details", claimed, &ilk).unwrap();
            assert_eq!(action, "get_ilk");
            assert_eq!(
                p,
                json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222"})
            );
        }
        assert!(
            translate_cloud_op("get_ilk_details", Some(tenant), &json!({"ilk_id": "nope"}))
                .is_err()
        );
        assert!(translate_cloud_op("get_ilk_details", Some(tenant), &json!({})).is_err());
    }

    #[test]
    fn every_tenant_op_takes_the_claimed_tenant_and_never_the_root() {
        let root = fluxbee_sdk::DEFAULT_ROOT_TENANT_ID;
        let params = json!({
            "key": "google:acme:sales", "value": {"t": "1"}, "resource_type": "gmail",
            "node_name": "IO.wapp@motherbee", "ilk_id": "ilk:22222222-2222-4222-8222-222222222222",
        });
        for op in [
            "put_token",
            "provision_node",
            "delete_tenant",
            "restore_tenant",
            "list_tokens",
            "delete_token",
            "update_tenant",
            "set_ilk_tenant",
        ] {
            let op_params = if op == "update_tenant" {
                json!({"name": "Acme"})
            } else {
                params.clone()
            };
            let err = translate_cloud_op(op, Some(root), &op_params).unwrap_err();
            assert_eq!(err["error_code"], "TENANT_ROOT_PROTECTED", "{op}");
            let err = translate_cloud_op(op, None, &op_params).unwrap_err();
            assert_eq!(err["error_code"], "INVALID_REQUEST", "{op}");
        }
        // Only the canonical spelling: other spellings of a UUID (the root's included) name no
        // tenant the stores know.
        for spelling in [
            "tnt:00000000000000000000000000000001",
            "tnt:{00000000-0000-0000-0000-000000000001}",
            "tnt:AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
        ] {
            let err = translate_cloud_op("delete_tenant", Some(spelling), &json!({})).unwrap_err();
            assert_eq!(err["error_code"], "INVALID_REQUEST", "{spelling}");
        }
        // create_tenant has no tenant yet.
        assert!(translate_cloud_op("create_tenant", None, &json!({"name": "Acme"})).is_ok());
        // A tenant inside params is spotted (run_loop refuses it before any op runs).
        assert!(tenant_in_params(
            &json!({"op": "get_ilk", "params": {"tenant_id": "tnt:x"}})
        ));
        assert!(!tenant_in_params(
            &json!({"op": "get_ilk", "tenant_id": "tnt:x", "params": {}})
        ));
        // Every error io.cloud writes has a code.
        assert_eq!(cloud_error("x")["error_code"], "INVALID_REQUEST");
    }

    #[test]
    fn translate_list_and_delete_tokens_stay_in_the_claimed_tenant() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let (action, p) = translate_cloud_op(
            "list_tokens",
            Some(tenant),
            &json!({"resource_type": "gmail", "prefix": "google:", "limit": 50, "ilk": "ilk:x", "tags": ["a"]}),
        )
        .unwrap();
        assert_eq!(action, "vault_list");
        // Only the tenant, resource type, prefix and limit reach the filter.
        assert_eq!(
            p,
            json!({"filter": {"tenant_id": tenant, "resource_type": "gmail", "prefix": "google:", "limit": 50}})
        );
        let (_, p) = translate_cloud_op("list_tokens", Some(tenant), &json!({})).unwrap();
        assert_eq!(p, json!({"filter": {"tenant_id": tenant}}));
        // The next page (FINDINGS A-198): the cursor beside the filter.
        let (_, p) = translate_cloud_op(
            "list_tokens",
            Some(tenant),
            &json!({"limit": 20, "cursor": "0a1b"}),
        )
        .unwrap();
        assert_eq!(
            p,
            json!({"filter": {"tenant_id": tenant, "limit": 20}, "cursor": "0a1b"})
        );
        for bad in [
            json!({"limit": 0}),
            json!({"limit": 1001}),
            json!({"limit": "10"}),
            json!({"prefix": 3}),
            json!({"cursor": 5}),
        ] {
            assert!(
                translate_cloud_op("list_tokens", Some(tenant), &bad).is_err(),
                "{bad}"
            );
        }

        let (action, p) = translate_cloud_op(
            "delete_token",
            Some(tenant),
            &json!({"key": "google:acme:sales"}),
        )
        .unwrap();
        assert_eq!(action, "vault_delete");
        assert_eq!(p, json!({"key": "google:acme:sales", "tenant_id": tenant}));
        assert!(translate_cloud_op("delete_token", Some(tenant), &json!({})).is_err());
        for reserved in [
            "edge_tls_fluxbee_ai",
            "edge_channel_secret:ich:x",
            "ssh:motherbee",
            "edge_fanout_verify:ich:x",
        ] {
            assert!(
                translate_cloud_op("delete_token", Some(tenant), &json!({"key": reserved}))
                    .is_err(),
                "{reserved}"
            );
        }
    }

    #[test]
    fn translate_update_tenant_edits_the_whole_tenant_but_pending() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let sponsor = "tnt:22222222-2222-4222-8222-222222222222";
        let (action, p) = translate_cloud_op(
            "update_tenant",
            Some(tenant),
            &json!({"name": "  Acme S.A. ", "domain": "Acme.com"}),
        )
        .unwrap();
        assert_eq!(action, "update_tenant");
        assert_eq!(
            p,
            json!({"tenant_id": tenant, "name": "Acme S.A.", "domain": "Acme.com"})
        );
        // null clears the domain (an empty string for SY.identity).
        let (_, p) =
            translate_cloud_op("update_tenant", Some(tenant), &json!({"domain": null})).unwrap();
        assert_eq!(p, json!({"tenant_id": tenant, "domain": ""}));
        // Stage 4: the status (active or suspended), the settings and the sponsor too.
        let (_, p) = translate_cloud_op(
            "update_tenant",
            Some(tenant),
            &json!({"status": "Suspended", "settings": {"plan": "pro"}, "sponsor_tenant_id": sponsor}),
        )
        .unwrap();
        assert_eq!(
            p,
            json!({"tenant_id": tenant, "status": "suspended", "settings": {"plan": "pro"},
                   "sponsor_tenant_id": sponsor})
        );
        let (_, p) = translate_cloud_op(
            "update_tenant",
            Some(tenant),
            &json!({"sponsor_tenant_id": null}),
        )
        .unwrap();
        assert_eq!(p, json!({"tenant_id": tenant, "sponsor_tenant_id": null}));
        // Nothing to change, a bad value, pending (the operator's), the root tenant as sponsor or
        // an unknown field: refused, never dropped in silence.
        for bad in [
            json!({}),
            json!({"name": "  "}),
            json!({"name": 3}),
            json!({"domain": 3}),
            json!({"status": "pending"}),
            json!({"status": 1}),
            json!({"settings": "x"}),
            json!({"sponsor_tenant_id": fluxbee_sdk::DEFAULT_ROOT_TENANT_ID}),
            json!({"sponsor_tenant_id": "tnt:NOT-CANONICAL"}),
            json!({"name": "Acme", "approved_by": "x"}),
        ] {
            assert!(
                translate_cloud_op("update_tenant", Some(tenant), &bad).is_err(),
                "{bad}"
            );
        }
    }

    /// What Cloud sees of a node is its state: the process and the host paths stay in the hive.
    #[test]
    fn cloud_sees_the_state_of_its_nodes_not_how_they_run() {
        let node = json!({
            "node_name": "IO.slack.a@motherbee", "hive": "motherbee", "kind": "IO",
            "lifecycle_state": "RUNNING", "health_state": "HEALTHY",
            "runtime": {"name": "io.slack", "version": "0.1.68"},
            "config": {"path": "/var/lib/fluxbee/nodes/IO/x/config.json", "config_version": 3},
            "state": {"path": "/var/lib/fluxbee/nodes/IO/x/state.json"},
            "identity": {"ilk_id": "ilk:a", "tenant_id": "tnt:a"},
            "launched_by": "cloud", "relaunch_on_boot": true, "managed": true
        });
        let listed = cloud_view(
            "list_nodes",
            json!({"status": "ok", "tenant_id": "tnt:a", "nodes": [node], "unreachable_hives": []}),
        );
        let seen = &listed["nodes"][0];
        assert_eq!(seen["config_version"], 3);
        assert_eq!(seen["launched_by"], "cloud");
        assert_eq!(seen["runtime"]["version"], "0.1.68");
        assert!(seen.get("config").is_none() && seen.get("state").is_none());
        let status = cloud_view(
            "get_node_status",
            json!({"status": "ok", "node_status": {
                "node_name": "IO.slack.a@motherbee", "hive_id": "motherbee",
                "lifecycle_state": "STOPPED", "health_state": "UNKNOWN",
                "runtime": {"name": "io.slack", "resolved_version": "0.1.68"},
                "process": {"pid": 42, "unit": "fluxbee-node-x"},
                "config": {"path": "/x", "config_version": 3},
                "identity": {"ilk_id": "ilk:a", "tenant_id": "tnt:a"}
            }}),
        );
        assert_eq!(status["node_status"]["hive"], "motherbee");
        assert_eq!(status["node_status"]["runtime"]["version"], "0.1.68");
        assert!(status["node_status"].get("process").is_none());
        // Starting, stopping and deleting: what happened, not the unit, signal or host paths.
        let stopped = cloud_view(
            "stop_node",
            json!({"status": "ok", "state": "stopped", "node_name": "IO.slack.a@motherbee",
                   "unit": "fluxbee-node-x", "signal": "SIGTERM", "relaunch_on_boot": false,
                   "unit_was_active": true}),
        );
        assert_eq!(
            stopped,
            json!({"status": "ok", "state": "stopped", "node_name": "IO.slack.a@motherbee",
                   "relaunch_on_boot": false, "unit_was_active": true})
        );
        let deleted = cloud_view(
            "delete_node",
            json!({"status": "ok", "node_name": "IO.slack.a@motherbee", "instance_purged": true,
                   "removed_path": "/var/lib/fluxbee/nodes/IO/x", "timer_purge": {"status": "ok"}}),
        );
        assert_eq!(deleted["state"], "deleted");
        assert!(deleted.get("removed_path").is_none() && deleted.get("timer_purge").is_none());
        // The kind comes from the name when the hive does not say it.
        assert_eq!(status["node_status"]["kind"], "IO");
        // Any other op answers as the hive does.
        assert_eq!(cloud_view("list_tokens", json!({"x": 1})), json!({"x": 1}));
    }

    /// Stage 4: the tenant reads and the node lifecycle go to the admin, scoped to the claimed
    /// tenant; stop and delete are kills, a stop that lasts and the teardown.
    #[test]
    fn translate_stage_four_ops() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        assert_eq!(
            translate_cloud_op("get_tenant", Some(tenant), &json!({})).unwrap(),
            ("get_tenant", json!({"tenant_id": tenant}))
        );
        assert_eq!(
            translate_cloud_op("list_tenants", None, &json!({})).unwrap(),
            ("list_tenants", json!({}))
        );
        // One page of them (FINDINGS A-198).
        assert_eq!(
            translate_cloud_op(
                "list_tenants",
                None,
                &json!({"limit": 50, "cursor": "tnt:a", "x": 1})
            )
            .unwrap(),
            ("list_tenants", json!({"limit": 50, "cursor": "tnt:a"}))
        );
        for bad in [
            json!({"limit": 0}),
            json!({"limit": "5"}),
            json!({"cursor": []}),
        ] {
            assert!(
                translate_cloud_op("list_tenants", None, &bad).is_err(),
                "{bad}"
            );
        }
        assert_eq!(
            translate_cloud_op("list_nodes", Some(tenant), &json!({})).unwrap(),
            ("list_tenant_nodes", json!({"tenant_id": tenant}))
        );
        // One page of them (FINDINGS A-198): only the page's params go on.
        assert_eq!(
            translate_cloud_op(
                "list_nodes",
                Some(tenant),
                &json!({"limit": 20, "cursor": "worker1|IO.a", "x": 1})
            )
            .unwrap(),
            (
                "list_tenant_nodes",
                json!({"tenant_id": tenant, "limit": 20, "cursor": "worker1|IO.a"})
            )
        );
        for bad in [
            json!({"limit": 0}),
            json!({"limit": "5"}),
            json!({"cursor": 3}),
        ] {
            assert!(
                translate_cloud_op("list_nodes", Some(tenant), &bad).is_err(),
                "{bad}"
            );
        }
        let page = cloud_view(
            "list_nodes",
            json!({"status": "ok", "tenant_id": tenant, "nodes": [], "next_cursor": "worker1|IO.a", "total": 40}),
        );
        assert_eq!(page["next_cursor"], json!("worker1|IO.a"));
        assert_eq!(page["total"], json!(40));
        let last = cloud_view("list_nodes", json!({"status": "ok", "nodes": []}));
        assert!(last.get("next_cursor").is_none());
        let node = json!({"node_name": "IO.slack.acme@motherbee"});
        assert_eq!(
            translate_cloud_op("get_node_status", Some(tenant), &node).unwrap(),
            (
                "get_node_status",
                json!({"node_name": "IO.slack.acme@motherbee", "tenant_id": tenant})
            )
        );
        assert_eq!(
            translate_cloud_op("start_node", Some(tenant), &node)
                .unwrap()
                .0,
            "start_node"
        );
        let (action, p) = translate_cloud_op("stop_node", Some(tenant), &node).unwrap();
        assert_eq!(
            (action, p["purge_instance"].clone()),
            ("kill_node", json!(false))
        );
        let (action, p) = translate_cloud_op("delete_node", Some(tenant), &node).unwrap();
        assert_eq!(
            (action, p["purge_instance"].clone()),
            ("kill_node", json!(true))
        );
        // Never a node that is not IO.*, never without the claimed tenant, never the genesis.
        for op in ["get_node_status", "start_node", "stop_node", "delete_node"] {
            assert!(
                translate_cloud_op(op, Some(tenant), &json!({"node_name": "AI.x@motherbee"}))
                    .is_err()
            );
            assert!(translate_cloud_op(op, None, &node).is_err());
            assert!(
                translate_cloud_op(op, Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID), &node).is_err()
            );
        }
        assert!(translate_cloud_op(
            "get_tenant",
            Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID),
            &json!({})
        )
        .is_err());
    }

    /// A-70 stage 3: the claimed tenant's LLM usage through SY.admin's get_llm_usage, with the
    /// question's own fields only: never without the claim, never the root tenant, another tenant
    /// or every tenant.
    #[test]
    fn translate_get_usage_reads_the_claimed_tenant_only() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let period = json!({"from": "2026-10-01T00:00:00Z", "to": "2026-11-01T00:00:00Z",
                            "group_by": "day", "limit": 50, "after": null});
        assert_eq!(
            translate_cloud_op("get_usage", Some(tenant), &period).unwrap(),
            (
                "get_llm_usage",
                json!({"tenant_id": tenant, "from": "2026-10-01T00:00:00Z",
                       "to": "2026-11-01T00:00:00Z", "group_by": "day", "limit": 50})
            )
        );
        let sync = json!({"since": "2026-10-01T00:00:00Z", "after": "cursor"});
        assert_eq!(
            translate_cloud_op("get_usage", Some(tenant), &sync)
                .unwrap()
                .1,
            json!({"tenant_id": tenant, "since": "2026-10-01T00:00:00Z", "after": "cursor"})
        );
        let refused = |tenant: Option<&str>| {
            translate_cloud_op("get_usage", tenant, &sync).unwrap_err()["error_code"].clone()
        };
        assert_eq!(refused(None), "INVALID_REQUEST");
        assert_eq!(
            refused(Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID)),
            "TENANT_ROOT_PROTECTED"
        );
        assert_eq!(refused(Some("tnt:acme")), "INVALID_REQUEST");
        for bad in [
            json!({"tenant_id": "tnt:99999999-9999-4999-8999-999999999999"}),
            json!({"all_tenants": true}),
            json!({"since": "2026-10-01T00:00:00Z", "node_name": "IO.x@motherbee"}),
            json!({"from": 20261001}),
            json!({"limit": "50"}),
            json!({"limit": -1}),
        ] {
            assert!(
                translate_cloud_op("get_usage", Some(tenant), &bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn translate_put_token_rejects_reserved_infra_keys() {
        // A Cloud relay must not be able to overwrite an endpoint's own bearer via put_token.
        for reserved in [
            "edge_channel_secret:ich:14b66389-d425-531c-a140-a591d25e8f39",
            "edge_tls",
            "edge_tls_fluxbee_ai",
            "ssh:motherbee",
            "edge_fanout_verify:ich:14b66389-d425-531c-a140-a591d25e8f39",
        ] {
            let err = translate_cloud_op(
                "put_token",
                Some("tnt:11111111-1111-4111-8111-111111111111"),
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
            Some("tnt:11111111-1111-4111-8111-111111111111"),
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

    const NODE: &str = "IO.cloud@motherbee";
    const ROOT_TENANT: &str = "tnt:00000000-0000-0000-0000-000000000001";

    /// A fresh managed-nodes root of the test's own under the system's temp dir.
    fn nodes_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "io-cloud-nodes-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create nodes root");
        dir
    }

    fn write_config(root: &Path, config: &Value) {
        let path = fluxbee_sdk::managed_node_config_path_with_root(NODE, root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, config.to_string()).unwrap();
    }

    fn boot(root: &Path) -> IoControlPlaneState {
        boot_control_plane(NODE, &IoCloudAdapterConfigContract, root)
    }

    /// FINDINGS A-91: io.cloud's defaults do not cover its work, so the orchestrator's seed alone,
    /// and no config.json at all, boot UNCONFIGURED, naming what it misses, `io.edge_node`; never
    /// a silent UNCONFIGURED. The reason says how the edge gets there: the publish applies it to
    /// the running io.cloud at once (FINDINGS A-255), as a CONFIG_SET does, so it names no restart.
    #[test]
    fn the_seed_alone_boots_unconfigured_naming_the_missing_edge() {
        let root = nodes_root("seed");
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Unconfigured);
        assert_eq!(state.effective_config, None);
        assert_eq!(
            state.last_error.as_ref().map(|error| error.code.as_str()),
            Some(MISSING_EDGE_NODE)
        );

        let seed = json!({
            "tenant_id": ROOT_TENANT,
            "_system": {"config_version": 3, "ilk_id": "ilk:11111111-2222-3333-4444-555555555555"},
        });
        write_config(&root, &seed);
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Unconfigured);
        assert_eq!(
            state.config_version, 3,
            "kept, so CONFIG_SET stays monotonic"
        );
        let error = state.last_error.expect("what it misses");
        assert_eq!(error.code, MISSING_EDGE_NODE);
        for what in [
            "io.edge_node",
            "publish_cloud_endpoint",
            "applies that edge here at once",
            "CONFIG_SET io.edge_node (node_control_config_set)",
        ] {
            assert!(error.message.contains(what), "{what}: {}", error.message);
        }
        for gone in ["restart", "only when it starts"] {
            assert!(!error.message.contains(gone), "{gone}: {}", error.message);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// What `publish_cloud_endpoint` records (`set_node_config` of `io.edge_node`, written to
    /// config.json) boots CONFIGURED on that edge. A config the contract accepts without an edge is
    /// still UNCONFIGURED, missing it, and so is a CONFIG_SET without one; a config the contract
    /// refuses is FAILED_CONFIG with the contract's reason.
    #[test]
    fn only_a_config_that_names_an_edge_is_configured() {
        let root = nodes_root("edge");
        write_config(
            &root,
            &json!({
                "io": {"edge_node": "SY.edge@ingress1"},
                "tenant_id": ROOT_TENANT,
                "_system": {"config_version": 4},
            }),
        );
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(
            trusted_edge_node(state.effective_config.as_ref()).as_deref(),
            Some("SY.edge@ingress1")
        );
        assert_eq!(state.last_error, None);

        write_config(&root, &json!({"io": {}, "_system": {"config_version": 5}}));
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Unconfigured);
        assert_eq!(state.effective_config, Some(json!({"io": {}})));
        assert_eq!(
            state.last_error.as_ref().map(|error| error.code.as_str()),
            Some(MISSING_EDGE_NODE)
        );

        write_config(
            &root,
            &json!({"io": {"edge_node": "AI.x@motherbee"}, "_system": {"config_version": 6}}),
        );
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::FailedConfig);
        assert_eq!(state.effective_config, None);
        let error = state.last_error.expect("why");
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("SY.edge"), "{}", error.message);
        let _ = std::fs::remove_dir_all(root);

        let mut set = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            config_source: IoConfigSource::Dynamic,
            schema_version: 1,
            config_version: 7,
            effective_config: Some(json!({"io": {}})),
            last_error: None,
        };
        require_trusted_edge(&mut set);
        assert_eq!(set.current_state, IoNodeLifecycleState::Unconfigured);
        assert_eq!(set.effective_config, Some(json!({"io": {}})), "applied");
        set.effective_config = Some(json!({"io": {"edge_node": "SY.edge@ingress1"}}));
        require_trusted_edge(&mut set);
        assert_eq!(set.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(set.last_error, None);
    }

    /// FINDINGS A-91: the status is HEALTHY only while io.cloud trusts an edge, where the SDK's
    /// default handler said HEALTHY always, and CONFIG_GET is `ok` only then; otherwise both give
    /// the same reason: `missing_edge_node`, or the contract's for a refused config.
    #[test]
    fn the_status_and_the_config_answer_name_what_is_missing() {
        let root = nodes_root("answers");
        let missing = boot(&root);
        let _ = std::fs::remove_dir_all(root);

        let status = node_status_payload(&missing);
        assert_eq!(status["status"], "ok");
        assert_eq!(status["health_state"], "DEGRADED");
        assert_eq!(status["lifecycle_state"], "UNCONFIGURED");
        assert_eq!(status["last_error"]["code"], MISSING_EDGE_NODE);
        let answer = config_get_answer(NODE, &missing, &IoCloudAdapterConfigContract);
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["state"], "UNCONFIGURED");
        assert_eq!(answer["error"]["code"], MISSING_EDGE_NODE);
        assert_eq!(answer["error"], status["last_error"]);

        let no_edge = IoControlPlaneState {
            effective_config: Some(json!({"io": {}})),
            config_source: IoConfigSource::Dynamic,
            ..missing.clone()
        };
        let answer = config_get_answer(NODE, &no_edge, &IoCloudAdapterConfigContract);
        assert_eq!(answer["ok"], false, "a config in use, and still no edge");
        assert_eq!(answer["error"]["code"], MISSING_EDGE_NODE);

        let refused = IoControlPlaneState {
            current_state: IoNodeLifecycleState::FailedConfig,
            last_error: Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: "io.edge_node must be an SY.edge L2 name (got 'AI.x@motherbee')"
                    .to_string(),
            }),
            ..missing.clone()
        };
        let status = node_status_payload(&refused);
        assert_eq!(status["health_state"], "DEGRADED");
        assert_eq!(status["lifecycle_state"], "FAILED_CONFIG");
        assert_eq!(status["last_error"]["code"], "invalid_config");
        let answer = config_get_answer(NODE, &refused, &IoCloudAdapterConfigContract);
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["error"]["code"], "invalid_config");

        let configured = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            config_source: IoConfigSource::OrchestratorFallback,
            effective_config: Some(json!({"io": {"edge_node": "SY.edge@ingress1"}})),
            last_error: None,
            ..missing
        };
        let status = node_status_payload(&configured);
        assert_eq!(status["health_state"], "HEALTHY");
        assert_eq!(status["lifecycle_state"], "CONFIGURED");
        assert!(status.get("last_error").is_none());
        let answer = config_get_answer(NODE, &configured, &IoCloudAdapterConfigContract);
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["state"], "CONFIGURED");
        assert_eq!(answer["error"], Value::Null);
        assert_eq!(
            answer["effective_config"]["io"]["edge_node"],
            "SY.edge@ingress1"
        );
    }

    /// FINDINGS A-91's review: a CONFIG_SET that names no edge is applied (`ok`, the config in use
    /// and persisted), and its answer says what io.cloud still misses, as its status does, where the
    /// family's answer had a null `error`; SY.admin gives it as `node_error` with its 200. One that
    /// names an edge is CONFIGURED, with no error, and says it applied the edge.
    #[test]
    fn a_config_set_without_an_edge_is_applied_and_its_answer_says_what_is_missing() {
        let set = |config: Value, config_version: u64| NodeConfigSetPayload {
            node_name: NODE.to_string(),
            config_version,
            config,
            ..NodeConfigSetPayload::default()
        };
        let contract = IoCloudAdapterConfigContract;

        let no_edge = set(json!({"io": {"inbound_family": "user"}}), 8);
        let effective = apply_adapter_config_replace(&contract, &no_edge.config).expect("valid");
        let next = config_set_state(&no_edge, effective);
        assert_eq!(next.current_state, IoNodeLifecycleState::Unconfigured);
        assert_eq!(next.config_source, IoConfigSource::Dynamic);
        assert_eq!(next.config_version, 8);
        assert_eq!(
            next.effective_config,
            Some(json!({"io": {"inbound_family": "user"}})),
            "applied"
        );
        let answer = config_set_answer(NODE, &next, &[]);
        assert_eq!(answer["ok"], true, "applied and persisted");
        assert_eq!(answer["state"], "UNCONFIGURED");
        assert_eq!(answer["config_version"], 8);
        assert_eq!(answer["error"]["code"], MISSING_EDGE_NODE);
        assert_eq!(answer["error"], node_status_payload(&next)["last_error"]);
        assert_eq!(answer["apply"]["mode"], "hot_reload");
        assert_eq!(answer["apply"]["hot_applied"], json!([]));

        let edge = set(json!({"io": {"edge_node": "SY.edge@ingress1"}}), 9);
        let effective = apply_adapter_config_replace(&contract, &edge.config).expect("valid");
        let next = config_set_state(&edge, effective);
        assert_eq!(next.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(next.last_error, None);
        let answer = config_set_answer(NODE, &next, &["io.edge_node".to_string()]);
        assert_eq!(answer["ok"], true);
        assert_eq!(answer["state"], "CONFIGURED");
        assert_eq!(answer["error"], Value::Null);
        assert_eq!(answer["apply"]["hot_applied"], json!(["io.edge_node"]));
        assert_eq!(
            answer["effective_config"]["io"]["edge_node"],
            "SY.edge@ingress1"
        );
    }

    fn status_request(trace_id: &str) -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-the-orchestrator".to_string(),
                src_l2_name: Some("SY.orchestrator@motherbee".to_string()),
                dst: Destination::Unicast(NODE.to_string()),
                ttl: 16,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_NODE_STATUS_GET.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    /// FINDINGS A-91's review: NODE_STATUS_GET is answered to whoever asked, on its trace, as
    /// NODE_STATUS_GET_RESPONSE, with the status of the live control plane it is given.
    #[test]
    fn node_status_is_answered_to_the_asker_from_the_live_state() {
        let request = status_request("trace-status-1");
        assert!(is_node_status_request(&request));
        let root = nodes_root("status-reply");
        let missing = boot(&root);
        let _ = std::fs::remove_dir_all(root);

        let reply = node_status_reply("uuid-of-io-cloud", &request, &missing);
        assert_eq!(reply.routing.src, "uuid-of-io-cloud");
        let Destination::Unicast(dst) = &reply.routing.dst else {
            panic!("a reply goes to whoever asked: {:?}", reply.routing.dst);
        };
        assert_eq!(dst, "uuid-of-the-orchestrator");
        assert_eq!(reply.routing.trace_id, "trace-status-1");
        assert_eq!(reply.meta.msg_type, SYSTEM_KIND);
        assert_eq!(
            reply.meta.msg.as_deref(),
            Some(MSG_NODE_STATUS_GET_RESPONSE)
        );
        assert_eq!(reply.payload, node_status_payload(&missing));
        assert_eq!(reply.payload["health_state"], "DEGRADED");
        assert_eq!(reply.payload["last_error"]["code"], MISSING_EDGE_NODE);

        let mut trusting = IoControlPlaneState {
            effective_config: Some(json!({"io": {"edge_node": "SY.edge@ingress1"}})),
            ..missing
        };
        require_trusted_edge(&mut trusting);
        let reply = node_status_reply("uuid-of-io-cloud", &request, &trusting);
        assert_eq!(reply.payload["health_state"], "HEALTHY");
        assert_eq!(reply.payload["lifecycle_state"], "CONFIGURED");
    }

    /// FINDINGS A-91's review: the receive loop answers NODE_STATUS_GET from the config state, as
    /// its first branch, ahead of the config commands and the edge gate, and the SDK's default
    /// handler, which says HEALTHY whatever the state, is called nowhere.
    ///
    /// It is pinned by reading the source because the property is one of ORDER, as io.slack's
    /// `control_traffic_never_reaches_the_path_out_to_slack`: a test on `node_status_reply` alone
    /// passes just as happily with the default handler put back ahead of it. The literals are
    /// split with `concat!` so this test cannot find ITSELF.
    #[test]
    fn the_receive_loop_answers_node_status_from_the_config_state() {
        let src = include_str!("main.rs");
        assert!(
            !src.contains(concat!("try_handle_default_", "node_status(")),
            "the SDK's default handler says HEALTHY whatever the config state"
        );
        let loop_at = src
            .find(concat!("async fn run_", "loop("))
            .expect("the receive loop");
        let in_loop = |what: &str| {
            loop_at
                + src[loop_at..]
                    .find(what)
                    .unwrap_or_else(|| panic!("the receive loop has {what}"))
        };
        let receive = in_loop(concat!("incoming.", "recv()"));
        let status = in_loop(concat!("if is_node_status_", "request(&msg) {"));
        let reply = in_loop(concat!("node_status_", "reply(sender.uuid(), &msg, "));
        let config = in_loop(concat!("if is_config_", "command(&msg) {"));
        let edge_gate = in_loop(concat!("if !message_from_", "configured_edge("));
        assert!(
            receive < status && status < reply && reply < config && config < edge_gate,
            "broken order: receive={receive} status={status} reply={reply} config={config} \
             edge_gate={edge_gate}"
        );
        assert!(
            !src[receive..status].contains("\n        if "),
            "nothing is decided between the receive and the status branch"
        );
    }

    /// FINDINGS A-255: the edge `publish_cloud_endpoint` publishes on reaches the running io.cloud
    /// (SY.admin applies it with a CONFIG_SET), so no text of io.cloud tells the operator to restart
    /// it after a publish any more, as `missing_edge_node`'s reason and the line logged for a
    /// dropped request did in 0.1.131. Read from the source, the literals split so this test cannot
    /// find itself.
    #[test]
    fn no_text_of_io_cloud_asks_for_a_restart_after_a_publish() {
        let src = include_str!("main.rs");
        for gone in [
            concat!("restart IO", ".cloud"),
            concat!("trusted after its ", "restart"),
            concat!("reads only when it ", "starts"),
            concat!("(restart", "_node)"),
        ] {
            assert!(!src.contains(gone), "{gone}");
        }
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

    #[test]
    fn translate_set_ilk_tenant_names_the_target_tenant() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let (action, p) = translate_cloud_op(
            "set_ilk_tenant",
            Some(tenant),
            &json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222"}),
        )
        .unwrap();
        assert_eq!(action, "set_ilk_tenant");
        assert_eq!(
            p,
            json!({"ilk_id": "ilk:22222222-2222-4222-8222-222222222222", "tenant_id": tenant})
        );
        assert!(
            translate_cloud_op("set_ilk_tenant", Some(tenant), &json!({"ilk_id": "nope"})).is_err()
        );
    }

    #[test]
    fn an_existing_account_answers_without_a_handoff() {
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let outcome = |created: bool, status: &str, charged: &str| ProvisionOutcome {
            ilk_id: "ilk:22222222-2222-4222-8222-222222222222".to_string(),
            created,
            registration_status: status.to_string(),
            tenant_id: Some(charged.to_string()),
        };
        // A complete account: existing, charged where it is, nothing created.
        let reply = existing_account_reply(&outcome(false, "complete", tenant)).expect("existing");
        assert_eq!(reply["existing"], true);
        assert_eq!(reply["tenant_id"], tenant);
        assert_eq!(reply["registration_status"], "complete");
        // Charged to the root tenant (registered before 0.1.54): the operator first.
        let reply = existing_account_reply(&outcome(
            false,
            "complete",
            fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
        ))
        .expect("refused");
        assert_eq!(reply["error_code"], "ACCOUNT_IN_ROOT_TENANT");
        // New, or a temporary: on to the frontdesk.
        assert!(existing_account_reply(&outcome(true, "temporary", tenant)).is_none());
        assert!(existing_account_reply(&outcome(false, "temporary", tenant)).is_none());
    }

    #[test]
    fn a_refused_account_keeps_its_code() {
        use io_common::identity::IdentityError;
        let reply = provision_error_reply(&IdentityError::Other(
            "provision rejected: ACCOUNT_AMBIGUOUS".to_string(),
        ));
        assert_eq!(reply["error_code"], "ACCOUNT_AMBIGUOUS");
        let reply = provision_error_reply(&IdentityError::Other(
            "provision rejected: SOMETHING_ELSE".to_string(),
        ));
        assert_eq!(reply["error_code"], "IDENTITY_UNAVAILABLE");
        assert_eq!(
            provision_error_reply(&IdentityError::Timeout)["error_code"],
            "IDENTITY_UNAVAILABLE"
        );
    }

    #[test]
    fn a_pending_signup_is_a_temporary_person_holding_an_account_key() {
        use fluxbee_sdk::identity::{IdentityIchOption, IdentityIlkOption};
        let person = |ilk_id: &str, status: &str| IdentityIlkOption {
            ilk_id: ilk_id.to_string(),
            tenant_id: "tnt:11111111-1111-4111-8111-111111111111".to_string(),
            display_name: None,
            handler_node: None,
            registration_status: status.to_string(),
            ilk_type: "human".to_string(),
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
        };
        let channel = |channel_type: &str, scope: &str, ilk: IdentityIlkOption| IdentityIchOption {
            ich_id: format!("ich:{}", uuid::Uuid::new_v4()),
            channel_type: channel_type.to_string(),
            address: "someone@acme.com".to_string(),
            scope: scope.to_string(),
            is_primary: false,
            owner_l2_name: None,
            enabled: false,
            ilks: vec![ilk],
        };
        let root = fluxbee_sdk::DEFAULT_ROOT_TENANT_ID;
        let tenant = "tnt:11111111-1111-4111-8111-111111111111";
        let options = vec![
            channel("cloud", root, person("ilk:pending", "temporary")),
            channel("cloud", root, person("ilk:account", "complete")),
            channel("cloud", tenant, person("ilk:old-scope", "temporary")),
            channel("slack", tenant, person("ilk:wrote-in", "temporary")),
        ];
        let pending = pending_signups(&options);
        assert_eq!(
            pending,
            std::collections::HashSet::from(["ilk:pending".to_string()])
        );
        let summary = ilk_shm_subset(&person("ilk:pending", "temporary"), &pending);
        assert_eq!(summary["pending_signup"], true);
        assert_eq!(
            ilk_shm_subset(&person("ilk:account", "complete"), &pending)["pending_signup"],
            false
        );
    }

    const TEST_TENANT: &str = "tnt:11111111-1111-4111-8111-111111111111";
    /// The temporary ILK SY.identity gives the person in the tests through the handler.
    const TEST_TEMP_ILK: &str = "ilk:33333333-3333-4333-8333-333333333333";

    /// An io.cloud whose router is the test: the SDK's in-process harness, with the receive loop's
    /// routes. The test plays SY.identity and the frontdesk.
    fn io_cloud_on_a_test_router() -> (Arc<RuntimeState>, fluxbee_sdk::RouterDispatcherTestHarness)
    {
        let (dispatcher, router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "IO.cloud@motherbee",
            io_cloud_route_profile().expect("the routes"),
        );
        let control_plane = IoControlPlaneState::default();
        let state = Arc::new(RuntimeState {
            config: Config {
                node_name: "IO.cloud@motherbee".to_string(),
                hive_id: MOTHERBEE_HIVE.to_string(),
                node_version: "test".to_string(),
                router_socket: PathBuf::new(),
                uuid_persistence_dir: PathBuf::new(),
                config_dir: PathBuf::new(),
                spawn_config_path: PathBuf::new(),
                identity_target: "SY.identity@motherbee".to_string(),
                admin_target: "SY.admin@motherbee".to_string(),
                orchestrator_target: "SY.orchestrator@motherbee".to_string(),
            },
            own_ich: "ich:14b66389-d425-531c-a140-a591d25e8f39".to_string(),
            control_metrics: Arc::new(IoControlPlaneMetrics::with_initial_state(
                control_plane.current_state.as_str(),
                control_plane.config_version,
            )),
            control_plane: Arc::new(RwLock::new(control_plane)),
            adapter_contract: Arc::new(IoCloudAdapterConfigContract),
            dispatcher,
        });
        (state, router)
    }

    /// A peer's answer to one of io.cloud's requests: the same trace, back to io.cloud.
    fn answer_to(request: &Message, msg_type: &str, msg: Option<&str>, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-the-peer".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(request.routing.src.clone()),
                ttl: 16,
                trace_id: request.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: msg.map(ToString::to_string),
                ..Meta::default()
            },
            payload,
        }
    }

    /// One `register_human` through the handler: the test answers SY.identity's ILK_PROVISION with
    /// a new temporary person, then the handoff with `frontdesk_answer`, and gives Cloud's answer.
    async fn register_human_answered_by(frontdesk_answer: Value) -> Value {
        let (state, mut router) = io_cloud_on_a_test_router();
        let sender = state.dispatcher.sender_snapshot();
        let request = Message {
            routing: Routing {
                src: "uuid-of-the-edge".to_string(),
                src_l2_name: Some("SY.edge@ingress1".to_string()),
                dst: Destination::Unicast("IO.cloud@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-of-the-cloud-request".to_string(),
            },
            meta: Meta {
                msg_type: CLOUD_INBOUND_FAMILY.to_string(),
                ..Meta::default()
            },
            payload: json!({
                "op": "register_human", "tenant_id": TEST_TENANT, "request_id": "req-a293",
                "params": {
                    "type": "frontdesk_handoff", "schema_version": 1,
                    "operation": "complete_registration",
                    "subject": {"display_name": "Juan Perez", "email": "juan@acme.com"}
                }
            }),
        };
        let peers = async {
            let provision = router
                .next_outgoing_within(Duration::from_secs(5))
                .await
                .expect("io.cloud provisions the person first");
            assert_eq!(
                provision.meta.msg.as_deref(),
                Some(fluxbee_sdk::MSG_ILK_PROVISION)
            );
            router
                .inject(answer_to(
                    &provision,
                    SYSTEM_KIND,
                    Some(fluxbee_sdk::MSG_ILK_PROVISION_RESPONSE),
                    json!({"status": "ok", "ilk_id": TEST_TEMP_ILK, "created": true,
                           "registration_status": "temporary", "tenant_id": TEST_TENANT}),
                ))
                .await
                .expect("SY.identity answers");
            let handoff = router
                .next_outgoing_within(Duration::from_secs(5))
                .await
                .expect("then io.cloud hands the person to the frontdesk");
            assert_eq!(handoff.meta.msg.as_deref(), Some(FRONTDESK_HANDOFF_MSG));
            assert!(matches!(
                &handoff.routing.dst,
                Destination::Unicast(target) if target == DEFAULT_FRONTDESK_TARGET
            ));
            router
                .inject(answer_to(&handoff, USER_KIND, None, frontdesk_answer))
                .await
                .expect("the frontdesk answers");
        };
        let (answer, ()) = tokio::join!(handle_register_human(&sender, &state, &request), peers);
        answer
    }

    /// FINDINGS A-293, through the handler: an error the frontdesk sends in place of its verdict
    /// reaches Cloud by its `retryable`. A frontdesk too busy to run the handoff (the AI runtime's
    /// `ai_runtime_busy`: retryable, nothing run) is FRONTDESK_UNAVAILABLE, the code Cloud sends the
    /// request again on, where it was FRONTDESK_REJECTED, a final refusal; a handler that failed
    /// (`ai_runtime_error`, not retryable) stays FRONTDESK_REJECTED. Both keep the person's ILK and
    /// name the frontdesk's code, and the frontdesk's verdict is relayed as before.
    #[tokio::test]
    async fn a_frontdesk_error_reaches_cloud_by_its_retryable() {
        let busy = register_human_answered_by(json!({
            "type": "error",
            "code": "ai_runtime_busy",
            "message": "The AI runtime is busy: its 4 model workers are all taken and 128 more \
                        messages wait for them. This message was not run; send it again later.",
            "retryable": true
        }))
        .await;
        assert_eq!(busy["status"], "error");
        assert_eq!(busy["op"], "register_human");
        assert_eq!(busy["error_code"], "FRONTDESK_UNAVAILABLE");
        assert_eq!(busy["ilk_id"], TEST_TEMP_ILK);
        assert!(
            busy["error_detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("ai_runtime_busy: The AI runtime is busy")),
            "{busy}"
        );

        let failed = register_human_answered_by(json!({
            "type": "error",
            "code": "ai_runtime_error",
            "message": "The AI runtime failed while processing this message.",
            "retryable": false
        }))
        .await;
        assert_eq!(failed["status"], "error");
        assert_eq!(failed["error_code"], "FRONTDESK_REJECTED");
        assert_eq!(failed["ilk_id"], TEST_TEMP_ILK);
        assert_eq!(
            failed["error_detail"],
            "ai_runtime_error: The AI runtime failed while processing this message."
        );

        let registered = register_human_answered_by(json!({
            "type": "text",
            "content": json!({
                "success": true,
                "human_message": "Registration completed successfully.",
                "ilk_id": TEST_TEMP_ILK,
                "merged": false
            })
            .to_string()
        }))
        .await;
        assert_eq!(registered["status"], "ok");
        assert_eq!(registered["registration_status"], "complete");
        assert_eq!(registered["ilk_id"], TEST_TEMP_ILK);
    }

    /// FINDINGS A-293: only the frontdesk's own `retryable: true` makes an error one to send again;
    /// an error that does not say so is final, as every error was before.
    #[test]
    fn only_an_error_the_frontdesk_marks_retryable_is_unavailable() {
        // The frontdesk's error with this `retryable`, or none when it is absent.
        let code = |retryable: Option<Value>| {
            let mut error =
                json!({"type": "error", "code": "provider_unreachable", "message": "m"});
            if let Some(retryable) = retryable {
                error["retryable"] = retryable;
            }
            frontdesk_error_reply(TEST_TEMP_ILK, &error)["error_code"].clone()
        };
        assert_eq!(code(Some(json!(true))), "FRONTDESK_UNAVAILABLE");
        for not_retryable in [
            Some(json!(false)),
            None,
            Some(json!("true")),
            Some(Value::Null),
        ] {
            assert_eq!(
                code(not_retryable.clone()),
                "FRONTDESK_REJECTED",
                "{not_retryable:?}"
            );
        }
        // With neither a code nor a message, the detail still says what happened.
        let bare = json!({"type": "error", "retryable": true});
        let bare = frontdesk_error_reply(TEST_TEMP_ILK, &bare);
        assert_eq!(bare["error_detail"], "the frontdesk answered with an error");
        assert_eq!(bare["ilk_id"], TEST_TEMP_ILK);
    }
}
