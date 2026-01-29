use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use std::future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use jsr_client::{NodeClient, NodeConfig};
use jsr_client::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, SCOPE_GLOBAL,
    SYSTEM_KIND,
};


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
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let (broadcast_tx, broadcast_rx) = mpsc::unbounded_channel::<BroadcastRequest>();
    let http_tx = broadcast_tx.clone();
    let http_ctx = AdminContext {
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        socket_dir: socket_dir.clone(),
        island_id,
        authorized_islands,
    };
    tokio::spawn(async move {
        if let Err(err) = run_http_server(&admin_listen, &http_tx, http_ctx).await {
            tracing::error!("http server error: {err}");
        }
    });

    let loop_config_dir = config_dir.clone();
    let loop_state_dir = state_dir.clone();
    let loop_socket_dir = socket_dir.clone();
    tokio::spawn(async move {
        run_broadcast_loop(broadcast_rx, loop_config_dir, loop_state_dir, loop_socket_dir).await;
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
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
) {
    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let mut client = match NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!("broadcast loop connect failed: {err}");
            return;
        }
    };
    tracing::info!("connected to router");

    while let Some(req) = rx.recv().await {
        let failed = match req {
            BroadcastRequest::Routes(routes) => {
                match broadcast_config_changed(
                    &mut client,
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
                    &mut client,
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
            match NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await {
                Ok(new_client) => {
                    client = new_client;
                    tracing::info!("reconnected to router");
                }
                Err(err) => {
                    tracing::warn!(error = %err, "reconnect failed");
                }
            }
        }
    }
}

fn load_island(config_dir: &Path) -> Result<IslandFile, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(config_dir.join("island.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

async fn broadcast_config_changed(
    client: &mut NodeClient,
    subsystem: &str,
    action: Option<String>,
    auto_apply: Option<bool>,
    version: u64,
    config: serde_json::Value,
    target_island: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dst = match target_island {
        Some(island) => Destination::Unicast(format!("SY.opa.rules@{}", island)),
        None => Destination::Broadcast,
    };
    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
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
            action,
            auto_apply,
            version,
            config,
        })?,
    };
    client.send(&msg).await?;
    tracing::info!(subsystem = subsystem, version = version, "config changed broadcast sent");
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
            OpaAction::CompileApply => Some(true),
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

async fn run_http_server(
    listen: &str,
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: AdminContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(addr = %listen, "sy.admin http listening");
    loop {
        let (mut stream, _) = listener.accept().await?;
        let tx = tx.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_http(&mut stream, &tx, &ctx).await {
                tracing::warn!("http handler error: {err}");
            }
        });
    }
}

async fn handle_http(
    stream: &mut tokio::net::TcpStream,
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: &AdminContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let (method, path, headers, body) = read_http_request(stream).await?;
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => {
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
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
            let (status, resp) = handle_opa_http(ctx, req, OpaAction::CompileApply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/compile") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, req, OpaAction::Compile).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/apply") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, req, OpaAction::Apply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/rollback") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, req, OpaAction::Rollback).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/check") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, req, OpaAction::Check).await?;
            respond_json(stream, status, &resp).await?;
        }
        _ => {
            let _ = headers;
            respond_json(stream, 404, r#"{"error":"not_found"}"#).await?;
        }
    }
    Ok(())
}

async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<(String, String, HashMap<String, String>, Vec<u8>), Box<dyn std::error::Error>> {
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

async fn respond_json(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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

fn next_opa_version(
    state_dir: &Path,
    requested: Option<u64>,
) -> Result<u64, Box<dyn std::error::Error>> {
    let path = state_dir.join("opa-version.txt");
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
    mut req: OpaRequest,
    action: OpaAction,
) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let version = match action {
        OpaAction::Apply => {
            let Some(version) = req.version else {
                let resp = serde_json::json!({
                    "status": "error",
                    "error": "version_required"
                });
                return Ok((400, resp.to_string()));
            };
            version
        }
        OpaAction::Rollback => req.version.unwrap_or(0),
        _ => next_opa_version(&ctx.state_dir, req.version)?,
    };
    let entrypoint = req.entrypoint.clone().unwrap_or_else(|| "router/target".to_string());
    let target = req.island.take();

    if action.needs_rego() && req.rego.as_deref().unwrap_or("").is_empty() {
        let resp = serde_json::json!({
            "status": "error",
            "error": "rego_missing"
        });
        return Ok((400, resp.to_string()));
    }

    let mut responses = Vec::new();
    if matches!(action, OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check) {
        responses = send_opa_action(
            ctx,
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
            send_opa_action(ctx, "apply", version, None, None, None, target.clone()).await?;
        if apply_responses.is_empty() || apply_responses.iter().any(|r| r.status != "ok") {
            let mut combined = responses;
            combined.extend(apply_responses);
            return Ok(build_opa_http_response(ctx, action, version, combined, target));
        }
        responses.extend(apply_responses);
    } else if matches!(action, OpaAction::Apply | OpaAction::Rollback) {
        let apply_action = action.as_str();
        responses =
            send_opa_action(ctx, apply_action, version, None, None, None, target.clone()).await?;
    }

    Ok(build_opa_http_response(ctx, action, version, responses, target))
}

async fn send_opa_action(
    ctx: &AdminContext,
    action: &str,
    version: u64,
    rego: Option<String>,
    entrypoint: Option<String>,
    auto_apply: Option<bool>,
    target: Option<String>,
) -> Result<Vec<OpaResponseEntry>, Box<dyn std::error::Error>> {
    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: ctx.socket_dir.clone(),
        uuid_persistence_dir: ctx.state_dir.join("nodes"),
        config_dir: ctx.config_dir.clone(),
        version: "1.0".to_string(),
    };
    let mut client = NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    let mut cfg = serde_json::json!({});
    if let Some(rego) = rego {
        cfg = serde_json::json!({
            "rego": rego,
            "entrypoint": entrypoint.unwrap_or_else(|| "router/target".to_string()),
        });
    }
    broadcast_config_changed(
        &mut client,
        "opa",
        Some(action.to_string()),
        auto_apply,
        version,
        cfg,
        target.clone(),
    )
    .await?;

    let expected = expected_islands(ctx, target.as_deref());
    let responses = collect_opa_responses(&mut client, action, version, &expected).await;
    Ok(responses)
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
    client: &mut NodeClient,
    action: &str,
    version: u64,
    expected: &[String],
) -> Vec<OpaResponseEntry> {
    use tokio::time::{timeout, Instant};

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut responses: HashMap<String, OpaResponseEntry> = HashMap::new();

    while responses.len() < expected.len() && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = match timeout(remaining, client.recv()).await {
            Ok(Ok(msg)) => msg,
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
    let status = if responses.is_empty() || responses.iter().any(|r| r.status != "ok") {
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
    })
    .to_string();
    (status, body)
}
