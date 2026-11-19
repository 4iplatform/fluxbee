#![forbid(unsafe_code)]

//! `IO.web` is the motherbee's web node behind the edge (docs/io-web-v2.md §11.2 item 3, §12, §13;
//! docs/io-web-v2-stage-2-1.md, docs/io-web-v2-stage-2-2.md; the operator's page is
//! docs/io/io-web.md).
//!
//! The edge on the ingress proxies each exposure's host to it over the mesh's mutual TLS, on
//! `fluxbee_sdk::mesh::IO_WEB_PORT` (`web`). It holds the exposures SY.admin pushes and their keys
//! (`exposures`), mints and redeems their one-time links (`access`), and is a transparent reverse
//! proxy for a valid session: to its exposure's upstream (`proxy`, `upstream`), with §13.7's
//! headers (`headers`), cutting what is open once the exposure is left without access (`open`). The
//! exposures' two commands come on a channel of their own, from `SY.admin@motherbee` alone
//! (`run_exposure_commands`).
//!
//! Like io.blob and io.cloud it is a FIRST-CLASS MANAGED IO NODE in the root tenant: only
//! `IO.web@motherbee`, in the root tenant, by a fail-closed self-check in the binary
//! (`ensure_canonical_instance`); CONFIG_GET/SET through the common IO control plane;
//! NODE_STATUS_GET; its log in its unit's journal; a runtime the core ships and moves to each new
//! version. Unlike them it does not boot with the install (`boot: false` in
//! packaging/base-nodes.json): the operator starts it once, `run_node IO.web@motherbee` in the
//! root tenant.
//!
//! Configuration model: the contract (`IoWebAdapterConfigContract`) has no field in this stage, so
//! the orchestrator's seed alone is io.web's whole config and it boots CONFIGURED on its defaults
//! (`boot_control_plane`). The port opens once the config is in use: a config the contract refuses
//! leaves io.web FAILED_CONFIG, serving nobody, until a CONFIG_SET it accepts.

mod access;
mod exposures;
mod headers;
mod open;
mod proxy;
#[cfg(test)]
mod testing;
mod upstream;
mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fluxbee_sdk::managed_control_plane::{
    build_node_status_response_payload, ControlPlaneErrorInfo, ManagedNodeLifecycleState,
    DEFAULT_MANAGED_NODES_ROOT,
};
use fluxbee_sdk::node_config::NodeConfigSetPayload;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, IoWebAccessLinkRequest, IoWebAccessLinkResponse,
    IoWebExposuresPayload, IoWebExposuresResponse, Message, Meta, Routing,
    IO_WEB_ERROR_UNAUTHORIZED, MSG_CONFIG_GET, MSG_CONFIG_SET, MSG_IO_WEB_ACCESS_LINK,
    MSG_IO_WEB_ACCESS_LINK_RESPONSE, MSG_IO_WEB_EXPOSURES, MSG_IO_WEB_EXPOSURES_RESPONSE,
    MSG_NODE_STATUS_GET, MSG_NODE_STATUS_GET_RESPONSE, PRIMARY_HIVE_ID, SYSTEM_KIND,
};
use fluxbee_sdk::{
    is_root_tenant, managed_node_config_path, managed_node_config_path_with_root,
    read_self_ilk_from_env, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, RpcCommandReceiver, RpcError,
    DEFAULT_ROOT_TENANT_ID, ENV_SELF_TENANT_ID, FLUXBEE_NODE_NAME_ENV,
};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
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
use io_common::io_web_adapter_config::IoWebAdapterConfigContract;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{watch, RwLock};
use tracing_subscriber::EnvFilter;

use exposures::{
    boot_second, run_access_clock, unix_now, Exposures, VaultKeys, EXPOSURES_FILE, KEY_RETRY,
};
use proxy::Proxy;
use upstream::{archi_from_hive_yaml, Upstreams};
use web::{WebSetup, WebState, WebStatus};

type DynError = Box<dyn std::error::Error + Send + Sync>;

const RPC_CH_CONTROL: &str = "control";
/// The exposures' two commands, served by their own task (`run_exposure_commands`), so a key read
/// never holds NODE_STATUS_GET or CONFIG (docs/io-web-v2-stage-2-1.md §5.5).
const RPC_CH_EXPOSURES: &str = "exposures";
/// Whatever else reaches io.web, on a channel and a task of its own (`run_unserved`): a flood of it
/// fills only its own bounded lanes, never those of the exposures' channel, where it would get
/// SY.admin's commands refused `NOT_TAKEN` (`node_busy`).
const RPC_CH_UNSERVED: &str = "unserved";

/// The local name of the one node io.web runs as, `IO.web@motherbee`.
const IO_WEB_LOCAL_NAME: &str = "IO.web";

/// The one sender of io.web's exposure commands, as the router stamps it: SY.admin on the
/// motherbee (§5.5), which acts on them for io.cloud alone (§5.1). A constant, never the config's
/// `admin_target`, which a node's `config.json` may name otherwise.
const EXPOSURES_AUTHORITY: &str = "SY.admin@motherbee";

/// The error code of an exposure command whose payload io.web cannot read: the set or the link
/// request is not taken.
const INVALID_REQUEST: &str = "INVALID_REQUEST";

/// Boot infra from the managed spawn (the env the runtime injects, or the `node`/`_system` of
/// `config.json`), as io.blob reads it. None of it is operator config.
#[derive(Debug, Clone)]
struct Config {
    node_name: String,
    /// The tenant the orchestrator spawned it in (`spawn_tenant`); `None` when the spawn names
    /// none.
    tenant_id: Option<String>,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    admin_target: String,
    orchestrator_target: String,
}

impl Config {
    /// Env first, then the spawn `config.json`, then the default. Reading the spawn doc is
    /// tolerant, as in io.blob: the control plane classifies a config.json on its own.
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
            tenant_id: spawn_tenant(env(ENV_SELF_TENANT_ID), &spawn_doc),
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
            // The two CONFIG authorities, as in io.cloud: from the spawn doc or the hive's
            // defaults, never from an env.
            admin_target: json_get_string(&spawn_doc, "node.admin_target")
                .unwrap_or_else(|| format!("SY.admin@{hive_id}")),
            orchestrator_target: json_get_string(&spawn_doc, "node.orchestrator_target")
                .unwrap_or_else(|| format!("SY.orchestrator@{hive_id}")),
            node_name,
        })
    }
}

