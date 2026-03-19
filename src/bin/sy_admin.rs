use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use std::future;
use tar::{Archive, Builder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::nats::{NatsClient, NatsRequestEnvelope, NatsResponseEnvelope};
use fluxbee_sdk::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, SCOPE_GLOBAL,
    SYSTEM_KIND,
};
use fluxbee_sdk::{connect, ClientConfig, NodeConfig, NodeReceiver, NodeSender};
use json_router::shm::{
    now_epoch_ms, LsaRegionReader, RemoteHiveEntry, FLAG_DELETED, FLAG_STALE, HEARTBEAT_STALE_MS,
};

type AdminError = Box<dyn std::error::Error + Send + Sync>;
const PRIMARY_HIVE_ID: &str = "motherbee";
const MSG_ADMIN_COMMAND: &str = "ADMIN_COMMAND";
const MSG_ADMIN_COMMAND_RESPONSE: &str = "ADMIN_COMMAND_RESPONSE";

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    admin: Option<AdminSection>,
    wan: Option<WanSection>,
    storage: Option<StorageSection>,
}

#[derive(Debug, Deserialize)]
struct AdminSection {
    listen: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    authorized_hives: Option<Vec<String>>,
}

#[derive(Clone)]
struct AdminContext {
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    hive_id: String,
    authorized_hives: Vec<String>,
    nats_endpoint: String,
    nats_client: Arc<NatsClient>,
    command_lock: Arc<Mutex<()>>,
}

struct AdminRouterClient {
    sender: NodeSender,
    pending_admin: Mutex<HashMap<String, oneshot::Sender<Message>>>,
    system_tx: broadcast::Sender<Message>,
    query_tx: broadcast::Sender<Message>,
    internal_admin_tx: broadcast::Sender<Message>,
}

impl AdminRouterClient {
    fn new(sender: NodeSender) -> Self {
        let (system_tx, _) = broadcast::channel(256);
        let (query_tx, _) = broadcast::channel(256);
        let (internal_admin_tx, _) = broadcast::channel(256);
        Self {
            sender,
            pending_admin: Mutex::new(HashMap::new()),
            system_tx,
            query_tx,
            internal_admin_tx,
        }
    }

    async fn connect_with_retry(
        config: NodeConfig,
        delay: Duration,
    ) -> Result<Arc<Self>, fluxbee_sdk::NodeError> {
        loop {
            match connect(&config).await {
                Ok((sender, receiver)) => {
                    let client = Arc::new(Self::new(sender));
                    client.start(receiver);
                    return Ok(client);
                }
                Err(err) => {
                    tracing::warn!("connect failed: {err}");
                    time::sleep(delay).await;
                }
            }
        }
    }

    fn start(self: &Arc<Self>, receiver: NodeReceiver) {
        let client = Arc::clone(self);
        tokio::spawn(async move {
            client.recv_loop(receiver).await;
        });
    }

    async fn recv_loop(self: Arc<Self>, mut receiver: NodeReceiver) {
        loop {
            match receiver.recv().await {
                Ok(msg) => self.dispatch(msg).await,
                Err(err) => {
                    tracing::warn!("router recv error: {err}");
                }
            }
        }
    }

    async fn dispatch(&self, msg: Message) {
        if msg.meta.msg_type == "admin" || msg.meta.msg_type == SYSTEM_KIND {
            let mut pending = self.pending_admin.lock().await;
            if let Some(tx) = pending.remove(&msg.routing.trace_id) {
                let _ = tx.send(msg);
                return;
            }
        }
        if msg.meta.msg_type == "admin" && msg.meta.msg.as_deref() == Some(MSG_ADMIN_COMMAND) {
            let _ = self.internal_admin_tx.send(msg);
            return;
        }
        if msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some("CONFIG_RESPONSE") {
            let _ = self.system_tx.send(msg);
            return;
        }
        if msg.meta.msg_type == "query_response" {
            let _ = self.query_tx.send(msg);
        }
    }

    async fn enqueue_admin_waiter(&self, trace_id: String, tx: oneshot::Sender<Message>) {
        let mut pending = self.pending_admin.lock().await;
        pending.insert(trace_id, tx);
    }

    async fn drop_admin_waiter(&self, trace_id: &str) {
        let mut pending = self.pending_admin.lock().await;
        pending.remove(trace_id);
    }

    fn subscribe_system(&self) -> broadcast::Receiver<Message> {
        self.system_tx.subscribe()
    }

    fn subscribe_query(&self) -> broadcast::Receiver<Message> {
        self.query_tx.subscribe()
    }

    fn subscribe_internal_admin(&self) -> broadcast::Receiver<Message> {
        self.internal_admin_tx.subscribe()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct RouteConfig {
    prefix: String,
    #[serde(default)]
    match_kind: Option<String>,
    action: String,
    #[serde(default)]
    next_hop_hive: Option<String>,
    #[serde(default)]
    metric: Option<u32>,
    #[serde(default)]
    priority: Option<u16>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct VpnConfig {
    pattern: String,
    #[serde(default)]
    match_kind: Option<String>,
    vpn_id: u32,
    #[serde(default)]
    priority: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<(), AdminError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_admin supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();

    let hive = load_hive(&config_dir)?;
    let hive_id = hive.hive_id.clone();
    let authorized_hives = hive
        .wan
        .as_ref()
        .and_then(|wan| wan.authorized_hives.clone())
        .unwrap_or_default();
    if hive.hive_id == PRIMARY_HIVE_ID && !is_mother_role(hive.role.as_deref()) {
        return Err(format!(
            "invalid hive.yaml: hive_id='{}' is reserved for role=motherbee",
            PRIMARY_HIVE_ID
        )
        .into());
    }
    if !is_mother_role(hive.role.as_deref()) {
        tracing::warn!("SY.admin solo corre en motherbee; role != motherbee");
        return Ok(());
    }
    if hive.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: role=motherbee requires hive_id='{}' (got '{}')",
            PRIMARY_HIVE_ID, hive.hive_id
        )
        .into());
    }
    // Default to loopback for safer deployments; expose externally only via explicit config/env.
    let admin_listen = std::env::var("JSR_ADMIN_LISTEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| hive.admin.as_ref().and_then(|admin| admin.listen.clone()))
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: socket_dir.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let client_config = ClientConfig::new(node_config.clone());
    let router_client = AdminRouterClient::connect_with_retry(node_config, Duration::from_secs(1))
        .await
        .map_err(|err| {
            tracing::error!("router client connect failed: {err}");
            err
        })?;

    let (broadcast_tx, broadcast_rx) = mpsc::unbounded_channel::<BroadcastRequest>();
    let http_tx = broadcast_tx.clone();
    let nats_client = Arc::new(NatsClient::from_client_config(&client_config)?);
    let command_lock = Arc::new(Mutex::new(()));
    let http_ctx = AdminContext {
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        socket_dir: socket_dir.clone(),
        hive_id,
        authorized_hives,
        nats_endpoint: nats_client.endpoint().to_string(),
        nats_client,
        command_lock,
    };
    let internal_ctx = http_ctx.clone();
    let http_client = router_client.clone();
    tokio::spawn(async move {
        if let Err(err) = run_http_server(&admin_listen, &http_tx, http_ctx, http_client).await {
            tracing::error!("http server error: {err}");
        }
    });

    let loop_client = router_client.clone();
    tokio::spawn(async move {
        run_broadcast_loop(broadcast_rx, loop_client).await;
    });

    let internal_client = router_client.clone();
    let internal_rx = router_client.subscribe_internal_admin();
    tokio::spawn(async move {
        run_internal_admin_loop(internal_ctx, internal_client, internal_rx).await;
    });

    future::pending::<()>().await;
    Ok(())
}

enum BroadcastRequest {
    Routes {
        routes: Vec<RouteConfig>,
        version: u64,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Vpns {
        vpns: Vec<VpnConfig>,
        version: u64,
        ack: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Deserialize)]
struct OpaRequest {
    #[serde(default)]
    rego: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default, alias = "target")]
    hive: Option<String>,
}

async fn run_broadcast_loop(
    mut rx: mpsc::UnboundedReceiver<BroadcastRequest>,
    client: Arc<AdminRouterClient>,
) {
    while let Some(req) = rx.recv().await {
        let result = match req {
            BroadcastRequest::Routes {
                routes,
                version,
                ack,
            } => {
                match broadcast_config_changed(
                    &client,
                    "routes",
                    None,
                    None,
                    version,
                    serde_json::json!({ "routes": routes }),
                    None,
                )
                .await
                {
                    Ok(()) => {
                        let _ = ack.send(Ok(()));
                        Ok(())
                    }
                    Err(err) => {
                        let err_msg = err.to_string();
                        let _ = ack.send(Err(err_msg.clone()));
                        tracing::warn!(error = %err, "broadcast failed");
                        Err(err_msg)
                    }
                }
            }
            BroadcastRequest::Vpns { vpns, version, ack } => {
                match broadcast_config_changed(
                    &client,
                    "vpn",
                    None,
                    None,
                    version,
                    serde_json::json!({ "vpns": vpns }),
                    None,
                )
                .await
                {
                    Ok(()) => {
                        let _ = ack.send(Ok(()));
                        Ok(())
                    }
                    Err(err) => {
                        let err_msg = err.to_string();
                        let _ = ack.send(Err(err_msg.clone()));
                        tracing::warn!(error = %err, "broadcast failed");
                        Err(err_msg)
                    }
                }
            }
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "broadcast failed; message dropped");
        }
    }
}

async fn send_broadcast_request<F>(
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    build: F,
    label: &str,
) -> Result<(), AdminError>
where
    F: FnOnce(oneshot::Sender<Result<(), String>>) -> BroadcastRequest,
{
    let (ack_tx, ack_rx) = oneshot::channel::<Result<(), String>>();
    tx.send(build(ack_tx))
        .map_err(|_| format!("broadcast queue closed for {label}"))?;

    let timeout_secs = env_timeout_secs("JSR_ADMIN_BROADCAST_TIMEOUT_SECS").unwrap_or(5);
    match time::timeout(Duration::from_secs(timeout_secs), ack_rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(err))) => Err(format!("broadcast failed for {label}: {err}").into()),
        Ok(Err(_)) => Err(format!("broadcast ack channel closed for {label}").into()),
        Err(_) => Err(format!("broadcast ack timeout for {label} ({timeout_secs}s)").into()),
    }
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, AdminError> {
    let data = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn config_changed_version_channel(subsystem: &str) -> &'static str {
    match subsystem {
        // routes + vpn share version stream because SY.config.routes keeps one config version.
        "routes" | "vpn" | "vpns" => "routes-vpns",
        "storage" => "storage",
        _ => "general",
    }
}

fn config_changed_version_path(state_dir: &Path, subsystem: &str) -> PathBuf {
    state_dir
        .join("config_versions")
        .join(format!("{}.txt", config_changed_version_channel(subsystem)))
}

