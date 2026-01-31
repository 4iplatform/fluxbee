use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use std::future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use jsr_client::{connect, NodeConfig, NodeReceiver, NodeSender};
use jsr_client::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, SCOPE_GLOBAL,
    SYSTEM_KIND,
};

type AdminError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
    role: Option<String>,
    admin: Option<AdminSection>,
    wan: Option<WanSection>,
}

#[derive(Debug, Deserialize)]
struct AdminSection {
    listen: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    authorized_islands: Option<Vec<String>>,
}

#[derive(Clone)]
struct AdminContext {
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    island_id: String,
    authorized_islands: Vec<String>,
}

struct AdminRouterClient {
    sender: NodeSender,
    pending_admin: Mutex<HashMap<String, oneshot::Sender<Message>>>,
    system_tx: broadcast::Sender<Message>,
    query_tx: broadcast::Sender<Message>,
}

impl AdminRouterClient {
    fn new(sender: NodeSender) -> Self {
        let (system_tx, _) = broadcast::channel(256);
        let (query_tx, _) = broadcast::channel(256);
        Self {
            sender,
            pending_admin: Mutex::new(HashMap::new()),
            system_tx,
            query_tx,
        }
    }

    async fn connect_with_retry(
        config: NodeConfig,
        delay: Duration,
    ) -> Result<Arc<Self>, jsr_client::NodeError> {
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
        if msg.meta.msg_type == "admin" {
            let mut pending = self.pending_admin.lock().await;
            if let Some(tx) = pending.remove(&msg.routing.trace_id) {
                let _ = tx.send(msg);
            }
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
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct RouteConfig {
    prefix: String,
    #[serde(default)]
    match_kind: Option<String>,
    action: String,
    #[serde(default)]
    next_hop_island: Option<String>,
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

    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);

    let island = load_island(&config_dir)?;
    let island_id = island.island_id.clone();
    let authorized_islands = island
        .wan
        .as_ref()
        .and_then(|wan| wan.authorized_islands.clone())
        .unwrap_or_default();
    if island.role.as_deref() != Some("mother") {
        tracing::warn!("SY.admin solo corre en mother island; role != mother");
        return Ok(());
    }
    let admin_listen = island
        .admin
        .and_then(|admin| admin.listen)
        .unwrap_or_else(|| "0.0.0.0:8080".to_string());

    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: socket_dir.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let router_client = AdminRouterClient::connect_with_retry(node_config, Duration::from_secs(1))
        .await
        .map_err(|err| {
            tracing::error!("router client connect failed: {err}");
            err
        })?;

    let (broadcast_tx, broadcast_rx) = mpsc::unbounded_channel::<BroadcastRequest>();
    let http_tx = broadcast_tx.clone();
    let http_ctx = AdminContext {
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        socket_dir: socket_dir.clone(),
        island_id,
        authorized_islands,
    };
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

    future::pending::<()>().await;
    Ok(())
}

enum BroadcastRequest {
    Routes(Vec<RouteConfig>),
    Vpns(Vec<VpnConfig>),
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
    island: Option<String>,
}

async fn run_broadcast_loop(
    mut rx: mpsc::UnboundedReceiver<BroadcastRequest>,
    client: Arc<AdminRouterClient>,
) {
    while let Some(req) = rx.recv().await {
        let failed = match req {
            BroadcastRequest::Routes(routes) => {
                match broadcast_config_changed(
                    &client,
                    "routes",
                    None,
                    None,
                    0,
                    serde_json::json!({ "routes": routes }),
                    None,
                )
                .await
                {
                    Ok(()) => false,
                    Err(err) => {
                        tracing::warn!(error = %err, "broadcast failed");
                        true
                    }
                }
            }
            BroadcastRequest::Vpns(vpns) => {
                match broadcast_config_changed(
                    &client,
                    "vpn",
                    None,
                    None,
                    0,
                    serde_json::json!({ "vpns": vpns }),
                    None,
                )
                .await
                {
                    Ok(()) => false,
                    Err(err) => {
                        tracing::warn!(error = %err, "broadcast failed");
                        true
                    }
                }
            }
        };
        if failed {
            tracing::warn!("broadcast failed; message dropped");
        }
    }
}

fn load_island(config_dir: &Path) -> Result<IslandFile, AdminError> {
    let data = fs::read_to_string(config_dir.join("island.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

async fn broadcast_config_changed(
    client: &AdminRouterClient,
    subsystem: &str,
    action: Option<String>,
    auto_apply: Option<bool>,
    version: u64,
    config: serde_json::Value,
    target_island: Option<String>,
) -> Result<(), AdminError> {
    let dst = match target_island {
        Some(island) => Destination::Unicast(format!("SY.opa.rules@{}", island)),
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

#[derive(Debug)]
struct AdminRequest {
    action: String,
    payload: serde_json::Value,
    target: String,
    unicast: Option<String>,
}

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
        matches!(self, OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check)
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
    island: Option<String>,
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
    island: String,
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
    island: String,
    status: String,
    payload: serde_json::Value,
}

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
    if let Some((status, resp)) =
        handle_island_paths(method.as_str(), path, &query, &body, tx, ctx, client).await?
    {
        respond_json(stream, status, &resp).await?;
        return Ok(());
    }
    match (method.as_str(), path) {
        ("GET", "/health") => {
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("GET", "/routes") => {
            let island = query.get("island").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_routes", island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/routes") => {
            let route: RouteConfig = serde_json::from_slice(&body)?;
            let island = query.get("island").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "add_route", serde_json::to_value(route)?, island)
                    .await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/routes") => {
            let prefix = query.get("prefix").cloned().unwrap_or_default();
            let island = query.get("island").cloned();
            let payload = serde_json::json!({ "prefix": prefix });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_route", payload, island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/vpns") => {
            let island = query.get("island").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_vpns", island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/vpns") => {
            let vpn: VpnConfig = serde_json::from_slice(&body)?;
            let island = query.get("island").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "add_vpn", serde_json::to_value(vpn)?, island)
                    .await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/vpns") => {
            let pattern = query.get("pattern").cloned().unwrap_or_default();
            let island = query.get("island").cloned();
            let payload = serde_json::json!({ "pattern": pattern });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_vpn", payload, island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/nodes") => {
            let island = query.get("island").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_nodes", island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/nodes") => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(&body)?
            };
            let island = query.get("island").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "run_node", payload, island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/nodes") => {
            let name = query.get("name").cloned().unwrap_or_default();
            let island = query.get("island").cloned();
            let payload = serde_json::json!({ "name": name });
            let (status, resp) =
                handle_admin_command(ctx, client, "kill_node", payload, island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/routers") => {
            let island = query.get("island").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_routers", island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/routers") => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(&body)?
            };
            let island = query.get("island").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "run_router", payload, island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/routers") => {
            let name = query.get("name").cloned().unwrap_or_default();
            let island = query.get("island").cloned();
            let payload = serde_json::json!({ "name": name });
            let (status, resp) =
                handle_admin_command(ctx, client, "kill_router", payload, island).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("PUT", "/config/routes") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            if let Some(routes) = update.routes {
                let _ = tx.send(BroadcastRequest::Routes(routes));
            }
            if let Some(vpns) = update.vpns {
                let _ = tx.send(BroadcastRequest::Vpns(vpns));
            }
            tracing::info!("config routes update received");
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("PUT", "/config/vpns") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            if let Some(vpns) = update.vpns {
                let _ = tx.send(BroadcastRequest::Vpns(vpns));
            }
            tracing::info!("config vpns update received");
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
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
                    .get("island")
                    .cloned()
                    .or_else(|| query.get("target").cloned()),
            );
            let (status, resp) = handle_opa_query(ctx, client, "get_policy", target).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/opa/status") => {
            let target = normalize_opa_target(
                query
                    .get("island")
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

async fn handle_island_paths(
    method: &str,
    path: &str,
    _query: &HashMap<String, String>,
    body: &[u8],
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: &AdminContext,
    client: &AdminRouterClient,
) -> Result<Option<(u16, String)>, AdminError> {
    let trimmed = path.trim_matches('/');
    let parts = trimmed.split('/').collect::<Vec<_>>();
    if parts.first().copied() != Some("islands") {
        return Ok(None);
    }
    if parts.len() < 2 {
        return Ok(None);
    }
    let island = parts[1].to_string();
    let rest = &parts[2..];
    match (method, rest) {
        ("GET", ["routes"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_routes", Some(island))
                .await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["routes"]) => {
            let route: RouteConfig = serde_json::from_slice(body)?;
            let (status, resp) = handle_admin_command(
                ctx,
                client,
                "add_route",
                serde_json::to_value(route)?,
                Some(island),
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["routes", prefix]) => {
            let payload = serde_json::json!({ "prefix": decode_percent(prefix) });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_route", payload, Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["vpns"]) => {
            let (status, resp) =
                handle_admin_query(ctx, client, "list_vpns", Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["vpns"]) => {
            let vpn: VpnConfig = serde_json::from_slice(body)?;
            let (status, resp) = handle_admin_command(
                ctx,
                client,
                "add_vpn",
                serde_json::to_value(vpn)?,
                Some(island),
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["vpns", pattern]) => {
            let payload = serde_json::json!({ "pattern": decode_percent(pattern) });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_vpn", payload, Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes"]) => {
            let (status, resp) =
                handle_admin_query(ctx, client, "list_nodes", Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_admin_command(ctx, client, "run_node", payload, Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["nodes", name]) => {
            let payload = serde_json::json!({ "name": decode_percent(name) });
            let (status, resp) =
                handle_admin_command(ctx, client, "kill_node", payload, Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["routers"]) => {
            let (status, resp) =
                handle_admin_query(ctx, client, "list_routers", Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["routers"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_admin_command(ctx, client, "run_router", payload, Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["routers", name]) => {
            let payload = serde_json::json!({ "name": decode_percent(name) });
            let (status, resp) =
                handle_admin_command(ctx, client, "kill_router", payload, Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.island = Some(island);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::CompileApply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "compile"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.island = Some(island);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Compile).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "apply"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.island = Some(island);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Apply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "rollback"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.island = Some(island);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Rollback).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "check"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.island = Some(island);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Check).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["opa", "policy"]) => {
            let (status, resp) =
                handle_opa_query(ctx, client, "get_policy", Some(island)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["opa", "status"]) => {
            let (status, resp) =
                handle_opa_query(ctx, client, "get_status", Some(island)).await?;
            Ok(Some((status, resp)))
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

fn load_node_uuid(dir: &Path, base_name: &str) -> Result<String, AdminError> {
    let path = dir.join(format!("{base_name}.uuid"));
    let data = fs::read_to_string(&path)?;
    let uuid = Uuid::parse_str(data.trim())?;
    Ok(uuid.to_string())
}

async fn respond_json(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<(), AdminError> {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        404 => "HTTP/1.1 404 Not Found",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.as_bytes().len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

fn next_opa_version(requested: Option<u64>) -> Result<u64, AdminError> {
    let path = PathBuf::from(json_router::paths::OPA_VERSION_PATH);
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
                return Err(format!(
                    "version must be greater than current (current={current})"
                )
                .into());
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
    let entrypoint = req.entrypoint.clone().unwrap_or_else(|| "router/target".to_string());
    let target = normalize_opa_target(req.island.take());

    if action.needs_rego() && req.rego.as_deref().unwrap_or("").is_empty() {
        let resp = serde_json::json!({
            "status": "error",
            "error_code": "COMPILE_ERROR",
            "error_detail": "rego_missing"
        });
        return Ok((400, resp.to_string()));
    }

    let mut responses = Vec::new();
    if matches!(action, OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check) {
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
            return Ok(build_opa_http_response(ctx, action, version, responses, target));
        }
    }

    if action.apply_after() {
        let apply_responses =
            send_opa_action(ctx, client, "apply", version, None, None, None, target.clone()).await?;
        if apply_responses.is_empty() || apply_responses.iter().any(|r| r.status != "ok") {
            let mut combined = responses;
            combined.extend(apply_responses);
            return Ok(build_opa_http_response(ctx, action, version, combined, target));
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

    Ok(build_opa_http_response(ctx, action, version, responses, target))
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
    island: Option<String>,
) -> Result<(u16, String), AdminError> {
    let request = build_admin_request(ctx, action, serde_json::json!({}), island);
    let response = send_admin_request(client, request).await;
    Ok(build_admin_http_response(action, response))
}

async fn handle_admin_command(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    payload: serde_json::Value,
    island: Option<String>,
) -> Result<(u16, String), AdminError> {
    let request = build_admin_request(ctx, action, payload, island);
    let target_island = extract_island_from_target(&request.target);
    let response = send_admin_request(client, request).await;
    if let Ok(ref payload) = response {
        if let Some(status) = payload.get("status").and_then(|v| v.as_str()) {
            if status == "ok" {
                if matches!(
                    action,
                    "add_route" | "delete_route" | "add_vpn" | "delete_vpn"
                ) {
                    if let Err(err) =
                        broadcast_full_config(ctx, client, action, target_island.as_deref()).await
                    {
                        tracing::warn!("broadcast after admin action failed: {err}");
                    }
                }
            }
        }
    }
    Ok(build_admin_http_response(action, response))
}

fn build_admin_request(
    ctx: &AdminContext,
    action: &str,
    payload: serde_json::Value,
    island: Option<String>,
) -> AdminRequest {
    let island_id = island.unwrap_or_else(|| ctx.island_id.clone());
    let base = match action {
        "list_routes" | "add_route" | "delete_route" | "list_vpns" | "add_vpn"
        | "delete_vpn" => "SY.config.routes",
        "list_nodes" | "run_node" | "kill_node" | "list_routers" | "run_router"
        | "kill_router" => "SY.orchestrator",
        _ => "SY.config.routes",
    };
    let target = if island_id.contains('@') {
        island_id
    } else {
        format!("{}@{}", base, island_id)
    };
    let unicast = if target.ends_with(&format!("@{}", ctx.island_id)) {
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
            trace_id,
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: None,
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let msg = match timeout(remaining, rx).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(_)) => return Err("admin response channel closed".into()),
        Err(_) => {
            client.drop_admin_waiter(&trace_id).await;
            return Err("admin request timeout".into());
        }
    };
    Ok(msg.payload)
}

fn build_admin_http_response(
    action: &str,
    response: Result<serde_json::Value, AdminError>,
) -> (u16, String) {
    match response {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("ok");
            let code = if status == "ok" { 200 } else { 500 };
            (
                code,
                serde_json::json!({
                    "status": status,
                    "action": action,
                    "payload": payload,
                })
                .to_string(),
            )
        }
        Err(err) => (
            500,
            serde_json::json!({
                "status": "error",
                "action": action,
                "error_code": "TIMEOUT",
                "error_detail": err.to_string(),
            })
            .to_string(),
        ),
    }
}

fn extract_island_from_target(target: &str) -> Option<String> {
    target.split_once('@').map(|(_, island)| island.to_string())
}

async fn broadcast_full_config(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target_island: Option<&str>,
) -> Result<(), AdminError> {
    let list_action = if action.contains("route") {
        "list_routes"
    } else {
        "list_vpns"
    };
    let list_req = build_admin_request(ctx, list_action, serde_json::json!({}), target_island.map(|s| s.to_string()));
    let response = send_admin_request(client, list_req).await?;
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

    let subsystem = if list_action == "list_routes" { "routes" } else { "vpn" };
    broadcast_config_changed(
        client,
        subsystem,
        None,
        None,
        0,
        config,
        None,
    )
    .await?;
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

    let expected = expected_islands(ctx, target.as_deref());
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
        .map(|island| format!("SY.opa.rules@{}", island));
    let dst = match target.as_deref() {
        Some(island) => Destination::Unicast(format!("SY.opa.rules@{}", island)),
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
            scope: None,
            target: target_pattern,
            priority: None,
            context: None,
        },
        payload: serde_json::json!({}),
    };
    client.sender.send(msg).await?;

    let expected = expected_islands(ctx, target.as_deref());
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

fn expected_islands(ctx: &AdminContext, target: Option<&str>) -> Vec<String> {
    if let Some(target) = target {
        return vec![target.to_string()];
    }
    let mut islands = Vec::new();
    islands.push(ctx.island_id.clone());
    for island in &ctx.authorized_islands {
        if island != &ctx.island_id {
            islands.push(island.clone());
        }
    }
    islands
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
        let island = payload.island.clone().unwrap_or_else(|| "unknown".to_string());
        let entry = OpaResponseEntry {
            island: island.clone(),
            status: payload.status.unwrap_or_else(|| "error".to_string()),
            version: payload.version,
            compile_time_ms: payload.compile_time_ms,
            wasm_size_bytes: payload.wasm_size_bytes,
            hash: payload.hash,
            error_code: payload.error_code,
            error_detail: payload.error_detail,
        };
        responses.insert(island, entry);
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
        let island = payload
            .get("island")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let status = payload
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("ok")
            .to_string();
        responses.insert(
            island.clone(),
            OpaQueryEntry {
                island,
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
    let expected = expected_islands(ctx, target.as_deref());
    let pending: Vec<String> = expected
        .into_iter()
        .filter(|island| !responses.iter().any(|r| r.island == *island))
        .collect();
    let timed_out = !pending.is_empty();
    let status = if timed_out || responses.is_empty() || responses.iter().any(|r| r.status != "ok") {
        500
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
        "error_code": if timed_out {
            Some("TIMEOUT".to_string())
        } else {
            None
        },
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
    let expected = expected_islands(ctx, target.as_deref());
    let pending: Vec<String> = expected
        .into_iter()
        .filter(|island| !responses.iter().any(|r| r.island == *island))
        .collect();
    let timed_out = !pending.is_empty();
    let status = if timed_out || responses.is_empty() || responses.iter().any(|r| r.status != "ok") {
        500
    } else {
        200
    };
    let body = serde_json::json!({
        "status": if status == 200 { "ok" } else { "error" },
        "action": action,
        "responses": responses,
        "pending": pending,
        "error_code": if timed_out {
            Some("TIMEOUT".to_string())
        } else {
            None
        },
    })
    .to_string();
    (status, body)
}