/// The tenant the orchestrator spawned io.web in: `FLUXBEE_NODE_TENANT_ID`, which it injects with
/// the node's identity (`env_tenant`), else the spawn `config.json`, `_system.tenant_id` then
/// `tenant_id`, the order the orchestrator itself reads a node's tenant in. An env that is set wins
/// whatever it holds. No default: a spawn that names no tenant is not the root tenant's.
fn spawn_tenant(env_tenant: Option<String>, spawn_doc: &Value) -> Option<String> {
    env_tenant
        .or_else(|| json_get_string(spawn_doc, "_system.tenant_id"))
        .or_else(|| json_get_string(spawn_doc, "tenant_id"))
}

/// The node's runtime state, in the style of io.blob's. The port runs in its own task
/// (`web::run`); the control loop reads its state and tells it when the config is in use.
struct RuntimeState {
    config: Config,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    dispatcher: Arc<RouterDispatcher>,
    control_plane: RwLock<IoControlPlaneState>,
    control_metrics: IoControlPlaneMetrics,
    /// Whether the config is in use (CONFIGURED); the port opens once it is.
    configured: watch::Sender<bool>,
    web_setup: WebSetup,
    web_status: Arc<WebStatus>,
    exposures: Arc<Exposures>,
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,io_web=debug,io_common=info,fluxbee_sdk=info")
        }))
        .init();

    let config = Config::load()?;

    // Fail-closed safety net, before anything else: run as anything but IO.web@motherbee in the
    // root tenant, io.web connects to no router and opens no port.
    if let Err(err) = ensure_canonical_instance(&config.node_name, config.tenant_id.as_deref()) {
        tracing::error!(
            node = %config.node_name,
            tenant = config.tenant_id.as_deref().unwrap_or_default(),
            error = %err,
            "IO.web runs only as IO.web@motherbee in the root tenant; refusing to run"
        );
        return Err(err.into());
    }

    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoWebAdapterConfigContract);
    let boot_state = boot_control_plane(
        &config.node_name,
        adapter_contract.as_ref(),
        Path::new(DEFAULT_MANAGED_NODES_ROOT),
    );

    let started = unix_now();
    let node_config = NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    };
    let dispatcher = RouterDispatcher::connect_with_retry(
        node_config,
        Duration::from_secs(1),
        io_web_profile()?,
    )
    .await?;
    let sender = dispatcher.sender_snapshot();

    // The exposures: their keys read from the vault as io.web, with the ILK the orchestrator
    // injected at the spawn (§3.1); their copy beside io.web's config.json (§2.3).
    let ilk = read_self_ilk_from_env();
    let exposures = Arc::new(Exposures::new(
        exposures_file(&config.node_name, Path::new(DEFAULT_MANAGED_NODES_ROOT)),
        Arc::new(VaultKeys::new(
            dispatcher.clone(),
            &config.node_name,
            ilk.clone(),
        )),
        boot_second(started),
    ));
    let (loaded, loaded_rx) = watch::channel(false);
    tokio::spawn(run_exposure_commands(
        dispatcher.take_command_receiver(RPC_CH_EXPOSURES).await?,
        sender.clone(),
        Arc::clone(&exposures),
        loaded,
    ));
    tokio::spawn(run_unserved(
        dispatcher.take_command_receiver(RPC_CH_UNSERVED).await?,
        sender.clone(),
    ));
    tokio::spawn(run_key_retries(Arc::clone(&exposures)));
    tokio::spawn(run_access_clock(Arc::clone(&exposures)));

    let web_setup = WebSetup::motherbee();
    // Where each exposure's upstream is (docs/io-web-v2-stage-2-2.md §4.2): Archi where hive.yaml
    // says it listens, read once now; the workers from the registry, at each request.
    let upstreams = Upstreams::new(
        archi_from_hive_yaml(&config.config_dir),
        web_setup.registry.clone(),
    );
    match upstreams.archi() {
        Ok(address) => tracing::info!(
            archi = %address,
            "IO.web dials Archi's exposure where hive.yaml's architect.listen says Archi listens"
        ),
        Err(reason) => tracing::error!(
            reason = %reason,
            "IO.web has no address for Archi: Archi's exposure answers 502 until io.web restarts \
             with a hive.yaml it can use"
        ),
    }
    let web_status = Arc::new(WebStatus::new());
    let (configured, configured_rx) =
        watch::channel(boot_state.current_state == IoNodeLifecycleState::Configured);
    tracing::info!(
        node = %sender.full_name(),
        lifecycle = %boot_state.current_state.as_str(),
        config_version = boot_state.config_version,
        last_error = ?boot_state.last_error,
        listen = %web_setup.listen,
        tls_dir = %web_setup.tls_dir.display(),
        hive_registry = %web_setup.registry.root().display(),
        exposures_file = %exposures.file().display(),
        ilk = ilk.as_deref().unwrap_or("none: no key can be read"),
        "IO.web ready (managed control plane); its port opens once its config is in use and its \
         exposures are loaded"
    );
    tokio::spawn(web::run(
        web_setup.clone(),
        Arc::clone(&web_status),
        configured_rx,
        loaded_rx,
        Arc::new(Proxy {
            exposures: Arc::clone(&exposures),
            upstreams,
        }),
    ));

    let state = RuntimeState {
        config,
        adapter_contract,
        dispatcher: dispatcher.clone(),
        control_metrics: IoControlPlaneMetrics::with_initial_state(
            boot_state.current_state.as_str(),
            boot_state.config_version,
        ),
        control_plane: RwLock::new(boot_state),
        configured,
        web_setup,
        web_status,
        exposures,
    };

    let mut incoming = dispatcher.take_command_receiver(RPC_CH_CONTROL).await?;
    while let Some(message) = incoming.recv().await {
        let response = if is_node_status_request(&message) {
            let payload = node_status_payload(
                &*state.control_plane.read().await,
                &state.web_status.get(),
                &state.web_setup,
                state.exposures.view(unix_now()),
            );
            build_reply(&sender, &message, MSG_NODE_STATUS_GET_RESPONSE, payload)
        } else if is_config_command(&message) {
            handle_control_message(&state, &message).await
        } else {
            tracing::debug!(
                trace_id = %message.routing.trace_id,
                caller = ?message.routing.src_l2_name,
                msg = ?message.meta.msg,
                "IO.web ignored a message it does not serve"
            );
            continue;
        };
        sender.send(response).await?;
    }

    Ok(())
}