fn allocate_config_changed_versions(
    state_dir: &Path,
    subsystem: &str,
    count: usize,
    requested_start: Option<u64>,
) -> Result<Vec<u64>, AdminError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let path = config_changed_version_path(state_dir, subsystem);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = fs::read_to_string(&path)
        .ok()
        .and_then(|data| data.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let start = match requested_start {
        Some(requested) => {
            if requested <= current {
                return Err(
                    format!("version must be greater than current (current={current})").into(),
                );
            }
            requested
        }
        None => current.saturating_add(1),
    };
    let end = start.saturating_add(count as u64 - 1);
    fs::write(&path, end.to_string())?;
    Ok((start..=end).collect())
}

fn next_config_changed_version(
    state_dir: &Path,
    subsystem: &str,
    requested: Option<u64>,
) -> Result<u64, AdminError> {
    let mut versions = allocate_config_changed_versions(state_dir, subsystem, 1, requested)?;
    Ok(versions.remove(0))
}

async fn broadcast_config_changed(
    client: &AdminRouterClient,
    subsystem: &str,
    action: Option<String>,
    auto_apply: Option<bool>,
    version: u64,
    config: serde_json::Value,
    target_hive: Option<String>,
) -> Result<(), AdminError> {
    let dst = match target_hive {
        Some(hive) => Destination::Unicast(format!("SY.opa.rules@{}", hive)),
        None => Destination::Broadcast,
    };
    let msg = Message {
        routing: Routing {
            src: client.sender.uuid().to_string(),
            dst,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_CONFIG_CHANGED.to_string()),
            src_ilk: None,
            scope: Some(SCOPE_GLOBAL.to_string()),
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload: serde_json::to_value(ConfigChangedPayload {
            subsystem: subsystem.to_string(),
            action: action.clone(),
            auto_apply,
            version,
            config,
        })?,
    };
    client.sender.send(msg).await?;
    tracing::info!(
        subsystem = subsystem,
        action = action.as_deref().unwrap_or(""),
        version = version,
        "config changed broadcast sent"
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ConfigUpdate {
    #[serde(default)]
    routes: Option<Vec<RouteConfig>>,
    #[serde(default)]
    vpns: Option<Vec<VpnConfig>>,
    #[serde(default)]
    version: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StorageUpdate {
    path: String,
    #[serde(default)]
    version: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StorageSection {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug)]
struct AdminRequest {
    action: String,
    payload: serde_json::Value,
    target: String,
    unicast: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InternalAdminCommandPayload {
    action: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    params: serde_json::Value,
    #[serde(default)]
    request_id: Option<String>,
}

async fn run_internal_admin_loop(
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
    mut rx: broadcast::Receiver<Message>,
) {
    loop {
        match rx.recv().await {
            Ok(msg) => {
                if let Err(err) = handle_internal_admin_command(&ctx, &client, &msg).await {
                    tracing::warn!(error = %err, "internal admin command handling failed");
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped = skipped,
                    "internal admin loop lagged; dropping stale commands"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn handle_internal_admin_command(
    ctx: &AdminContext,
    client: &Arc<AdminRouterClient>,
    msg: &Message,
) -> Result<(), AdminError> {
    if msg.meta.msg_type != "admin" || msg.meta.msg.as_deref() != Some(MSG_ADMIN_COMMAND) {
        return Ok(());
    }

    let payload = serde_json::from_value::<InternalAdminCommandPayload>(msg.payload.clone());
    let parsed = match payload {
        Ok(parsed) => parsed,
        Err(err) => {
            let detail = serde_json::json!({ "message": err.to_string() });
            return send_admin_command_response(
                client,
                msg,
                "",
                "error",
                serde_json::Value::Null,
                Some("INVALID_REQUEST".to_string()),
                Some(detail),
                None,
            )
            .await;
        }
    };

    let action = match parsed
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(action) => action.to_string(),
        None => {
            let detail = serde_json::json!({ "message": "missing payload.action" });
            return send_admin_command_response(
                client,
                msg,
                "",
                "error",
                serde_json::Value::Null,
                Some("INVALID_REQUEST".to_string()),
                Some(detail),
                parsed.request_id,
            )
            .await;
        }
    };

    let internal = {
        let _command_guard = ctx.command_lock.lock().await;
        dispatch_internal_admin_command(
            ctx,
            client,
            &action,
            parsed.target.as_deref(),
            parsed.params,
        )
        .await?
    };
    let status = internal
        .envelope
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(if internal.http_status < 400 {
            "ok"
        } else {
            "error"
        });
    let action_out = internal
        .envelope
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or(&action)
        .to_string();
    let payload = internal
        .envelope
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let error_code = internal
        .envelope
        .get("error_code")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let error_detail = internal
        .envelope
        .get("error_detail")
        .cloned()
        .and_then(|v| if v.is_null() { None } else { Some(v) });

    send_admin_command_response(
        client,
        msg,
        &action_out,
        status,
        payload,
        error_code,
        error_detail,
        parsed.request_id,
    )
    .await
}

async fn send_admin_command_response(
    client: &AdminRouterClient,
    request_msg: &Message,
    action: &str,
    status: &str,
    payload: serde_json::Value,
    error_code: Option<String>,
    error_detail: Option<serde_json::Value>,
    request_id: Option<String>,
) -> Result<(), AdminError> {
    let mut body = serde_json::json!({
        "status": status,
        "action": action,
        "payload": payload,
        "error_code": error_code,
        "error_detail": error_detail,
    });
    if let Some(req_id) = request_id {
        body["request_id"] = serde_json::Value::String(req_id);
    }

    let response = Message {
        routing: Routing {
            src: client.sender.uuid().to_string(),
            dst: Destination::Unicast(request_msg.routing.src.clone()),
            ttl: 16,
            trace_id: request_msg.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: Some(MSG_ADMIN_COMMAND_RESPONSE.to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: Some(action.to_string()),
            priority: None,
            context: None,
        },
        payload: body,
    };
    client.sender.send(response).await?;
    Ok(())
}

#[derive(Debug)]
struct InternalAdminDispatchResult {
    http_status: u16,
    envelope: serde_json::Value,
}

#[derive(Clone, Copy)]
enum InternalActionRoute {
    Query(&'static str),
    Command(&'static str),
    Update,
    SyncHint,
    Inventory,
    OpaHttp(OpaAction),
    OpaQuery(&'static str),
}

#[derive(Clone, Copy)]
struct InternalActionSpec {
    action: &'static str,
    route: InternalActionRoute,
    requires_target: bool,
    allow_legacy_hive_id: bool,
}

const INTERNAL_ACTION_REGISTRY_VERSION: &str = "1";

const INTERNAL_ACTION_REGISTRY: &[InternalActionSpec] = &[
    InternalActionSpec {
        action: "hive_status",
        route: InternalActionRoute::Query("hive_status"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_hives",
        route: InternalActionRoute::Query("list_hives"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_admin_actions",
        route: InternalActionRoute::Query("list_admin_actions"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_versions",
        route: InternalActionRoute::Query("list_versions"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_versions",
        route: InternalActionRoute::Query("get_versions"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_routes",
        route: InternalActionRoute::Query("list_routes"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_vpns",
        route: InternalActionRoute::Query("list_vpns"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_nodes",
        route: InternalActionRoute::Query("list_nodes"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_storage",
        route: InternalActionRoute::Query("get_storage"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "add_hive",
        route: InternalActionRoute::Command("add_hive"),
        requires_target: false,
        allow_legacy_hive_id: true,
    },
    InternalActionSpec {
        action: "get_hive",
        route: InternalActionRoute::Command("get_hive"),
        requires_target: false,
        allow_legacy_hive_id: true,
    },
    InternalActionSpec {
        action: "remove_hive",
        route: InternalActionRoute::Command("remove_hive"),
        requires_target: false,
        allow_legacy_hive_id: true,
    },
    InternalActionSpec {
        action: "add_route",
        route: InternalActionRoute::Command("add_route"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "delete_route",
        route: InternalActionRoute::Command("delete_route"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "add_vpn",
        route: InternalActionRoute::Command("add_vpn"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "delete_vpn",
        route: InternalActionRoute::Command("delete_vpn"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "run_node",
        route: InternalActionRoute::Command("run_node"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "kill_node",
        route: InternalActionRoute::Command("kill_node"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_node_config",
        route: InternalActionRoute::Command("get_node_config"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "set_node_config",
        route: InternalActionRoute::Command("set_node_config"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_node_state",
        route: InternalActionRoute::Command("get_node_state"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_node_status",
        route: InternalActionRoute::Command("get_node_status"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_deployments",
        route: InternalActionRoute::Command("list_deployments"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_deployments",
        route: InternalActionRoute::Command("get_deployments"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_drift_alerts",
        route: InternalActionRoute::Command("list_drift_alerts"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_drift_alerts",
        route: InternalActionRoute::Command("get_drift_alerts"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "set_storage",
        route: InternalActionRoute::Command("set_storage"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "update",
        route: InternalActionRoute::Update,
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "sync_hint",
        route: InternalActionRoute::SyncHint,
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "inventory",
        route: InternalActionRoute::Inventory,
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_compile_apply",
        route: InternalActionRoute::OpaHttp(OpaAction::CompileApply),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_compile",
        route: InternalActionRoute::OpaHttp(OpaAction::Compile),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_apply",
        route: InternalActionRoute::OpaHttp(OpaAction::Apply),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_rollback",
        route: InternalActionRoute::OpaHttp(OpaAction::Rollback),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_check",
        route: InternalActionRoute::OpaHttp(OpaAction::Check),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_get_policy",
        route: InternalActionRoute::OpaQuery("get_policy"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_get_status",
        route: InternalActionRoute::OpaQuery("get_status"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
];

async fn dispatch_internal_admin_command(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target: Option<&str>,
    params: serde_json::Value,
) -> Result<InternalAdminDispatchResult, AdminError> {
    let spec = match resolve_internal_action_spec(action) {
        Ok(route) => route,
        Err(detail) => return Ok(internal_invalid_request(action, detail)),
    };

    if let Some(detail) = internal_admin_payload_contract_error(spec, &params) {
        return Ok(internal_invalid_request(action, &detail));
    }

    let (status, body) = match spec.route {
        InternalActionRoute::Query(canonical) => {
            let hive = resolve_internal_action_hive(canonical, target, &params);
            if spec.requires_target && hive.is_none() {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            }
            handle_admin_query(ctx, client, canonical, hive).await?
        }
        InternalActionRoute::Command(canonical) => {
            let hive = resolve_internal_action_hive(canonical, target, &params);
            if spec.requires_target && hive.is_none() {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            }
            handle_admin_command(ctx, client, canonical, params, hive).await?
        }
        InternalActionRoute::Update => {
            let hive = resolve_internal_action_hive("update", target, &params);
            let Some(hive_id) = hive else {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            };
            handle_hive_update_command(ctx, client, hive_id, params).await?
        }
        InternalActionRoute::SyncHint => {
            let hive = resolve_internal_action_hive("sync_hint", target, &params);
            let Some(hive_id) = hive else {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            };
            handle_hive_sync_hint_command(ctx, client, hive_id, params).await?
        }
        InternalActionRoute::Inventory => handle_inventory_http(ctx, client, params).await?,
        InternalActionRoute::OpaHttp(opa_action) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            let req = parse_internal_opa_request(params, hive)
                .map_err(|detail| -> AdminError { detail.into() })?;
            handle_opa_http(ctx, client, req, opa_action).await?
        }
        InternalActionRoute::OpaQuery(query_action) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            handle_opa_query(ctx, client, query_action, hive.clone()).await?
        }
    };

    Ok(InternalAdminDispatchResult {
        http_status: status,
        envelope: parse_internal_response_envelope(action, status, &body),
    })
}

fn parse_internal_response_envelope(
    action: &str,
    http_status: u16,
    body: &str,
) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) if value.is_object() => value,
        Ok(_) | Err(_) => serde_json::json!({
            "status": "error",
            "action": action,
            "payload": serde_json::Value::Null,
            "error_code": "TRANSPORT_ERROR",
            "error_detail": format!("invalid response envelope (http_status={http_status})"),
        }),
    }
}

fn normalize_internal_target_hive(target: Option<&str>) -> Option<String> {
    let raw = target?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some((_, hive)) = raw.rsplit_once('@') {
        let hive = hive.trim();
        if !hive.is_empty() {
            return Some(hive.to_string());
        }
    }
    Some(raw.to_string())
}

fn internal_admin_payload_contract_error(
    action: &InternalActionSpec,
    params: &serde_json::Value,
) -> Option<String> {
    if !params.is_object() {
        return None;
    }
    if params.get("hive_id").is_some() && !action.allow_legacy_hive_id {
        return Some(
            "legacy field 'hive_id' is not supported for this action; use payload.target"
                .to_string(),
        );
    }
    None
}

fn resolve_internal_action_hive(
    action: &str,
    target: Option<&str>,
    params: &serde_json::Value,
) -> Option<String> {
    if let Some(node_hive) = node_name_hive_from_payload(action, params).map(str::to_string) {
        return Some(node_hive);
    }
    normalize_internal_target_hive(target)
}

fn resolve_internal_action_spec(action: &str) -> Result<&'static InternalActionSpec, &'static str> {
    debug_assert!(!INTERNAL_ACTION_REGISTRY_VERSION.is_empty());
    if matches!(
        action,
        "list_routers" | "get_routers" | "add_router" | "delete_router"
    ) {
        return Err(
            "legacy router action is not supported; use inventory/list_nodes/list_routes/list_vpns",
        );
    }
    INTERNAL_ACTION_REGISTRY
        .iter()
        .find(|spec| spec.action == action)
        .ok_or("unknown action")
}

fn parse_internal_opa_request(
    params: serde_json::Value,
    default_hive: Option<String>,
) -> Result<OpaRequest, String> {
    if !params.is_null() && !params.is_object() {
        return Err("invalid params: expected JSON object for OPA action".to_string());
    }
    let mut req = if params.is_null() {
        OpaRequest {
            rego: None,
            entrypoint: None,
            version: None,
            action: None,
            hive: None,
        }
    } else {
        serde_json::from_value::<OpaRequest>(params)
            .map_err(|err| format!("invalid OPA params: {err}"))?
    };
    if req.hive.is_none() {
        req.hive = default_hive;
    }
    Ok(req)
}

fn internal_invalid_request(action: &str, detail: &str) -> InternalAdminDispatchResult {
    InternalAdminDispatchResult {
        http_status: 400,
        envelope: serde_json::json!({
            "status": "error",
            "action": action,
            "payload": serde_json::Value::Null,
            "error_code": "INVALID_REQUEST",
            "error_detail": detail,
        }),
    }
}

#[derive(Clone, Copy)]
enum OpaAction {
    Compile,
    CompileApply,
    Apply,
    Rollback,
    Check,
}

impl OpaAction {
    fn as_str(&self) -> &'static str {
        match self {
            OpaAction::Compile => "compile",
            OpaAction::CompileApply => "compile",
            OpaAction::Apply => "apply",
            OpaAction::Rollback => "rollback",
            OpaAction::Check => "compile",
        }
    }

    fn needs_rego(&self) -> bool {
        matches!(
            self,
            OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check
        )
    }

    fn apply_after(&self) -> bool {
        matches!(self, OpaAction::CompileApply)
    }

    fn check_only(&self) -> bool {
        matches!(self, OpaAction::Check)
    }

    fn auto_apply_flag(&self) -> Option<bool> {
        match self {
            OpaAction::Compile => Some(false),
            OpaAction::CompileApply => Some(false),
            OpaAction::Check => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigResponsePayload {
    subsystem: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    hive: Option<String>,
    #[serde(default)]
    compile_time_ms: Option<u64>,
    #[serde(default)]
    wasm_size_bytes: Option<u64>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpaResponseEntry {
    hive: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compile_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wasm_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpaQueryEntry {
    hive: String,
    status: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct StorageInboxMetrics {
    pending: i64,
    pending_with_error: i64,
    oldest_pending_age_s: i64,
    processed_total: i64,
    max_attempts: i32,
}

const SUBJECT_STORAGE_METRICS_GET: &str = "storage.metrics.get";
const STORAGE_METRICS_NATS_TIMEOUT_SECS: u64 = 8;

async fn run_http_server(
    listen: &str,
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
) -> Result<(), AdminError> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(addr = %listen, "sy.admin http listening");
    loop {
        let (mut stream, _) = listener.accept().await?;
        let tx = tx.clone();
        let ctx = ctx.clone();
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_http(&mut stream, &tx, &ctx, &client).await {
                tracing::warn!("http handler error: {err}");
            }
        });
    }
}

async fn handle_http(
    stream: &mut tokio::net::TcpStream,
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: &AdminContext,
    client: &AdminRouterClient,
) -> Result<(), AdminError> {
    let (method, path, headers, body) = read_http_request(stream).await?;
    let (path, query) = split_path_query(&path);
    if method == "GET" && path == "/health" {
        respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        return Ok(());
    }

    // FR9-T5: single global command lock shared by HTTP and internal socket commands.
    // Contention is blocking (wait), never immediate reject.
    let _command_guard = ctx.command_lock.lock().await;

    if method == "GET" {
        if path == "/inventory" {
            let mut payload = serde_json::json!({ "scope": "global" });
            if let Some(kind) = query
                .get("type")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["filter_type"] = serde_json::Value::String(kind.to_ascii_uppercase());
            }
            if let Some(hive_id) = query
                .get("hive")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["scope"] = serde_json::Value::String("hive".to_string());
                payload["filter_hive"] = serde_json::Value::String(hive_id.to_string());
            }
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
        if path == "/inventory/summary" {
            let payload = serde_json::json!({ "scope": "summary" });
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
        if let Some(hive_id) = path
            .strip_prefix("/inventory/")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| *value != "summary")
        {
            let mut payload = serde_json::json!({
                "scope": "hive",
                "filter_hive": decode_percent(hive_id),
            });
            if let Some(kind) = query
                .get("type")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["filter_type"] = serde_json::Value::String(kind.to_ascii_uppercase());
            }
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
    }
    if let Some((status, resp)) =
        handle_hive_paths(method.as_str(), path, &query, &body, ctx, client).await?
    {
        respond_json(stream, status, &resp).await?;
        return Ok(());
    }
    if let Some((status, resp, content_type)) =
        handle_modules_paths(method.as_str(), path, &body).await?
    {
        match content_type {
            Some(ct) => respond_bytes(stream, status, &ct, &resp).await?,
            None => {
                let body = String::from_utf8_lossy(&resp);
                respond_json(stream, status, &body).await?
            }
        }
        return Ok(());
    }
    match (method.as_str(), path) {
        ("GET", "/admin/actions") => {
            let (status, resp) =
                handle_admin_query(ctx, client, "list_admin_actions", None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/hive/status") => {
            let (status, resp) = handle_admin_query(ctx, client, "hive_status", None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/versions") => {
            let hive = query.get("hive").cloned();
            if hive.is_some() {
                let (status, resp) = handle_admin_query(ctx, client, "get_versions", hive).await?;
                respond_json(stream, status, &resp).await?;
            } else {
                let (status, resp) = handle_admin_query(ctx, client, "list_versions", None).await?;
                respond_json(stream, status, &resp).await?;
            }
        }
        ("GET", "/deployments") => {
            let hive = query.get("hive").cloned();
            let payload = deployments_payload_from_query(&query);
            if hive.is_some() {
                let (status, resp) =
                    handle_admin_command(ctx, client, "get_deployments", payload, hive).await?;
                respond_json(stream, status, &resp).await?;
            } else {
                let (status, resp) =
                    handle_admin_command(ctx, client, "list_deployments", payload, None).await?;
                respond_json(stream, status, &resp).await?;
            }
        }
        ("GET", "/drift-alerts") => {
            let hive = query.get("hive").cloned();
            let payload = drift_alerts_payload_from_query(&query);
            if hive.is_some() {
                let (status, resp) =
                    handle_admin_command(ctx, client, "get_drift_alerts", payload, hive).await?;
                respond_json(stream, status, &resp).await?;
            } else {
                let (status, resp) =
                    handle_admin_command(ctx, client, "list_drift_alerts", payload, None).await?;
                respond_json(stream, status, &resp).await?;
            }
        }
        ("GET", "/routes") => {
            let hive = query.get("hive").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_routes", hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/routes") => {
            let route: RouteConfig = serde_json::from_slice(&body)?;
            let hive = query.get("hive").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "add_route", serde_json::to_value(route)?, hive)
                    .await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/routes") => {
            let prefix = query.get("prefix").cloned().unwrap_or_default();
            let hive = query.get("hive").cloned();
            let payload = serde_json::json!({ "prefix": prefix });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_route", payload, hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/vpns") => {
            let hive = query.get("hive").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_vpns", hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/vpns") => {
            let vpn: VpnConfig = serde_json::from_slice(&body)?;
            let hive = query.get("hive").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "add_vpn", serde_json::to_value(vpn)?, hive)
                    .await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/vpns") => {
            let pattern = query.get("pattern").cloned().unwrap_or_default();
            let hive = query.get("hive").cloned();
            let payload = serde_json::json!({ "pattern": pattern });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_vpn", payload, hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("PUT", "/config/routes") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            let mut broadcasts = usize::from(update.routes.is_some());
            broadcasts += usize::from(update.vpns.is_some());
            let mut versions = match allocate_config_changed_versions(
                &ctx.state_dir,
                "routes",
                broadcasts,
                update.version,
            ) {
                Ok(versions) => versions,
                Err(err) => {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "VERSION_MISMATCH",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 409, &body).await?;
                    return Ok(());
                }
            };
            if let Some(routes) = update.routes {
                let version = versions.remove(0);
                if let Err(err) = send_broadcast_request(
                    tx,
                    |ack| BroadcastRequest::Routes {
                        routes,
                        version,
                        ack,
                    },
                    "routes",
                )
                .await
                {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "CONFIG_BROADCAST_FAILED",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 502, &body).await?;
                    return Ok(());
                }
            }
            if let Some(vpns) = update.vpns {
                let version = versions.remove(0);
                if let Err(err) = send_broadcast_request(
                    tx,
                    |ack| BroadcastRequest::Vpns { vpns, version, ack },
                    "vpns",
                )
                .await
                {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "CONFIG_BROADCAST_FAILED",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 502, &body).await?;
                    return Ok(());
                }
            }
            tracing::info!("config routes update received");
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("PUT", "/config/vpns") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            if let Some(vpns) = update.vpns {
                let version =
                    match next_config_changed_version(&ctx.state_dir, "vpn", update.version) {
                        Ok(version) => version,
                        Err(err) => {
                            let body = serde_json::json!({
                                "status": "error",
                                "error_code": "VERSION_MISMATCH",
                                "error_detail": err.to_string(),
                            })
                            .to_string();
                            respond_json(stream, 409, &body).await?;
                            return Ok(());
                        }
                    };
                if let Err(err) = send_broadcast_request(
                    tx,
                    |ack| BroadcastRequest::Vpns { vpns, version, ack },
                    "vpns",
                )
                .await
                {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "CONFIG_BROADCAST_FAILED",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 502, &body).await?;
                    return Ok(());
                }
            }
            tracing::info!("config vpns update received");
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("GET", "/config/storage") => {
            let (status, resp) = handle_admin_query(ctx, client, "get_storage", None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/config/storage/metrics") => {
            let (status, resp) = handle_storage_metrics_http(ctx).await;
            respond_json(stream, status, &resp).await?;
        }
        ("PUT", "/config/storage") => {
            let update: StorageUpdate = serde_json::from_slice(&body)?;
            let version =
                match next_config_changed_version(&ctx.state_dir, "storage", update.version) {
                    Ok(version) => version,
                    Err(err) => {
                        let body = serde_json::json!({
                            "status": "error",
                            "error_code": "VERSION_MISMATCH",
                            "error_detail": err.to_string(),
                        })
                        .to_string();
                        respond_json(stream, 409, &body).await?;
                        return Ok(());
                    }
                };
            let storage_payload = serde_json::json!({ "path": update.path });
            let (status, resp) =
                handle_admin_command(ctx, client, "set_storage", storage_payload.clone(), None)
                    .await?;
            if status != 200 {
                respond_json(stream, status, &resp).await?;
                return Ok(());
            }

            if let Err(err) = broadcast_config_changed(
                client,
                "storage",
                None,
                None,
                version,
                storage_payload,
                None,
            )
            .await
            {
                tracing::warn!("config storage broadcast failed after set_storage: {err}");
            }
            tracing::info!("config storage update received");
            respond_json(stream, 200, &resp).await?;
        }
        ("POST", "/opa/policy") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::CompileApply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/compile") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Compile).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/apply") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Apply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/rollback") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Rollback).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/check") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Check).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/opa/policy") => {
            let target = normalize_opa_target(
                query
                    .get("hive")
                    .cloned()
                    .or_else(|| query.get("target").cloned()),
            );
            let (status, resp) = handle_opa_query(ctx, client, "get_policy", target).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/opa/status") => {
            let target = normalize_opa_target(
                query
                    .get("hive")
                    .cloned()
                    .or_else(|| query.get("target").cloned()),
            );
            let (status, resp) = handle_opa_query(ctx, client, "get_status", target).await?;
            respond_json(stream, status, &resp).await?;
        }
        _ => {
            let _ = headers;
            respond_json(stream, 404, r#"{"error":"not_found"}"#).await?;
        }
    }
    Ok(())
}

async fn handle_inventory_http(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    let target = format!("SY.orchestrator@{}", ctx.hive_id);
    let timeout_secs = env_timeout_secs("JSR_ADMIN_INVENTORY_TIMEOUT_SECS").unwrap_or(10);
    let response = send_system_request(
        client,
        &target,
        "INVENTORY_REQUEST",
        "INVENTORY_RESPONSE",
        payload,
        Duration::from_secs(timeout_secs),
    )
    .await;
    Ok(build_admin_http_response("inventory", response))
}

async fn handle_hive_paths(
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    body: &[u8],
    ctx: &AdminContext,
    client: &AdminRouterClient,
) -> Result<Option<(u16, String)>, AdminError> {
    let trimmed = path.trim_matches('/');
    let parts = trimmed.split('/').collect::<Vec<_>>();
    if parts.first().copied() != Some("hives") {
        return Ok(None);
    }
    if parts.len() == 1 {
        match method {
            "GET" => {
                let (status, resp) = handle_admin_query(ctx, client, "list_hives", None).await?;
                return Ok(Some((status, resp)));
            }
            "POST" => {
                let payload = if body.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_slice(body)?
                };
                let (status, resp) =
                    handle_admin_command(ctx, client, "add_hive", payload, None).await?;
                return Ok(Some((status, resp)));
            }
            _ => return Ok(None),
        }
    }
    let hive = parts[1].to_string();
    let rest = &parts[2..];
    if rest.is_empty() {
        return match method {
            "GET" => {
                let payload = serde_json::json!({ "hive_id": hive });
                let (status, resp) =
                    handle_admin_command(ctx, client, "get_hive", payload, None).await?;
                Ok(Some((status, resp)))
            }
            "DELETE" => {
                let payload = serde_json::json!({ "hive_id": hive });
                let (status, resp) =
                    handle_admin_command(ctx, client, "remove_hive", payload, None).await?;
                Ok(Some((status, resp)))
            }
            _ => Ok(None),
        };
    }
    match (method, rest) {
        ("GET", ["routes"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_routes", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["routes"]) => {
            let route: RouteConfig = serde_json::from_slice(body)?;
            let (status, resp) = handle_admin_command(
                ctx,
                client,
                "add_route",
                serde_json::to_value(route)?,
                Some(hive),
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["routes", prefix]) => {
            let payload = serde_json::json!({ "prefix": decode_percent(prefix) });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_route", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["vpns"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_vpns", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["vpns"]) => {
            let vpn: VpnConfig = serde_json::from_slice(body)?;
            let (status, resp) = handle_admin_command(
                ctx,
                client,
                "add_vpn",
                serde_json::to_value(vpn)?,
                Some(hive),
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["vpns", pattern]) => {
            let payload = serde_json::json!({ "pattern": decode_percent(pattern) });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_vpn", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_nodes", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_admin_command(ctx, client, "run_node", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["nodes", name]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if payload.get("node_name").is_none() {
                payload["node_name"] = serde_json::Value::String(decode_percent(name));
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "kill_node", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes", name, "config"]) => {
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_node_config", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("PUT", ["nodes", name, "config"]) => {
            let config_payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if !config_payload.is_object() {
                return Ok(Some((
                    400,
                    serde_json::json!({
                        "status": "error",
                        "action": "set_node_config",
                        "payload": serde_json::Value::Null,
                        "error_code": "INVALID_REQUEST",
                        "error_detail": "request body must be a JSON object",
                    })
                    .to_string(),
                )));
            }
            let replace = query
                .get("replace")
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let notify = query
                .get("notify")
                .map(|value| !(value == "0" || value.eq_ignore_ascii_case("false")))
                .unwrap_or(true);
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
                "config": config_payload,
                "replace": replace,
                "notify": notify,
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "set_node_config", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes", name, "state"]) => {
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_node_state", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes", name, "status"]) => {
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_node_status", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["versions"]) => {
            let (status, resp) =
                handle_admin_query(ctx, client, "get_versions", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["deployments"]) => {
            let payload = deployments_payload_from_query(query);
            let (status, resp) =
                handle_admin_command(ctx, client, "get_deployments", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["drift-alerts"]) => {
            let payload = drift_alerts_payload_from_query(query);
            let (status, resp) =
                handle_admin_command(ctx, client, "get_drift_alerts", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["update"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) = handle_hive_update_command(ctx, client, hive, payload).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["sync-hint"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) = handle_hive_sync_hint_command(ctx, client, hive, payload).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::CompileApply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "compile"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Compile).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "apply"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Apply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "rollback"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Rollback).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "check"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Check).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["opa", "policy"]) => {
            let (status, resp) = handle_opa_query(ctx, client, "get_policy", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["opa", "status"]) => {
            let (status, resp) = handle_opa_query(ctx, client, "get_status", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        _ => Ok(None),
    }
}

async fn handle_modules_paths(
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<Option<(u16, Vec<u8>, Option<String>)>, AdminError> {
    let trimmed = path.trim_matches('/');
    let parts = trimmed.split('/').collect::<Vec<_>>();
    if parts.first().copied() != Some("modules") {
        return Ok(None);
    }
    let storage_root = storage_root();
    let modules_root = storage_root.join("modules");
    match (method, parts.as_slice()) {
        ("GET", ["modules"]) => {
            let modules = list_dirs(&modules_root)?;
            let body = serde_json::json!({
                "status": "ok",
                "modules": modules,
            })
            .to_string()
            .into_bytes();
            Ok(Some((200, body, None)))
        }
        ("GET", ["modules", name]) => {
            let name = decode_percent(name);
            let versions = list_dirs(&modules_root.join(&name))?;
            let body = serde_json::json!({
                "status": "ok",
                "name": name,
                "versions": versions,
            })
            .to_string()
            .into_bytes();
            Ok(Some((200, body, None)))
        }
        ("GET", ["modules", name, version]) => {
            let name = decode_percent(name);
            let version = decode_percent(version);
            let module_path = modules_root.join(&name).join(&version);
            if !module_path.exists() {
                let body = serde_json::json!({
                    "status": "error",
                    "error_code": "NOT_FOUND",
                })
                .to_string()
                .into_bytes();
                return Ok(Some((404, body, None)));
            }
            if module_path.is_file() {
                let data = fs::read(&module_path)?;
                return Ok(Some((
                    200,
                    data,
                    Some("application/octet-stream".to_string()),
                )));
            }
            let mut builder = Builder::new(Vec::new());
            builder.append_dir_all(".", &module_path)?;
            let data = builder.into_inner()?;
            Ok(Some((200, data, Some("application/x-tar".to_string()))))
        }
        ("POST", ["modules", name, version]) => {
            let name = decode_percent(name);
            let version = decode_percent(version);
            let module_dir = modules_root.join(&name).join(&version);
            if module_dir.exists() {
                let body = serde_json::json!({
                    "status": "error",
                    "error_code": "MODULE_EXISTS",
                })
                .to_string()
                .into_bytes();
                return Ok(Some((409, body, None)));
            }
            fs::create_dir_all(&module_dir)?;
            let mut archive = Archive::new(Cursor::new(body));
            if let Err(err) = archive.unpack(&module_dir) {
                let _ = fs::remove_dir_all(&module_dir);
                let body = serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_ARCHIVE",
                    "error_detail": err.to_string(),
                })
                .to_string()
                .into_bytes();
                return Ok(Some((400, body, None)));
            }
            let body = serde_json::json!({
                "status": "ok",
                "name": name,
                "version": version,
            })
            .to_string()
            .into_bytes();
            Ok(Some((200, body, None)))
        }
        _ => Ok(None),
    }
}

fn decode_percent(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn storage_root() -> PathBuf {
    let default_root = json_router::paths::storage_root_dir();
    let hive_path = json_router::paths::config_dir().join("hive.yaml");
    let data = match fs::read_to_string(&hive_path) {
        Ok(data) => data,
        Err(_) => return default_root,
    };
    let parsed: HiveFile = match serde_yaml::from_str(&data) {
        Ok(parsed) => parsed,
        Err(_) => return default_root,
    };
    if let Some(path) = parsed.storage.and_then(|storage| storage.path) {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    default_root
}

fn list_dirs(path: &Path) -> Result<Vec<String>, AdminError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                entries.push(name.to_string());
            }
        }
    }
    entries.sort();
    Ok(entries)
}

async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<(String, String, HashMap<String, String>, Vec<u8>), AdminError> {
    let mut buf = Vec::new();
    let mut header_end = None;
    loop {
        let mut chunk = [0u8; 1024];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            header_end = Some(pos + 4);
            break;
        }
    }
    let header_end = header_end.ok_or("invalid http request")?;
    let header_str = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = header_str.split("\r\n");
    let request_line = lines.next().ok_or("missing request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("missing method")?.to_string();
    let path = parts.next().ok_or("missing path")?.to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; content_length - body.len()];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok((method, path, headers, body))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn split_path_query(path: &str) -> (&str, HashMap<String, String>) {
    if let Some((base, query)) = path.split_once('?') {
        (base, parse_query(query))
    } else {
        (path, HashMap::new())
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some(parts) => parts,
            None => (pair, ""),
        };
        params.insert(key.to_string(), value.to_string());
    }
    params
}

fn deployments_payload_from_query(query: &HashMap<String, String>) -> serde_json::Value {
    let mut payload = serde_json::json!({});
    if let Some(limit_raw) = query.get("limit") {
        if let Ok(limit) = limit_raw.trim().parse::<u64>() {
            payload["limit"] = serde_json::Value::Number(limit.into());
        }
    }
    if let Some(category) = query
        .get("category")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        payload["category"] = serde_json::Value::String(category.to_string());
    }
    payload
}

fn drift_alerts_payload_from_query(query: &HashMap<String, String>) -> serde_json::Value {
    let mut payload = deployments_payload_from_query(query);
    if let Some(severity) = query
        .get("severity")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        payload["severity"] = serde_json::Value::String(severity.to_string());
    }
    if let Some(kind) = query
        .get("kind")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        payload["kind"] = serde_json::Value::String(kind.to_string());
    }
    payload
}

fn load_node_uuid(dir: &Path, base_name: &str) -> Result<String, AdminError> {
    let path = dir.join(format!("{base_name}.uuid"));
    let data = fs::read_to_string(&path)?;
    let uuid = Uuid::parse_str(data.trim())?;
    Ok(uuid.to_string())
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee")
}

fn http_status_line(status: u16) -> &'static str {
    match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        404 => "HTTP/1.1 404 Not Found",
        409 => "HTTP/1.1 409 Conflict",
        422 => "HTTP/1.1 422 Unprocessable Entity",
        500 => "HTTP/1.1 500 Internal Server Error",
        501 => "HTTP/1.1 501 Not Implemented",
        502 => "HTTP/1.1 502 Bad Gateway",
        503 => "HTTP/1.1 503 Service Unavailable",
        504 => "HTTP/1.1 504 Gateway Timeout",
        _ => "HTTP/1.1 500 Internal Server Error",
    }
}

fn error_code_to_http_status(error_code: &str) -> u16 {
    let code = error_code.trim().to_ascii_uppercase();
    if code == "TIMEOUT" || code == "WAN_TIMEOUT" || code == "SSH_TIMEOUT" {
        return 504;
    }
    if code.starts_with("SSH_") {
        return 502;
    }
    match code.as_str() {
        "INVALID_REQUEST" | "INVALID_ADDRESS" | "INVALID_ARCHIVE" | "INVALID_HIVE_ID" => 400,
        "NOT_FOUND" | "RUNTIME_NOT_AVAILABLE" => 404,
        "HIVE_EXISTS" | "MODULE_EXISTS" | "VERSION_MISMATCH" | "WAN_NOT_AUTHORIZED" => 409,
        "COMPILE_ERROR" => 422,
        "NOT_IMPLEMENTED" => 501,
        "SERVICE_FAILED"
        | "SPAWN_FAILED"
        | "KILL_FAILED"
        | "REMOVE_FAILED"
        | "COPY_FAILED"
        | "CONFIG_FAILED"
        | "CONFIG_BROADCAST_FAILED"
        | "RUNTIME_ERROR"
        | "RUNTIME_COMMAND_FAILED"
        | "RUNTIME_UNAVAILABLE"
        | "TRANSPORT_ERROR" => 502,
        "SHM_NOT_FOUND" | "RUNTIME_MANIFEST_MISSING" | "MISSING_WAN_LISTEN" => 503,
        _ => 500,
    }
}

fn infer_transport_error_code(error_detail: &str) -> &'static str {
    let lower = error_detail.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        "TIMEOUT"
    } else {
        "TRANSPORT_ERROR"
    }
}

fn value_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

fn payload_error_code(payload: &serde_json::Value) -> Option<String> {
    value_string_field(payload, "error_code")
}

fn payload_error_detail(payload: &serde_json::Value) -> Option<String> {
    value_string_field(payload, "error_detail").or_else(|| value_string_field(payload, "message"))
}

async fn handle_storage_metrics_http(ctx: &AdminContext) -> (u16, String) {
    let action = "get_storage_metrics";
    let trace_id = Uuid::new_v4().to_string();
    let reply_subject = ctx.nats_client.inbox_reply_subject(&trace_id);
    let request_envelope = NatsRequestEnvelope::<serde_json::Value>::new(
        SUBJECT_STORAGE_METRICS_GET,
        trace_id.clone(),
        reply_subject.clone(),
        None,
    );
    let request_body = match serde_json::to_vec(&request_envelope) {
        Ok(body) => body,
        Err(err) => {
            let error_detail = err.to_string();
            return (
                500,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "status": "error",
                        "error_code": "INVALID_REQUEST",
                        "message": error_detail,
                    },
                    "error_code": "INVALID_REQUEST",
                    "error_detail": error_detail,
                })
                .to_string(),
            );
        }
    };
    let sid = 0u32;
    let request_started = Instant::now();
    tracing::info!(
        trace_id = %trace_id,
        endpoint = %ctx.nats_endpoint,
        request_subject = SUBJECT_STORAGE_METRICS_GET,
        reply_subject = %reply_subject,
        sid = sid,
        timeout_secs = STORAGE_METRICS_NATS_TIMEOUT_SECS,
        request_bytes = request_body.len(),
        "storage metrics nats request send"
    );
    let response_body = match ctx
        .nats_client
        .request_with_session_inbox(
            SUBJECT_STORAGE_METRICS_GET,
            &request_body,
            &reply_subject,
            Duration::from_secs(STORAGE_METRICS_NATS_TIMEOUT_SECS),
        )
        .await
    {
        Ok(body) => {
            let elapsed_ms = request_started.elapsed().as_millis() as u64;
            tracing::info!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = elapsed_ms,
                response_bytes = body.len(),
                "storage metrics nats response received"
            );
            body
        }
        Err(err) => {
            let error_detail = err.to_string();
            let elapsed_ms = request_started.elapsed().as_millis() as u64;
            tracing::warn!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = elapsed_ms,
                error = %error_detail,
                "storage metrics nats request failed"
            );
            return (
                503,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "status": "error",
                        "error_code": "STORAGE_METRICS_UNAVAILABLE",
                        "message": error_detail,
                    },
                    "error_code": "STORAGE_METRICS_UNAVAILABLE",
                    "error_detail": error_detail,
                })
                .to_string(),
            );
        }
    };
    let response: NatsResponseEnvelope<StorageInboxMetrics> =
        match serde_json::from_slice(&response_body) {
            Ok(resp) => resp,
            Err(err) => {
                let error_detail = err.to_string();
                let elapsed_ms = request_started.elapsed().as_millis() as u64;
                tracing::warn!(
                    trace_id = %trace_id,
                    endpoint = %ctx.nats_endpoint,
                    request_subject = SUBJECT_STORAGE_METRICS_GET,
                    reply_subject = %reply_subject,
                    sid = sid,
                    elapsed_ms = elapsed_ms,
                    error = %error_detail,
                    "storage metrics nats response decode failed"
                );
                return (
                    502,
                    serde_json::json!({
                        "status": "error",
                        "action": action,
                        "payload": {
                            "status": "error",
                            "error_code": "TRANSPORT_ERROR",
                            "message": error_detail,
                        },
                        "error_code": "TRANSPORT_ERROR",
                        "error_detail": error_detail,
                    })
                    .to_string(),
                );
            }
        };
    tracing::info!(
        trace_id = %trace_id,
        endpoint = %ctx.nats_endpoint,
        request_subject = SUBJECT_STORAGE_METRICS_GET,
        reply_subject = %reply_subject,
        sid = sid,
        elapsed_ms = request_started.elapsed().as_millis() as u64,
        response_status = %response.status,
        has_metrics = response.payload.is_some(),
        response_error_code = ?response.error_code,
        response_error_detail = ?response.error_detail,
        "storage metrics nats response decoded"
    );
    if response.action != SUBJECT_STORAGE_METRICS_GET {
        let error_detail = format!(
            "action mismatch expected={} got={}",
            SUBJECT_STORAGE_METRICS_GET, response.action
        );
        let elapsed_ms = request_started.elapsed().as_millis() as u64;
        tracing::warn!(
            trace_id = %trace_id,
            endpoint = %ctx.nats_endpoint,
            request_subject = SUBJECT_STORAGE_METRICS_GET,
            reply_subject = %reply_subject,
            sid = sid,
            elapsed_ms = elapsed_ms,
            error = %error_detail,
            "storage metrics nats response action mismatch"
        );
        return (
            502,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": {
                    "status": "error",
                    "error_code": "TRANSPORT_ERROR",
                    "message": error_detail,
                },
                "error_code": "TRANSPORT_ERROR",
                "error_detail": error_detail,
            })
            .to_string(),
        );
    }
    if response.trace_id != trace_id {
        let error_detail = format!(
            "trace_id mismatch expected={} got={}",
            trace_id, response.trace_id
        );
        let elapsed_ms = request_started.elapsed().as_millis() as u64;
        tracing::warn!(
            trace_id = %trace_id,
            endpoint = %ctx.nats_endpoint,
            request_subject = SUBJECT_STORAGE_METRICS_GET,
            reply_subject = %reply_subject,
            sid = sid,
            elapsed_ms = elapsed_ms,
            error = %error_detail,
            "storage metrics nats response trace mismatch"
        );
        return (
            502,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": {
                    "status": "error",
                    "error_code": "TRANSPORT_ERROR",
                    "message": error_detail,
                },
                "error_code": "TRANSPORT_ERROR",
                "error_detail": error_detail,
            })
            .to_string(),
        );
    }

    match (response.status.as_str(), response.payload) {
        ("ok", Some(metrics)) => {
            tracing::info!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                pending = metrics.pending,
                pending_with_error = metrics.pending_with_error,
                oldest_pending_age_s = metrics.oldest_pending_age_s,
                max_attempts = metrics.max_attempts,
                processed_total = metrics.processed_total,
                "storage metrics nats request success"
            );
            (
                200,
                serde_json::json!({
                    "status": "ok",
                    "action": action,
                    "payload": {
                        "status": "ok",
                        "metrics": metrics,
                    },
                    "error_code": serde_json::Value::Null,
                    "error_detail": serde_json::Value::Null,
                })
                .to_string(),
            )
        }
        _ => {
            let error_code = response
                .error_code
                .unwrap_or_else(|| "STORAGE_METRICS_UNAVAILABLE".to_string());
            let error_detail = response
                .error_detail
                .unwrap_or_else(|| "storage metrics request failed".to_string());
            tracing::warn!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                error_code = %error_code,
                error_detail = %error_detail,
                "storage metrics nats request returned error payload"
            );
            (
                503,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "status": "error",
                        "error_code": error_code,
                        "message": error_detail,
                    },
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            )
        }
    }
}

fn is_ok_status(status: Option<&str>) -> bool {
    matches!(status, Some(value) if value.eq_ignore_ascii_case("ok"))
}

async fn respond_json(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<(), AdminError> {
    let status_line = http_status_line(status);
    let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.as_bytes().len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn respond_bytes(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), AdminError> {
    let status_line = http_status_line(status);
    let header = format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

fn next_opa_version(requested: Option<u64>) -> Result<u64, AdminError> {
    let path = json_router::paths::opa_version_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = fs::read_to_string(&path)
        .ok()
        .and_then(|data| data.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = match requested {
        Some(version) => {
            if version <= current {
                return Err(
                    format!("version must be greater than current (current={current})").into(),
                );
            }
            version
        }
        None => current + 1,
    };
    fs::write(&path, next.to_string())?;
    Ok(next)
}

async fn handle_opa_http(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    mut req: OpaRequest,
    action: OpaAction,
) -> Result<(u16, String), AdminError> {
    let version = match action {
        OpaAction::Apply => {
            let Some(version) = req.version else {
                let resp = serde_json::json!({
                    "status": "error",
                    "error_code": "VERSION_MISMATCH",
                    "error_detail": "version_required"
                });
                return Ok((400, resp.to_string()));
            };
            version
        }
        OpaAction::Rollback => req.version.unwrap_or(0),
        _ => next_opa_version(req.version)?,
    };
    let entrypoint = req
        .entrypoint
        .clone()
        .unwrap_or_else(|| "router/target".to_string());
    let target = normalize_opa_target(req.hive.take());

    if action.needs_rego() && req.rego.as_deref().unwrap_or("").is_empty() {
        let resp = serde_json::json!({
            "status": "error",
            "error_code": "COMPILE_ERROR",
            "error_detail": "rego_missing"
        });
        return Ok((400, resp.to_string()));
    }

    let mut responses = Vec::new();
    if matches!(
        action,
        OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check
    ) {
        responses = send_opa_action(
            ctx,
            client,
            "compile",
            version,
            req.rego.clone(),
            Some(entrypoint.clone()),
            action.auto_apply_flag(),
            target.clone(),
        )
        .await?;
        if responses.is_empty() || responses.iter().any(|r| r.status != "ok") {
            return Ok(build_opa_http_response(
                ctx, action, version, responses, target,
            ));
        }
    }

    if action.apply_after() {
        let apply_responses = send_opa_action(
            ctx,
            client,
            "apply",
            version,
            None,
            None,
            None,
            target.clone(),
        )
        .await?;
        if apply_responses.is_empty() || apply_responses.iter().any(|r| r.status != "ok") {
            let mut combined = responses;
            combined.extend(apply_responses);
            return Ok(build_opa_http_response(
                ctx, action, version, combined, target,
            ));
        }
        responses.extend(apply_responses);
    } else if matches!(action, OpaAction::Apply | OpaAction::Rollback) {
        let apply_action = action.as_str();
        responses = send_opa_action(
            ctx,
            client,
            apply_action,
            version,
            None,
            None,
            None,
            target.clone(),
        )
        .await?;
    }

    Ok(build_opa_http_response(
        ctx, action, version, responses, target,
    ))
}

async fn handle_opa_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target: Option<String>,
) -> Result<(u16, String), AdminError> {
    let responses = send_opa_query(ctx, client, action, target.clone()).await?;
    Ok(build_opa_query_response(ctx, action, responses, target))
}

async fn handle_admin_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    if action == "list_admin_actions" {
        return Ok(build_admin_actions_catalog_response());
    }
    let payload = normalize_admin_payload(action, serde_json::json!({}), hive.as_deref());
    let request = build_admin_request(ctx, action, payload, hive);
    let response = send_admin_request(client, request, admin_action_timeout(action)).await;
    Ok(build_admin_http_response(action, response))
}

fn build_admin_actions_catalog_response() -> (u16, String) {
    let actions: Vec<serde_json::Value> = INTERNAL_ACTION_REGISTRY
        .iter()
        .map(|spec| {
            let (handler, canonical_action) = match spec.route {
                InternalActionRoute::Query(canonical) => ("query", Some(canonical)),
                InternalActionRoute::Command(canonical) => ("command", Some(canonical)),
                InternalActionRoute::Update => ("update", None),
                InternalActionRoute::SyncHint => ("sync_hint", None),
                InternalActionRoute::Inventory => ("inventory", None),
                InternalActionRoute::OpaHttp(_) => ("opa_http", None),
                InternalActionRoute::OpaQuery(canonical) => ("opa_query", Some(canonical)),
            };
            serde_json::json!({
                "action": spec.action,
                "handler": handler,
                "canonical_action": canonical_action,
                "requires_target": spec.requires_target,
                "allow_legacy_hive_id": spec.allow_legacy_hive_id,
            })
        })
        .collect();
    (
        200,
        serde_json::json!({
            "status": "ok",
            "action": "list_admin_actions",
            "payload": {
                "registry_version": INTERNAL_ACTION_REGISTRY_VERSION,
                "actions": actions,
            },
            "error_code": serde_json::Value::Null,
            "error_detail": serde_json::Value::Null,
        })
        .to_string(),
    )
}

async fn handle_admin_command(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    if let Some(detail) = admin_payload_contract_error(action, &payload) {
        return Ok((
            400u16,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": detail,
            })
            .to_string(),
        ));
    }

    let payload = normalize_admin_payload(action, payload, hive.as_deref());
    let request = build_admin_request(ctx, action, payload, hive);
    let target_hive = extract_hive_from_target(&request.target);
    let mut response = send_admin_request(client, request, admin_action_timeout(action)).await;
    if let Ok(ref payload) = response {
        if let Some(status) = payload.get("status").and_then(|v| v.as_str()) {
            if status == "ok" {
                if matches!(
                    action,
                    "add_route" | "delete_route" | "add_vpn" | "delete_vpn"
                ) {
                    if let Err(err) =
                        broadcast_full_config(ctx, client, action, target_hive.as_deref()).await
                    {
                        let detail = format!("action applied but broadcast failed: {err}");
                        tracing::warn!(error = %detail, "broadcast after admin action failed");
                        response = Ok(serde_json::json!({
                            "status": "error",
                            "error_code": "CONFIG_BROADCAST_FAILED",
                            "message": detail,
                        }));
                    }
                }
            }
        }
    }
    Ok(build_admin_http_response(action, response))
}

async fn handle_hive_update_command(
    _ctx: &AdminContext,
    client: &AdminRouterClient,
    hive_id: String,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    let invalid_request = |detail: &str| {
        (
            400u16,
            serde_json::json!({
                "status": "error",
                "action": "update",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": detail,
            })
            .to_string(),
        )
    };

    let category = match payload.get("category") {
        None => "runtime".to_string(),
        Some(value) => match value
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
        {
            Some(category) if matches!(category.as_str(), "runtime" | "core" | "vendor") => {
                category
            }
            _ => {
                return Ok(invalid_request(
                    "category must be one of runtime/core/vendor",
                ))
            }
        },
    };

    let manifest_version = match payload.get("manifest_version") {
        None => 0,
        Some(value) => match value.as_u64() {
            Some(v) => v,
            None => {
                return Ok(invalid_request(
                    "manifest_version must be an unsigned integer",
                ))
            }
        },
    };
    if payload.get("version").is_some() {
        return Ok(invalid_request(
            "legacy field 'version' is not supported; use manifest_version",
        ));
    }

    let manifest_hash = match payload
        .get("manifest_hash")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(value) => value,
        None => return Ok(invalid_request("missing manifest_hash")),
    };
    if payload.get("hash").is_some() {
        return Ok(invalid_request(
            "legacy field 'hash' is not supported; use manifest_hash",
        ));
    }

    let target = format!("SY.orchestrator@{}", hive_id);
    let system_payload = serde_json::json!({
        "category": category,
        "manifest_version": manifest_version,
        "manifest_hash": manifest_hash,
    });

    let response = send_system_request(
        client,
        &target,
        "SYSTEM_UPDATE",
        "SYSTEM_UPDATE_RESPONSE",
        system_payload,
        admin_action_timeout("update"),
    )
    .await;

    match response {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let top_status = match status {
                "ok" => "ok",
                "sync_pending" => "sync_pending",
                _ => "error",
            };
            let error_code_opt = if matches!(status, "ok" | "sync_pending") {
                None
            } else {
                Some(payload_error_code(&payload).unwrap_or_else(|| "SERVICE_FAILED".into()))
            };
            let http_status = match status {
                "ok" => 200,
                "sync_pending" => 202,
                _ => {
                    error_code_to_http_status(error_code_opt.as_deref().unwrap_or("SERVICE_FAILED"))
                }
            };
            let error_code = if let Some(code) = error_code_opt {
                serde_json::json!(code)
            } else {
                serde_json::Value::Null
            };
            let error_detail = if status == "ok" || status == "sync_pending" {
                serde_json::Value::Null
            } else {
                serde_json::json!(payload_error_detail(&payload))
            };
            Ok((
                http_status,
                serde_json::json!({
                    "status": top_status,
                    "action": "update",
                    "payload": payload,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            Ok((
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": "update",
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
    }
}

async fn handle_hive_sync_hint_command(
    _ctx: &AdminContext,
    client: &AdminRouterClient,
    hive_id: String,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    let invalid_request = |detail: &str| {
        (
            400u16,
            serde_json::json!({
                "status": "error",
                "action": "sync_hint",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": detail,
            })
            .to_string(),
        )
    };

    let channel = match payload.get("channel") {
        None => "blob".to_string(),
        Some(value) => match value
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
        {
            Some(v) if matches!(v.as_str(), "blob" | "dist") => v,
            _ => return Ok(invalid_request("channel must be one of blob/dist")),
        },
    };

    let folder_id = match payload.get("folder_id") {
        None => match channel.as_str() {
            "blob" => "fluxbee-blob".to_string(),
            "dist" => "fluxbee-dist".to_string(),
            _ => unreachable!(),
        },
        Some(value) => match value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) => v.to_string(),
            None => return Ok(invalid_request("folder_id must be a non-empty string")),
        },
    };

    let wait_for_idle = match payload.get("wait_for_idle") {
        None => true,
        Some(value) => match value.as_bool() {
            Some(v) => v,
            None => return Ok(invalid_request("wait_for_idle must be boolean")),
        },
    };

    let timeout_ms = match payload.get("timeout_ms") {
        None => 30_000u64,
        Some(value) => match value.as_u64() {
            Some(v) if v > 0 => v,
            _ => {
                return Ok(invalid_request(
                    "timeout_ms must be a positive unsigned integer",
                ))
            }
        },
    };

    let target = format!("SY.orchestrator@{}", hive_id);
    let system_payload = serde_json::json!({
        "channel": channel,
        "folder_id": folder_id,
        "wait_for_idle": wait_for_idle,
        "timeout_ms": timeout_ms,
    });
    let response = send_system_request(
        client,
        &target,
        "SYSTEM_SYNC_HINT",
        "SYSTEM_SYNC_HINT_RESPONSE",
        system_payload,
        admin_action_timeout("sync_hint"),
    )
    .await;

    match response {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let top_status = match status {
                "ok" => "ok",
                "sync_pending" => "sync_pending",
                _ => "error",
            };
            let error_code_opt = if matches!(status, "ok" | "sync_pending") {
                None
            } else {
                Some(payload_error_code(&payload).unwrap_or_else(|| "SERVICE_FAILED".into()))
            };
            let http_status = match status {
                "ok" => 200,
                "sync_pending" => 202,
                _ => {
                    error_code_to_http_status(error_code_opt.as_deref().unwrap_or("SERVICE_FAILED"))
                }
            };
            let error_code = if let Some(code) = error_code_opt {
                serde_json::json!(code)
            } else {
                serde_json::Value::Null
            };
            let error_detail = if matches!(status, "ok" | "sync_pending") {
                serde_json::Value::Null
            } else {
                serde_json::json!(payload_error_detail(&payload))
            };
            Ok((
                http_status,
                serde_json::json!({
                    "status": top_status,
                    "action": "sync_hint",
                    "payload": payload,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            Ok((
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": "sync_hint",
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
    }
}

fn build_admin_request(
    ctx: &AdminContext,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> AdminRequest {
    let requested_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    let base =
        match action {
            "list_routes" | "add_route" | "delete_route" | "list_vpns" | "add_vpn"
            | "delete_vpn" => "SY.config.routes",
            "list_nodes" | "run_node" | "kill_node" | "hive_status" | "get_storage"
            | "set_storage" | "list_hives" | "get_hive" | "list_versions" | "get_versions"
            | "list_deployments" | "get_deployments" | "list_drift_alerts" | "get_drift_alerts"
            | "get_node_config" | "set_node_config" | "get_node_state" | "get_node_status"
            | "remove_hive" | "add_hive" => "SY.orchestrator",
            _ => "SY.config.routes",
        };
    let route_hive = if action_routes_via_local_orchestrator(action) {
        ctx.hive_id.clone()
    } else {
        requested_hive.clone()
    };
    let target = if route_hive.contains('@') {
        route_hive
    } else {
        format!("{}@{}", base, route_hive)
    };
    let unicast = if target.ends_with(&format!("@{}", ctx.hive_id)) {
        load_node_uuid(&ctx.state_dir.join("nodes"), base).ok()
    } else {
        None
    };
    AdminRequest {
        action: action.to_string(),
        payload,
        target,
        unicast,
    }
}

async fn send_admin_request(
    client: &AdminRouterClient,
    request: AdminRequest,
    timeout_window: Duration,
) -> Result<serde_json::Value, AdminError> {
    let dst = request
        .unicast
        .clone()
        .unwrap_or_else(|| request.target.clone());
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: client.sender.uuid().to_string(),
            dst: Destination::Unicast(dst),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: None,
            src_ilk: None,
            scope: None,
            target: Some(request.target.clone()),
            action: Some(request.action.clone()),
            priority: None,
            context: None,
        },
        payload: request.payload,
    };
    let (tx, rx) = oneshot::channel::<Message>();
    client.enqueue_admin_waiter(trace_id.clone(), tx).await;
    client.sender.send(msg).await?;

    use tokio::time::{timeout, Instant};
    let deadline = Instant::now() + timeout_window;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let msg = match timeout(remaining, rx).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(_)) => return Err("admin response channel closed".into()),
        Err(_) => {
            client.drop_admin_waiter(&trace_id).await;
            return Err(format!(
                "admin request timeout action={} timeout_secs={}",
                request.action,
                timeout_window.as_secs()
            )
            .into());
        }
    };
    Ok(msg.payload)
}

async fn send_system_request(
    client: &AdminRouterClient,
    target: &str,
    request_msg: &str,
    expected_response_msg: &str,
    payload: serde_json::Value,
    timeout_window: Duration,
) -> Result<serde_json::Value, AdminError> {
    use tokio::time::timeout;

    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: client.sender.uuid().to_string(),
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(request_msg.to_string()),
            src_ilk: None,
            scope: None,
            target: Some(target.to_string()),
            action: None,
            priority: None,
            context: None,
        },
        payload,
    };

    let (tx, rx) = oneshot::channel::<Message>();
    client.enqueue_admin_waiter(trace_id.clone(), tx).await;
    client.sender.send(msg).await?;

    let msg = match timeout(timeout_window, rx).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(_)) => return Err("system response channel closed".into()),
        Err(_) => {
            client.drop_admin_waiter(&trace_id).await;
            return Err(format!(
                "system request timeout msg={} target={} timeout_secs={}",
                request_msg,
                target,
                timeout_window.as_secs()
            )
            .into());
        }
    };

    if msg.meta.msg_type != SYSTEM_KIND {
        return Err(format!(
            "invalid response type: expected={} got={}",
            SYSTEM_KIND, msg.meta.msg_type
        )
        .into());
    }
    if msg.meta.msg.as_deref() != Some(expected_response_msg) {
        return Err(format!(
            "invalid response msg: expected={} got={}",
            expected_response_msg,
            msg.meta.msg.as_deref().unwrap_or("")
        )
        .into());
    }
    Ok(msg.payload)
}

fn env_timeout_secs(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse::<u64>().ok()
}

fn admin_action_timeout(action: &str) -> Duration {
    match action {
        // add_hive runs remote bootstrap + WAN wait and is expected to be long.
        "add_hive" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS").unwrap_or(180))
        }
        "update" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_UPDATE_TIMEOUT_SECS").unwrap_or(60))
        }
        "sync_hint" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_SYNC_HINT_TIMEOUT_SECS").unwrap_or(45))
        }
        // other orchestrator mutating actions can also take longer than default.
        "run_node" | "kill_node" | "remove_hive" | "set_node_config" | "get_node_config"
        | "get_node_state" | "get_node_status" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_ORCH_TIMEOUT_SECS").unwrap_or(30))
        }
        _ => Duration::from_secs(env_timeout_secs("JSR_ADMIN_TIMEOUT_SECS").unwrap_or(5)),
    }
}

fn build_admin_http_response(
    action: &str,
    response: Result<serde_json::Value, AdminError>,
) -> (u16, String) {
    match response {
        Ok(payload) => {
            let status = payload.get("status").and_then(|v| v.as_str());
            if is_ok_status(status) {
                return (
                    200,
                    serde_json::json!({
                        "status": "ok",
                        "action": action,
                        "payload": payload,
                        "error_code": serde_json::Value::Null,
                        "error_detail": serde_json::Value::Null,
                    })
                    .to_string(),
                );
            }

            let error_code =
                payload_error_code(&payload).unwrap_or_else(|| "SERVICE_FAILED".into());
            let error_detail = payload_error_detail(&payload);
            let status_code = error_code_to_http_status(&error_code);
            (
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": payload,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            )
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            (
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            )
        }
    }
}

fn extract_hive_from_target(target: &str) -> Option<String> {
    target.split_once('@').map(|(_, hive)| hive.to_string())
}

fn action_routes_via_local_orchestrator(action: &str) -> bool {
    matches!(
        action,
        "list_nodes"
            | "run_node"
            | "kill_node"
            | "hive_status"
            | "get_storage"
            | "set_storage"
            | "list_hives"
            | "get_hive"
            | "list_versions"
            | "get_versions"
            | "list_deployments"
            | "get_deployments"
            | "list_drift_alerts"
            | "get_drift_alerts"
            | "get_node_config"
            | "set_node_config"
            | "get_node_state"
            | "get_node_status"
            | "remove_hive"
            | "add_hive"
    )
}

fn normalize_admin_payload(
    action: &str,
    mut payload: serde_json::Value,
    hive: Option<&str>,
) -> serde_json::Value {
    if !action_routes_via_local_orchestrator(action) {
        return payload;
    }

    if let Some(node_hive) = node_name_hive_from_payload(action, &payload) {
        if let Some(requested_target) = payload
            .get("target")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if requested_target != node_hive {
                tracing::warn!(
                    action = action,
                    node_hive = %node_hive,
                    requested_target = %requested_target,
                    "node_name@hive overrides payload.target"
                );
            }
        }
        payload["target"] = serde_json::Value::String(node_hive.to_string());
        return payload;
    }

    if let Some(hive_id) = hive
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        if payload.get("target").is_none() {
            payload["target"] = serde_json::Value::String(hive_id.to_string());
        }
    }

    payload
}

fn node_name_hive_from_payload<'a>(
    action: &str,
    payload: &'a serde_json::Value,
) -> Option<&'a str> {
    if !is_node_scoped_action(action) {
        return None;
    }
    let node_name = payload
        .get("node_name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let (_, hive) = node_name.rsplit_once('@')?;
    let hive = hive.trim();
    if hive.is_empty() {
        return None;
    }
    Some(hive)
}

fn is_node_scoped_action(action: &str) -> bool {
    matches!(
        action,
        "run_node"
            | "kill_node"
            | "get_node_config"
            | "set_node_config"
            | "get_node_state"
            | "get_node_status"
    )
}

fn admin_payload_contract_error(action: &str, payload: &serde_json::Value) -> Option<String> {
    let legacy = match action {
        "run_node" => {
            if payload.get("name").is_some() {
                Some(("name", "node_name"))
            } else if payload.get("version").is_some() {
                Some(("version", "runtime_version"))
            } else {
                None
            }
        }
        "kill_node" => payload.get("name").map(|_| ("name", "node_name")),
        _ => None,
    };
    legacy.map(|(field, canonical)| {
        format!(
            "legacy field '{}' is not supported; use '{}'",
            field, canonical
        )
    })
}

async fn broadcast_full_config(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target_hive: Option<&str>,
) -> Result<(), AdminError> {
    let list_action = if action.contains("route") {
        "list_routes"
    } else {
        "list_vpns"
    };
    let list_req = build_admin_request(
        ctx,
        list_action,
        serde_json::json!({}),
        target_hive.map(|s| s.to_string()),
    );
    let response = send_admin_request(client, list_req, admin_action_timeout(list_action)).await?;
    let payload = response
        .get("status")
        .and_then(|v| v.as_str())
        .filter(|v| *v == "ok")
        .and_then(|_| response.get("routes").or_else(|| response.get("vpns")))
        .ok_or("list response missing routes/vpns")?;
    let config = if list_action == "list_routes" {
        serde_json::json!({ "routes": payload })
    } else {
        serde_json::json!({ "vpns": payload })
    };

    let subsystem = if list_action == "list_routes" {
        "routes"
    } else {
        "vpn"
    };
    let version = next_config_changed_version(&ctx.state_dir, subsystem, None)?;
    broadcast_config_changed(client, subsystem, None, None, version, config, None).await?;
    Ok(())
}

async fn send_opa_action(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    version: u64,
    rego: Option<String>,
    entrypoint: Option<String>,
    auto_apply: Option<bool>,
    target: Option<String>,
) -> Result<Vec<OpaResponseEntry>, AdminError> {
    let mut cfg = serde_json::json!({});
    if let Some(rego) = rego {
        cfg = serde_json::json!({
            "rego": rego,
            "entrypoint": entrypoint.unwrap_or_else(|| "router/target".to_string()),
        });
    }
    broadcast_config_changed(
        client,
        "opa",
        Some(action.to_string()),
        auto_apply,
        version,
        cfg,
        target.clone(),
    )
    .await?;

    let expected = expected_hive_sets(ctx, target.as_deref()).effective;
    let mut receiver = client.subscribe_system();
    let responses = collect_opa_responses(&mut receiver, action, version, &expected).await;
    Ok(responses)
}

async fn send_opa_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target: Option<String>,
) -> Result<Vec<OpaQueryEntry>, AdminError> {
    let target_pattern = target
        .as_deref()
        .map(|hive| format!("SY.opa.rules@{}", hive));
    let dst = match target.as_deref() {
        Some(hive) => Destination::Unicast(format!("SY.opa.rules@{}", hive)),
        None => Destination::Broadcast,
    };
    let msg = Message {
        routing: Routing {
            src: client.sender.uuid().to_string(),
            dst,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: "query".to_string(),
            action: Some(action.to_string()),
            msg: None,
            src_ilk: None,
            scope: None,
            target: target_pattern,
            priority: None,
            context: None,
        },
        payload: serde_json::json!({}),
    };
    client.sender.send(msg).await?;

    let expected = expected_hive_sets(ctx, target.as_deref()).effective;
    let mut receiver = client.subscribe_query();
    let responses = collect_opa_query_responses(&mut receiver, action, &expected).await;
    Ok(responses)
}

fn normalize_opa_target(target: Option<String>) -> Option<String> {
    target.and_then(|value| {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("broadcast") {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn remote_hive_name(entry: &RemoteHiveEntry) -> Option<String> {
    if entry.hive_id_len == 0 {
        return None;
    }
    let len = entry.hive_id_len as usize;
    Some(String::from_utf8_lossy(&entry.hive_id[..len]).into_owned())
}

fn remote_hive_is_stale(entry: &RemoteHiveEntry, now: u64) -> bool {
    (entry.flags & (FLAG_DELETED | FLAG_STALE)) != 0
        || now.saturating_sub(entry.last_updated) > HEARTBEAT_STALE_MS
}

fn topology_hives_from_lsa(ctx: &AdminContext) -> Vec<String> {
    let mut hives = HashSet::new();
    hives.insert(ctx.hive_id.clone());

    let shm_name = format!("/jsr-lsa-{}", ctx.hive_id);
    let reader = match LsaRegionReader::open_read_only(&shm_name) {
        Ok(reader) => reader,
        Err(err) => {
            tracing::warn!(error = %err, "failed to open lsa region for OPA expected hives");
            let mut fallback: Vec<String> = hives.into_iter().collect();
            fallback.sort();
            return fallback;
        }
    };
    let snapshot = match reader.read_snapshot() {
        Some(snapshot) => snapshot,
        None => {
            tracing::warn!("lsa snapshot unavailable for OPA expected hives");
            let mut fallback: Vec<String> = hives.into_iter().collect();
            fallback.sort();
            return fallback;
        }
    };
    let now = now_epoch_ms();
    for entry in &snapshot.hives {
        if remote_hive_is_stale(entry, now) {
            continue;
        }
        if let Some(hive) = remote_hive_name(entry).filter(|name| !name.trim().is_empty()) {
            hives.insert(hive);
        }
    }
    let mut out: Vec<String> = hives.into_iter().collect();
    out.sort();
    out
}

#[derive(Debug, Clone)]
struct OpaExpectedHives {
    effective: Vec<String>,
    topology: Vec<String>,
    policy: Vec<String>,
}

fn expected_hive_sets(ctx: &AdminContext, target: Option<&str>) -> OpaExpectedHives {
    if let Some(target) = target {
        let single = vec![target.to_string()];
        return OpaExpectedHives {
            effective: single.clone(),
            topology: single.clone(),
            policy: single,
        };
    }

    let topology = topology_hives_from_lsa(ctx);
    let mut policy_set = HashSet::new();
    policy_set.insert(ctx.hive_id.clone());
    for hive in &ctx.authorized_hives {
        if !hive.trim().is_empty() {
            policy_set.insert(hive.clone());
        }
    }
    let mut policy: Vec<String> = policy_set.into_iter().collect();
    policy.sort();

    // Topology (LSA/SHM) is canonical for fanout expectations.
    // Policy list is kept for explicit observability in response payload.
    let effective = topology.clone();
    OpaExpectedHives {
        effective,
        topology,
        policy,
    }
}

async fn collect_opa_responses(
    receiver: &mut broadcast::Receiver<Message>,
    action: &str,
    version: u64,
    expected: &[String],
) -> Vec<OpaResponseEntry> {
    use tokio::time::{timeout, Instant};

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut responses: HashMap<String, OpaResponseEntry> = HashMap::new();

    while responses.len() < expected.len() && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = match timeout(remaining, receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(err)) => {
                tracing::warn!("opa response recv error: {err}");
                break;
            }
            Err(_) => break,
        };
        if msg.meta.msg.as_deref() != Some("CONFIG_RESPONSE") {
            continue;
        }
        let payload: ConfigResponsePayload = match serde_json::from_value(msg.payload) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!("invalid config response payload: {err}");
                continue;
            }
        };
        if payload.subsystem != "opa" {
            continue;
        }
        if payload.action.as_deref() != Some(action) {
            continue;
        }
        if payload.version.unwrap_or(version) != version {
            continue;
        }
        let hive = payload
            .hive
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let entry = OpaResponseEntry {
            hive: hive.clone(),
            status: payload.status.unwrap_or_else(|| "error".to_string()),
            version: payload.version,
            compile_time_ms: payload.compile_time_ms,
            wasm_size_bytes: payload.wasm_size_bytes,
            hash: payload.hash,
            error_code: payload.error_code,
            error_detail: payload.error_detail,
        };
        responses.insert(hive, entry);
    }

    responses.into_values().collect()
}

async fn collect_opa_query_responses(
    receiver: &mut broadcast::Receiver<Message>,
    action: &str,
    expected: &[String],
) -> Vec<OpaQueryEntry> {
    use tokio::time::{timeout, Instant};

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut responses: HashMap<String, OpaQueryEntry> = HashMap::new();

    while responses.len() < expected.len() && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = match timeout(remaining, receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(err)) => {
                tracing::warn!("opa query recv error: {err}");
                break;
            }
            Err(_) => break,
        };
        if msg.meta.msg_type != "query_response" {
            continue;
        }
        if msg.meta.action.as_deref() != Some(action) {
            continue;
        }
        let payload = msg.payload;
        let hive = payload
            .get("hive")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let status = payload
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("ok")
            .to_string();
        responses.insert(
            hive.clone(),
            OpaQueryEntry {
                hive,
                status,
                payload,
            },
        );
    }

    responses.into_values().collect()
}

fn build_opa_http_response(
    ctx: &AdminContext,
    action: OpaAction,
    version: u64,
    responses: Vec<OpaResponseEntry>,
    target: Option<String>,
) -> (u16, String) {
    let expected = expected_hive_sets(ctx, target.as_deref());
    let pending: Vec<String> = expected
        .effective
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_topology: Vec<String> = expected
        .topology
        .iter()
        .into_iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_policy: Vec<String> = expected
        .policy
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let timed_out = !pending.is_empty();

    let (error_code, error_detail) = if timed_out {
        (
            Some("TIMEOUT".to_string()),
            Some(format!("pending_hives={}", pending.join(","))),
        )
    } else if responses.is_empty() {
        (
            Some("SERVICE_FAILED".to_string()),
            Some("no responses received".to_string()),
        )
    } else {
        match responses
            .iter()
            .find(|r| !r.status.eq_ignore_ascii_case("ok"))
        {
            Some(entry) => (
                entry
                    .error_code
                    .clone()
                    .or_else(|| Some("SERVICE_FAILED".to_string())),
                entry.error_detail.clone(),
            ),
            None => (None, None),
        }
    };

    let status = if let Some(code) = error_code.as_deref() {
        error_code_to_http_status(code)
    } else {
        200
    };

    let body = serde_json::json!({
        "status": if status == 200 { "ok" } else { "error" },
        "version": version,
        "action": action.as_str(),
        "check_only": action.check_only(),
        "responses": responses,
        "pending": pending,
        "expected_hives_topology": expected.topology,
        "expected_hives_policy": expected.policy,
        "pending_hives_topology": pending_topology,
        "pending_hives_policy": pending_policy,
        "error_code": error_code,
        "error_detail": error_detail,
    })
    .to_string();
    (status, body)
}

fn build_opa_query_response(
    ctx: &AdminContext,
    action: &str,
    responses: Vec<OpaQueryEntry>,
    target: Option<String>,
) -> (u16, String) {
    let expected = expected_hive_sets(ctx, target.as_deref());
    let pending: Vec<String> = expected
        .effective
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_topology: Vec<String> = expected
        .topology
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_policy: Vec<String> = expected
        .policy
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let timed_out = !pending.is_empty();

    let (error_code, error_detail) = if timed_out {
        (
            Some("TIMEOUT".to_string()),
            Some(format!("pending_hives={}", pending.join(","))),
        )
    } else if responses.is_empty() {
        (
            Some("SERVICE_FAILED".to_string()),
            Some("no responses received".to_string()),
        )
    } else {
        match responses
            .iter()
            .find(|r| !r.status.eq_ignore_ascii_case("ok"))
        {
            Some(entry) => (
                payload_error_code(&entry.payload).or_else(|| Some("SERVICE_FAILED".to_string())),
                payload_error_detail(&entry.payload),
            ),
            None => (None, None),
        }
    };

    let status = if let Some(code) = error_code.as_deref() {
        error_code_to_http_status(code)
    } else {
        200
    };

    let body = serde_json::json!({
        "status": if status == 200 { "ok" } else { "error" },
        "action": action,
        "responses": responses,
        "pending": pending,
        "expected_hives_topology": expected.topology,
        "expected_hives_policy": expected.policy,
        "pending_hives_topology": pending_topology,
        "pending_hives_policy": pending_policy,
        "error_code": error_code,
        "error_detail": error_detail,
    })
    .to_string();
    (status, body)
}