/// io.web runs only as `IO.web@motherbee`, in the root tenant, fail-closed in the binary as io.blob
/// and io.cloud keep to the motherbee: a managed runtime has no systemd ExecCondition to keep it off
/// a spoke, and its port must never open for a second instance, a tenant's (Cloud's
/// `provision_node`) or a mistyped name's, which could take 9200 before the operator's. The name is
/// the spawn name the router registers the node under, so it pins the hive too; the tenant is the
/// one the orchestrator spawned it in (`spawn_tenant`), the root tenant in any spelling of its
/// UUID. The orchestrator refuses both at launch as well (`core_io_refusal`).
fn ensure_canonical_instance(node_name: &str, tenant_id: Option<&str>) -> Result<(), String> {
    let canonical = format!("{IO_WEB_LOCAL_NAME}@{PRIMARY_HIVE_ID}");
    if node_name != canonical {
        return Err(format!(
            "IO.web runs only as {canonical}, not '{node_name}'"
        ));
    }
    match tenant_id {
        Some(tenant_id) if is_root_tenant(tenant_id) => Ok(()),
        Some(tenant_id) => Err(format!(
            "IO.web runs only in the root tenant {DEFAULT_ROOT_TENANT_ID}, not '{tenant_id}'"
        )),
        None => Err(format!(
            "IO.web runs only in the root tenant {DEFAULT_ROOT_TENANT_ID}, and its spawn names no tenant ({ENV_SELF_TENANT_ID}, or tenant_id in its config.json)"
        )),
    }
}

/// What reaches io.web, and where:
/// - the exposures' commands, by their exact names, on their own channel, before any pending
///   answer, as the edge takes its door's commands (docs/io-web-v2-stage-2-1.md §5.5);
/// - NODE_STATUS_GET and CONFIG_GET/SET on the control channel;
/// - anything else on a channel of its own (`run_unserved`), which answers `UNAUTHORIZED` to a
///   message named after a command in another kind or spelling, and drops the rest.
fn io_web_profile() -> Result<OperationalRouteProfile, RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_CONTROL)
        .command_channel(RPC_CH_EXPOSURES)
        .command_channel(RPC_CH_UNSERVED)
        .pre_pending_rule(
            RouteMatch::one_of(SYSTEM_KIND, [MSG_IO_WEB_EXPOSURES, MSG_IO_WEB_ACCESS_LINK]),
            RouteTarget::Command(RPC_CH_EXPOSURES),
        )
        .post_pending_rule(
            RouteMatch::exact(SYSTEM_KIND, MSG_NODE_STATUS_GET),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        // Managed control plane: CONFIG_GET/SET come in through the same channel.
        .post_pending_rule(
            RouteMatch::one_of(SYSTEM_KIND, [MSG_CONFIG_GET, MSG_CONFIG_SET]),
            RouteTarget::Command(RPC_CH_CONTROL),
        )
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command(RPC_CH_UNSERVED))
        .build()
}

/// io.web's copy of the exposures: `exposures.json` beside its `config.json` (§2.3), so a purge of
/// the node takes it too.
fn exposures_file(node_name: &str, nodes_root: &Path) -> PathBuf {
    managed_node_config_path_with_root(node_name, nodes_root)
        .ok()
        .and_then(|config| config.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| nodes_root.to_path_buf())
        .join(EXPOSURES_FILE)
}

/// io.web's exposure commands (§5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExposureCommand {
    Exposures,
    AccessLink,
}

impl ExposureCommand {
    /// The command a message is named after, in any case and spacing: a variant of the name is
    /// refused (`authorize_exposure_command`), never ignored.
    fn of(message: &Message) -> Option<Self> {
        let name = message.meta.msg.as_deref()?.trim();
        if name.eq_ignore_ascii_case(MSG_IO_WEB_EXPOSURES) {
            Some(Self::Exposures)
        } else if name.eq_ignore_ascii_case(MSG_IO_WEB_ACCESS_LINK) {
            Some(Self::AccessLink)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Exposures => MSG_IO_WEB_EXPOSURES,
            Self::AccessLink => MSG_IO_WEB_ACCESS_LINK,
        }
    }

    fn response(self) -> &'static str {
        match self {
            Self::Exposures => MSG_IO_WEB_EXPOSURES_RESPONSE,
            Self::AccessLink => MSG_IO_WEB_ACCESS_LINK_RESPONSE,
        }
    }
}

/// The exposures' channel: first the set of io.web's file and its keys, then `loaded` turns true
/// and the port may open; then each command in its own task, so a link never waits behind a push
/// reading keys (pushes take their turn in `Exposures`).
async fn run_exposure_commands(
    mut commands: RpcCommandReceiver,
    sender: NodeSender,
    exposures: Arc<Exposures>,
    loaded: watch::Sender<bool>,
) {
    exposures.load_at_boot(unix_now()).await;
    loaded.send_replace(true);
    while let Some(message) = commands.recv().await {
        // Only the commands' exact names are routed here (`io_web_profile`).
        let Some(command) = ExposureCommand::of(&message) else {
            continue;
        };
        let sender = sender.clone();
        let exposures = Arc::clone(&exposures);
        tokio::spawn(async move {
            let payload = answer_exposure_command(&exposures, command, &message, unix_now()).await;
            send_answer(&sender, &message, command, payload).await;
        });
    }
}

/// Whatever else reaches io.web (`RPC_CH_UNSERVED`): a message named after an exposure command in
/// another kind or spelling is answered `UNAUTHORIZED` and logged, never taken; the rest is
/// dropped without a line each, which a flood of it would turn into a flood of the journal.
async fn run_unserved(mut unserved: RpcCommandReceiver, sender: NodeSender) {
    while let Some(message) = unserved.recv().await {
        let Some(command) = ExposureCommand::of(&message) else {
            continue;
        };
        // The exact names go to the exposures' channel, so this never passes; and if it did, it
        // would still be refused here.
        let why = authorize_exposure_command(&message, command)
            .err()
            .unwrap_or_else(|| "not on the exposures' channel".to_string());
        let payload = refuse_exposure_command(&message, command, &why);
        send_answer(&sender, &message, command, payload).await;
    }
}

/// Sends the answer to an exposure command, logged when it cannot.
async fn send_answer(
    sender: &NodeSender,
    message: &Message,
    command: ExposureCommand,
    payload: Value,
) {
    let reply = build_reply(sender, message, command.response(), payload);
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(
            trace_id = %message.routing.trace_id,
            command = command.name(),
            error = %err,
            "IO.web could not answer an exposure command"
        );
    }
}

/// Every 30 s, the keys still unavailable are read again (§5.5).
async fn run_key_retries(exposures: Arc<Exposures>) {
    let mut every = tokio::time::interval(KEY_RETRY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick is at once, and the boot reads the keys itself.
    every.tick().await;
    loop {
        every.tick().await;
        exposures.retry(unix_now()).await;
    }
}

/// An exposure command is taken only as a system message from the router-stamped
/// `SY.admin@motherbee`, by its exact name, and never as a tap copy, which keeps its sender's name
/// but was sent to another node (§5.5). Why not, otherwise.
fn authorize_exposure_command(message: &Message, command: ExposureCommand) -> Result<(), String> {
    if message.meta.via_tap {
        return Err("a tap copy".to_string());
    }
    if !is_system_kind(&message.meta.msg_type) {
        return Err(format!(
            "not a system message (type '{}')",
            message.meta.msg_type
        ));
    }
    if message.meta.msg.as_deref() != Some(command.name()) {
        return Err(format!("not named exactly {}", command.name()));
    }
    match message.routing.src_l2_name.as_deref().map(str::trim) {
        Some(EXPOSURES_AUTHORITY) => Ok(()),
        Some("") | None => Err("no router-stamped sender".to_string()),
        Some(sender) => Err(format!("from {sender}")),
    }
}

/// The answer's payload to an exposure command: `UNAUTHORIZED`, logged, when it is not
/// SY.admin's; the set applied or the link minted otherwise.
async fn answer_exposure_command(
    exposures: &Exposures,
    command: ExposureCommand,
    message: &Message,
    now: u64,
) -> Value {
    if let Err(why) = authorize_exposure_command(message, command) {
        return refuse_exposure_command(message, command, &why);
    }
    match command {
        ExposureCommand::Exposures => {
            match serde_json::from_value::<IoWebExposuresPayload>(message.payload.clone()) {
                Ok(set) => wire(&exposures.apply_push(set, now).await),
                Err(err) => wire(&IoWebExposuresResponse::Error {
                    error_code: INVALID_REQUEST.to_string(),
                    error_detail: format!("io.web cannot read the set: {err}"),
                }),
            }
        }
        ExposureCommand::AccessLink => {
            match serde_json::from_value::<IoWebAccessLinkRequest>(message.payload.clone()) {
                Ok(request) => wire(&exposures.issue_link(&request, now)),
                Err(err) => wire(&IoWebAccessLinkResponse::Error {
                    error_code: INVALID_REQUEST.to_string(),
                    error_detail: format!("io.web cannot read the request: {err}"),
                }),
            }
        }
    }
}

/// The `UNAUTHORIZED` answer's payload to an exposure command that is not SY.admin's, logged
/// with why.
fn refuse_exposure_command(message: &Message, command: ExposureCommand, why: &str) -> Value {
    tracing::warn!(
        trace_id = %message.routing.trace_id,
        caller = ?message.routing.src_l2_name,
        msg = ?message.meta.msg,
        msg_type = %message.meta.msg_type,
        via_tap = message.meta.via_tap,
        why = %why,
        "IO.web refused an exposure command: only SY.admin@motherbee sends them"
    );
    refusal(
        IO_WEB_ERROR_UNAUTHORIZED,
        format!(
            "{} is taken only from {EXPOSURES_AUTHORITY}, as a system message: {why}",
            command.name()
        ),
    )
}

/// `{"status": "error", error_code, error_detail}`, the error shape of both answers.
fn refusal(error_code: &str, error_detail: String) -> Value {
    json!({"status": "error", "error_code": error_code, "error_detail": error_detail})
}

/// An answer as it travels. Both answers' shapes always serialize.
fn wire(answer: &impl Serialize) -> Value {
    serde_json::to_value(answer).unwrap_or_else(|err| refusal(INVALID_REQUEST, err.to_string()))
}

// --- Control plane (a mirror of io.blob's) -------------------------------------------------------

/// The control plane at boot, from the node's `config.json` through the contract. io.web has no
/// field to configure in this stage, so the orchestrator's seed alone is its whole config: what
/// the SDK reads as UNCONFIGURED (a config.json without operator content, or none) boots
/// CONFIGURED on the contract's defaults, and io.web serves. Operator content the contract refuses
/// is FAILED_CONFIG, with the contract's error.
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
    if state.current_state == IoNodeLifecycleState::Unconfigured {
        // No operator content: the defaults are the contract applied to nothing. The config is the
        // seed config.json the orchestrator wrote, the one source there is.
        state.effective_config = Some(json!({}));
        state.config_source = IoConfigSource::OrchestratorFallback;
        state.schema_version = state.schema_version.max(1);
    }
    validate_boot_effective_config(&mut state, contract);
    state
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
            state.effective_config = None;
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: err.code().to_string(),
                message: err.to_string(),
            });
        }
    }
}

fn is_node_status_request(message: &Message) -> bool {
    is_system_kind(&message.meta.msg_type)
        && message.meta.msg.as_deref() == Some(MSG_NODE_STATUS_GET)
}

/// NODE_STATUS_GET: the SDK's managed answer for the config (HEALTHY only when CONFIGURED), then
/// the port's. io.web is HEALTHY only while its config is in use AND its port serves. Otherwise it
/// is DEGRADED, never ERROR, as across the managed family, with the reason in `last_error`: the
/// config's while it is not in use, else the port's (`web::MESH_TLS_UNAVAILABLE` and the other
/// codes). `extensions.web` is the port's state, with its `exposures` (`Exposures::view`,
/// docs/io-web-v2-stage-2-1.md §5.5). The orchestrator's `get_node_status` reports `health_state`,
/// `last_error` and `extensions`.
fn node_status_payload(
    control: &IoControlPlaneState,
    web: &WebState,
    setup: &WebSetup,
    exposures: Value,
) -> Value {
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
    let mut payload = build_node_status_response_payload(lifecycle, last_error.as_ref());
    if lifecycle == ManagedNodeLifecycleState::Configured {
        match web {
            WebState::Serving => {}
            WebState::Starting => payload["health_state"] = json!("DEGRADED"),
            WebState::Unavailable { code, message } => {
                payload["health_state"] = json!("DEGRADED");
                payload["last_error"] = json!({ "code": code, "message": message });
            }
        }
    }
    let mut view = setup.view(web);
    view["exposures"] = exposures;
    payload["extensions"] = json!({ "web": view });
    payload
}

fn is_config_command(message: &Message) -> bool {
    message.meta.msg_type.eq_ignore_ascii_case(SYSTEM_KIND)
        && matches!(
            message.meta.msg.as_deref(),
            Some(command)
                if command.eq_ignore_ascii_case(MSG_CONFIG_GET)
                    || command.eq_ignore_ascii_case(MSG_CONFIG_SET)
        )
}

/// CONFIG_GET/SET come from the admin or the orchestrator, by the router-stamped caller.
fn control_caller_authorized(config: &Config, message: &Message) -> bool {
    let caller = message.routing.src_l2_name.as_deref().map(str::trim);
    caller == Some(config.admin_target.as_str())
        || caller == Some(config.orchestrator_target.as_str())
}

async fn handle_control_message(state: &RuntimeState, message: &Message) -> Message {
    let payload = if !control_caller_authorized(&state.config, message) {
        tracing::warn!(
            trace_id = %message.routing.trace_id,
            source = ?message.routing.src_l2_name,
            command = ?message.meta.msg,
            "IO.web rejected configuration command from non-authority"
        );
        let snapshot = state.control_plane.read().await.clone();
        build_io_config_set_error_payload(
            &state.config.node_name,
            &snapshot,
            "unauthorized",
            "CONFIG_GET/SET requires the Admin or Orchestrator origin",
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
    inject_runtime_status(&mut payload, state);
    payload
}

async fn apply_config_set(state: &RuntimeState, payload: &NodeConfigSetPayload) -> Value {
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
        effective_config: Some(effective),
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
    state
        .control_metrics
        .record_config_set_ok(next.current_state.as_str(), next.config_version);
    // The config is in use: a port that waited on a FAILED_CONFIG boot opens now.
    state.configured.send_replace(true);
    log_config_set_applied(
        &state.config.node_name,
        payload.schema_version,
        payload.config_version,
        &[],
        &[],
        &[],
    );

    let mut response = build_io_config_set_ok_payload(&state.config.node_name, &next);
    if let Some(object) = response.as_object_mut() {
        object.insert(
            "apply".to_string(),
            json!({
                "mode":"hot_reload",
                "hot_applied":[],
                "reinit_performed":[],
                "restart_required":[],
            }),
        );
    }
    inject_runtime_status(&mut response, state);
    response
}

fn inject_runtime_status(payload: &mut Value, state: &RuntimeState) {
    let mut web = state.web_setup.view(&state.web_status.get());
    web["exposures"] = state.exposures.view(unix_now());
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "runtime".to_string(),
            json!({
                "transport":"router_socket",
                "role":"web",
                "web":web,
                "tls_dir":state.web_setup.tls_dir.display().to_string(),
                "hive_registry":state.web_setup.registry.root().display().to_string(),
                "control_plane_metrics":state.control_metrics.snapshot(),
            }),
        );
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

/// Reads (tolerantly) the spawn `config.json` in the node's dir for the boot infra. A missing or
/// unreadable file does NOT abort the boot: it falls back to env and defaults.
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
                "io.web spawn config.json is not valid JSON; using env/defaults for infra"
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
    let mut current = root;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::managed_node_config_path_with_root;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const NODE: &str = "IO.web@motherbee";
    const ROOT_TENANT: &str = "tnt:00000000-0000-0000-0000-000000000001";

    /// A fresh managed-nodes root of its own under the system's temp dir.
    fn nodes_root(label: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "io-web-nodes-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create nodes root");
        dir
    }

    fn write_config(root: &Path, raw: &str) {
        let path = managed_node_config_path_with_root(NODE, root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, raw).unwrap();
    }

    fn boot(root: &Path) -> IoControlPlaneState {
        boot_control_plane(NODE, &IoWebAdapterConfigContract, root)
    }

    /// Only IO.web@motherbee in the root tenant runs: no other hive, no other name (a second io.web,
    /// a mistyped one), no other tenant, and no spawn that names no tenant.
    #[test]
    fn the_self_check_admits_only_io_web_at_the_motherbee_in_the_root_tenant() {
        assert_eq!(ensure_canonical_instance(NODE, Some(ROOT_TENANT)), Ok(()));
        assert_eq!(
            ensure_canonical_instance(NODE, Some("tnt:00000000000000000000000000000001")),
            Ok(()),
            "the root tenant in another spelling of its UUID"
        );
        for name in [
            "IO.web@worker1",
            "IO.web@ingress1",
            "IO.web@Motherbee",
            "IO.web@motherbee2",
            "IO.web2@motherbee",
            "IO.web.second@motherbee",
            "IO.Web@motherbee",
            "io.web@motherbee",
            "AI.web@motherbee",
            "IO.web",
            "",
        ] {
            let err = ensure_canonical_instance(name, Some(ROOT_TENANT)).expect_err(name);
            assert!(
                err.contains("runs only as IO.web@motherbee"),
                "{name}: {err}"
            );
        }
        for tenant in [
            Some("tnt:8a0c3f8e-2f1b-4d3a-9c55-0f6e2b7d9a11"),
            Some("tnt:not-a-uuid"),
            Some("00000000-0000-0000-0000-000000000001"),
            None,
        ] {
            let err = ensure_canonical_instance(NODE, tenant).expect_err("not the root tenant");
            assert!(err.contains(ROOT_TENANT), "{tenant:?}: {err}");
        }
    }

    /// The tenant as the orchestrator spawned it: its env first, whatever it holds, then the
    /// spawn config.json, `_system.tenant_id` before `tenant_id`; none when neither names one.
    #[test]
    fn the_tenant_is_the_spawns_env_then_its_config() {
        let other = "tnt:8a0c3f8e-2f1b-4d3a-9c55-0f6e2b7d9a11";
        let seed = json!({"tenant_id": ROOT_TENANT, "_system": {"tenant_id": ROOT_TENANT}});
        assert_eq!(
            spawn_tenant(Some(other.to_string()), &seed).as_deref(),
            Some(other)
        );
        assert_eq!(spawn_tenant(None, &seed).as_deref(), Some(ROOT_TENANT));
        // What a CONFIG_SET persists keeps `_system`, not the seed's top-level tenant_id.
        let set = json!({"io": {}, "_system": {"tenant_id": ROOT_TENANT, "config_version": 4}});
        assert_eq!(spawn_tenant(None, &set).as_deref(), Some(ROOT_TENANT));
        let top_only = json!({"tenant_id": other, "_system": {"config_version": 1}});
        assert_eq!(spawn_tenant(None, &top_only).as_deref(), Some(other));
        let both = json!({"tenant_id": ROOT_TENANT, "_system": {"tenant_id": other}});
        assert_eq!(spawn_tenant(None, &both).as_deref(), Some(other));
        for nothing in [
            json!({}),
            json!({"_system": {"tenant_id": " "}}),
            Value::Null,
        ] {
            assert_eq!(spawn_tenant(None, &nothing), None, "{nothing}");
        }
    }

    /// The orchestrator's seed alone, and no config.json at all, boot CONFIGURED on the
    /// defaults: io.web is never left UNCONFIGURED, which would keep it DEGRADED for good.
    #[test]
    fn the_seed_alone_boots_configured_on_the_defaults() {
        let root = nodes_root("seed");
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(state.effective_config, Some(json!({"io": {}})));
        assert_eq!(state.config_version, 0);

        let seed = json!({
            "tenant_id": ROOT_TENANT,
            "_system": {"config_version": 3, "ilk_id": "ilk:11111111-2222-3333-4444-555555555555"},
        });
        write_config(&root, &seed.to_string());
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(state.config_source, IoConfigSource::OrchestratorFallback);
        assert_eq!(state.effective_config, Some(json!({"io": {}})));
        assert_eq!(
            state.config_version, 3,
            "kept, so CONFIG_SET stays monotonic"
        );
        assert_eq!(state.schema_version, 1);
        assert_eq!(state.last_error, None);

        // What a CONFIG_SET persists boots the same.
        write_config(&root, r#"{"io": {}, "_system": {"config_version": 4}}"#);
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::Configured);
        assert_eq!(state.config_version, 4);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Operator content the contract refuses is FAILED_CONFIG with the contract's reason, and so
    /// is a config.json that is not JSON.
    #[test]
    fn a_config_the_contract_refuses_boots_failed_config() {
        let root = nodes_root("refused");
        let refused = json!({
            "io": {"port": 9300},
            "tenant_id": ROOT_TENANT,
            "_system": {"config_version": 2},
        });
        write_config(&root, &refused.to_string());
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::FailedConfig);
        assert_eq!(state.effective_config, None);
        assert_eq!(state.config_version, 2);
        let error = state.last_error.expect("why");
        assert_eq!(error.code, "invalid_config");
        assert!(error.message.contains("io.port"), "{}", error.message);

        write_config(&root, "not json");
        let state = boot(&root);
        assert_eq!(state.current_state, IoNodeLifecycleState::FailedConfig);
        assert_eq!(state.effective_config, None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// HEALTHY only while the config is in use and the port serves; otherwise DEGRADED with the
    /// reason: the config's while it is not in use, else the port's.
    #[test]
    fn the_status_is_healthy_only_while_configured_and_serving() {
        let setup = WebSetup::motherbee();
        let configured = IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            config_source: IoConfigSource::OrchestratorFallback,
            schema_version: 1,
            config_version: 3,
            effective_config: Some(json!({"io": {}})),
            last_error: None,
        };

        let exposures = json!({"generation": 7, "exposures": 1, "enabled": 1});
        let serving =
            node_status_payload(&configured, &WebState::Serving, &setup, exposures.clone());
        assert_eq!(serving["status"], "ok");
        assert_eq!(serving["health_state"], "HEALTHY");
        assert_eq!(serving["lifecycle_state"], "CONFIGURED");
        assert!(serving.get("last_error").is_none());
        assert_eq!(
            serving["extensions"]["web"],
            json!({
                "state": "serving",
                "listen": "0.0.0.0:9200",
                "admits": ["ingress"],
                "exposures": exposures,
            })
        );

        let starting = node_status_payload(&configured, &WebState::Starting, &setup, json!({}));
        assert_eq!(starting["health_state"], "DEGRADED");
        assert!(starting.get("last_error").is_none());
        assert_eq!(starting["extensions"]["web"]["state"], "starting");

        let unavailable = WebState::Unavailable {
            code: web::MESH_TLS_UNAVAILABLE,
            message: "/var/lib/fluxbee/tls/motherbee: io: not found".to_string(),
        };
        let no_material = node_status_payload(&configured, &unavailable, &setup, json!({}));
        assert_eq!(no_material["health_state"], "DEGRADED");
        assert_eq!(no_material["lifecycle_state"], "CONFIGURED");
        assert_eq!(no_material["last_error"]["code"], "mesh_tls_unavailable");
        assert_eq!(no_material["extensions"]["web"]["state"], "unavailable");
        assert_eq!(
            no_material["extensions"]["web"]["code"],
            "mesh_tls_unavailable"
        );

        let failed = IoControlPlaneState {
            current_state: IoNodeLifecycleState::FailedConfig,
            effective_config: None,
            last_error: Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: "io.web has no configurable field in this stage; refused: io.port"
                    .to_string(),
            }),
            ..configured
        };
        let refused = node_status_payload(&failed, &WebState::Starting, &setup, json!({}));
        assert_eq!(refused["health_state"], "DEGRADED");
        assert_eq!(refused["lifecycle_state"], "FAILED_CONFIG");
        assert_eq!(refused["last_error"]["code"], "invalid_config");
        assert_eq!(refused["extensions"]["web"]["state"], "starting");
    }

    #[test]
    fn only_the_admin_and_the_orchestrator_may_configure() {
        let config = Config {
            node_name: NODE.to_string(),
            tenant_id: Some(ROOT_TENANT.to_string()),
            node_version: "0.1.85".to_string(),
            router_socket: PathBuf::from("/var/run/fluxbee/routers"),
            uuid_persistence_dir: PathBuf::from("/var/lib/fluxbee/state/nodes"),
            config_dir: PathBuf::from("/etc/fluxbee"),
            admin_target: "SY.admin@motherbee".to_string(),
            orchestrator_target: "SY.orchestrator@motherbee".to_string(),
        };
        let from = |caller: Option<&str>| Message {
            routing: Routing {
                src: "00000000-0000-4000-8000-000000000001".to_string(),
                src_l2_name: caller.map(str::to_string),
                dst: Destination::Unicast(NODE.to_string()),
                ttl: 16,
                trace_id: "00000000-0000-4000-8000-000000000002".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_CONFIG_GET.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        };
        assert!(control_caller_authorized(
            &config,
            &from(Some("SY.admin@motherbee"))
        ));
        assert!(control_caller_authorized(
            &config,
            &from(Some("SY.orchestrator@motherbee"))
        ));
        for caller in [
            Some("SY.admin@worker1"),
            Some("IO.cloud@motherbee"),
            Some("SY.edge@ingress1"),
            None,
        ] {
            assert!(
                !control_caller_authorized(&config, &from(caller)),
                "{caller:?}"
            );
        }
    }

    /// An exposure command as it arrives: `msg` of kind `msg_type`, from the router-stamped
    /// `caller`, a tap copy when `via_tap`.
    fn exposure_command(
        caller: Option<&str>,
        msg_type: &str,
        msg: &str,
        via_tap: bool,
        payload: Value,
    ) -> Message {
        Message {
            routing: Routing {
                src: "00000000-0000-4000-8000-0000000000ad".to_string(),
                src_l2_name: caller.map(str::to_string),
                dst: Destination::Unicast(NODE.to_string()),
                ttl: 16,
                trace_id: uuid::Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: Some(msg.to_string()),
                via_tap,
                ..Meta::default()
            },
            payload,
        }
    }

    /// Through io.web's own routes and channels: the exposures' commands are taken only as system
    /// messages, by their exact names, from the router-stamped SY.admin@motherbee, never as a tap
    /// copy. Anything else named after them is answered UNAUTHORIZED, on its own trace, and
    /// changes nothing.
    #[tokio::test]
    async fn pushes_are_taken_only_from_sy_admin_at_the_motherbee_and_never_via_tap() {
        use crate::exposures::testing::{
            enablement, exposure, file, remove, MapKeys, SHOP, SHOP_ENB, SHOP_ID,
        };
        use fluxbee_sdk::protocol::WebExposureKeySlot;
        use fluxbee_sdk::RouterDispatcherTestHarness;

        assert_eq!(EXPOSURES_AUTHORITY, format!("SY.admin@{PRIMARY_HIVE_ID}"));
        let (dispatcher, mut router) =
            RouterDispatcherTestHarness::new(NODE, io_web_profile().expect("io.web's profile"));
        let keys = MapKeys::new();
        keys.put(SHOP_ID, WebExposureKeySlot::A, 4, 1);
        let now = unix_now();
        let exposures = Arc::new(Exposures::new(file("authority"), keys, now));
        let (loaded, mut loaded_rx) = watch::channel(false);
        tokio::spawn(run_exposure_commands(
            dispatcher
                .take_command_receiver(RPC_CH_EXPOSURES)
                .await
                .expect("the exposures' channel"),
            dispatcher.sender_snapshot(),
            Arc::clone(&exposures),
            loaded,
        ));
        tokio::spawn(run_unserved(
            dispatcher
                .take_command_receiver(RPC_CH_UNSERVED)
                .await
                .expect("the unserved channel"),
            dispatcher.sender_snapshot(),
        ));
        loaded_rx.wait_for(|loaded| *loaded).await.unwrap();

        let shop = exposure(
            SHOP_ID,
            SHOP,
            vec![enablement(SHOP_ENB, now + 3600, WebExposureKeySlot::A, 4)],
        );
        let set = json!({"generation": 5, "exposures": [shop]});
        let ask = json!({"exposure_id": SHOP_ID, "enablement_id": SHOP_ENB, "generation": 5});
        let admin = Some(EXPOSURES_AUTHORITY);
        let push = MSG_IO_WEB_EXPOSURES;
        let link = MSG_IO_WEB_ACCESS_LINK;
        for (case, message) in [
            (
                "SY.admin@worker1",
                exposure_command(
                    Some("SY.admin@worker1"),
                    SYSTEM_KIND,
                    push,
                    false,
                    set.clone(),
                ),
            ),
            (
                "SY.architect@motherbee",
                exposure_command(
                    Some("SY.architect@motherbee"),
                    SYSTEM_KIND,
                    push,
                    false,
                    set.clone(),
                ),
            ),
            (
                "IO.cloud@motherbee",
                exposure_command(
                    Some("IO.cloud@motherbee"),
                    SYSTEM_KIND,
                    push,
                    false,
                    set.clone(),
                ),
            ),
            (
                "no router-stamped sender",
                exposure_command(None, SYSTEM_KIND, push, false, set.clone()),
            ),
            (
                "an empty sender",
                exposure_command(Some(""), SYSTEM_KIND, push, false, set.clone()),
            ),
            (
                "a tap copy",
                exposure_command(admin, SYSTEM_KIND, push, true, set.clone()),
            ),
            (
                "not a system message",
                exposure_command(admin, "user", push, false, set.clone()),
            ),
            (
                "another spelling",
                exposure_command(admin, SYSTEM_KIND, "io_web_exposures", false, set.clone()),
            ),
            (
                "a link for SY.architect",
                exposure_command(
                    Some("SY.architect@motherbee"),
                    SYSTEM_KIND,
                    link,
                    false,
                    ask.clone(),
                ),
            ),
            (
                "a link as a tap copy",
                exposure_command(admin, SYSTEM_KIND, link, true, ask.clone()),
            ),
        ] {
            let trace = message.routing.trace_id.clone();
            let expected = ExposureCommand::of(&message).expect(case).response();
            router.inject(message).await.unwrap();
            let answer = router
                .next_outgoing_within(Duration::from_secs(2))
                .await
                .unwrap_or_else(|| panic!("{case}: answered"));
            assert_eq!(answer.meta.msg.as_deref(), Some(expected), "{case}");
            assert_eq!(answer.routing.trace_id, trace, "{case}");
            assert_eq!(answer.payload["status"], "error", "{case}");
            assert_eq!(
                answer.payload["error_code"], IO_WEB_ERROR_UNAUTHORIZED,
                "{case}"
            );
            assert_eq!(
                exposures.view(now)["generation"],
                0,
                "{case}: nothing taken"
            );
            assert_eq!(exposures.view(now)["links_issued"], 0, "{case}: no link");
        }

        router
            .inject(exposure_command(admin, SYSTEM_KIND, push, false, set))
            .await
            .unwrap();
        let answer = router
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("SY.admin's push is answered");
        assert_eq!(
            answer.meta.msg.as_deref(),
            Some(MSG_IO_WEB_EXPOSURES_RESPONSE)
        );
        assert!(
            matches!(
                &answer.routing.dst,
                Destination::Unicast(to) if to == "00000000-0000-4000-8000-0000000000ad"
            ),
            "answered to SY.admin's own uuid: {:?}",
            answer.routing.dst
        );
        assert_eq!(answer.payload["status"], "ok");
        assert_eq!(answer.payload["generation"], 5);
        assert_eq!(answer.payload["enablements"][0]["key"], "ready");

        router
            .inject(exposure_command(admin, SYSTEM_KIND, link, false, ask))
            .await
            .unwrap();
        let answer = router
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("SY.admin's link is answered");
        assert_eq!(
            answer.meta.msg.as_deref(),
            Some(MSG_IO_WEB_ACCESS_LINK_RESPONSE)
        );
        assert_eq!(answer.payload["status"], "ok");
        assert!(answer.payload["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("https://{SHOP}/.fluxbee/enter?code=L1.")));

        // A payload io.web cannot read is not taken.
        router
            .inject(exposure_command(
                admin,
                SYSTEM_KIND,
                push,
                false,
                json!({"generation": "x"}),
            ))
            .await
            .unwrap();
        let answer = router
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("answered");
        assert_eq!(answer.payload["error_code"], INVALID_REQUEST);
        assert_eq!(exposures.view(now)["generation"], 5);
        remove(&exposures);
    }

    /// Whatever else reaches io.web, unrelated messages and the commands' names in another kind
    /// or spelling, has a channel of its own and never enters the exposures' bounded lanes, so a
    /// flood of it cannot get SY.admin's commands refused NOT_TAKEN. There the variants are
    /// answered UNAUTHORIZED, and the rest is dropped unanswered.
    #[tokio::test]
    async fn the_catch_all_never_reaches_the_exposures_channel() {
        use fluxbee_sdk::RouterDispatcherTestHarness;

        let (dispatcher, mut router) =
            RouterDispatcherTestHarness::new(NODE, io_web_profile().expect("io.web's profile"));
        let mut commands = dispatcher
            .take_command_receiver(RPC_CH_EXPOSURES)
            .await
            .expect("the exposures' channel");
        tokio::spawn(run_unserved(
            dispatcher
                .take_command_receiver(RPC_CH_UNSERVED)
                .await
                .expect("the unserved channel"),
            dispatcher.sender_snapshot(),
        ));
        let admin = Some(EXPOSURES_AUTHORITY);
        let unrelated = [
            exposure_command(admin, SYSTEM_KIND, "VAULT_CHANGED", false, json!({})),
            exposure_command(
                Some("AI.chat@motherbee"),
                "user",
                "hello",
                false,
                json!({"text": "hi"}),
            ),
            exposure_command(None, "user", "", false, json!({})),
        ];
        let variants = [
            exposure_command(admin, "user", MSG_IO_WEB_EXPOSURES, false, json!({})),
            exposure_command(admin, SYSTEM_KIND, "io_web_exposures", false, json!({})),
            exposure_command(admin, SYSTEM_KIND, " IO_WEB_ACCESS_LINK", false, json!({})),
        ];
        let mut refused: Vec<String> = variants
            .iter()
            .map(|message| message.routing.trace_id.clone())
            .collect();
        refused.sort();
        for message in unrelated.into_iter().chain(variants) {
            router.inject(message).await.unwrap();
        }
        // SY.admin's push comes last, and is the first message the exposures' channel holds.
        let push = exposure_command(
            admin,
            SYSTEM_KIND,
            MSG_IO_WEB_EXPOSURES,
            false,
            json!({"generation": 1, "exposures": []}),
        );
        let trace = push.routing.trace_id.clone();
        router.inject(push).await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(2), commands.recv())
            .await
            .expect("the push reaches the exposures' channel")
            .expect("the channel is open");
        assert_eq!(first.routing.trace_id, trace, "nothing came before it");
        assert!(commands.try_recv().is_none(), "nothing came after it");

        // Each variant is refused on its own trace; a channel reads its system lane first, so not
        // in the order they came.
        let mut answers = Vec::new();
        for _ in 0..refused.len() {
            let answer = router
                .next_outgoing_within(Duration::from_secs(2))
                .await
                .expect("a variant is answered");
            assert_eq!(answer.payload["error_code"], IO_WEB_ERROR_UNAUTHORIZED);
            answers.push(answer.routing.trace_id);
        }
        answers.sort();
        assert_eq!(answers, refused);
        assert!(
            router
                .next_outgoing_within(Duration::from_millis(200))
                .await
                .is_none(),
            "the rest goes unanswered"
        );
    }

    /// io.web's copy of the exposures lies beside its config.json.
    #[test]
    fn the_exposures_file_lies_beside_the_config() {
        assert_eq!(
            exposures_file(NODE, Path::new("/var/lib/fluxbee/nodes")),
            Path::new("/var/lib/fluxbee/nodes/IO/IO.web@motherbee/exposures.json")
        );
    }
}
