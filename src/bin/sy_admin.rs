use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use std::future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use jsr_client::{NodeClient, NodeConfig};
use jsr_client::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, SCOPE_GLOBAL,
    SYSTEM_KIND,
};
use json_router::shm::now_epoch_ms;

const DEFAULT_CONFIG_DIR: &str = "/etc/json-router";
const DEFAULT_STATE_DIR: &str = "/var/lib/json-router/state";
const DEFAULT_SOCKET_DIR: &str = "/var/run/json-router/routers";

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
    role: Option<String>,
    admin: Option<AdminSection>,
}

#[derive(Debug, Deserialize)]
struct AdminSection {
    listen: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct SyConfigFile {
    version: u64,
    updated_at: String,
    #[serde(default)]
    routes: Vec<RouteConfig>,
    #[serde(default)]
    vpns: Vec<VpnConfig>,
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

    let config_dir = PathBuf::from(
        std::env::var("JSR_CONFIG_DIR").unwrap_or_else(|_| DEFAULT_CONFIG_DIR.to_string()),
    );
    let state_dir = PathBuf::from(
        std::env::var("JSR_STATE_DIR").unwrap_or_else(|_| DEFAULT_STATE_DIR.to_string()),
    );
    let socket_dir = PathBuf::from(
        std::env::var("JSR_SOCKET_DIR").unwrap_or_else(|_| DEFAULT_SOCKET_DIR.to_string()),
    );

    let island = load_island(&config_dir)?;
    if island.role.as_deref() != Some("mother") {
        tracing::warn!("SY.admin solo corre en mother island; role != mother");
        return Ok(());
    }
    let admin_listen = island
        .admin
        .and_then(|admin| admin.listen)
        .unwrap_or_else(|| "0.0.0.0:8080".to_string());

    let notify = std::sync::Arc::new(Notify::new());
    let shared_config = std::sync::Arc::new(tokio::sync::Mutex::new(SyConfigFile {
        version: 1,
        updated_at: now_epoch_ms().to_string(),
        routes: Vec::new(),
        vpns: Vec::new(),
    }));
    let notify_http = std::sync::Arc::clone(&notify);
    let config_http = std::sync::Arc::clone(&shared_config);
    tokio::spawn(async move {
        if let Err(err) = run_http_server(&admin_listen, &config_http, &notify_http).await {
            tracing::error!("http server error: {err}");
        }
    });

    let notify_loop = std::sync::Arc::clone(&notify);
    let loop_config = std::sync::Arc::clone(&shared_config);
    let loop_config_dir = config_dir.clone();
    let loop_state_dir = state_dir.clone();
    let loop_socket_dir = socket_dir.clone();
    tokio::spawn(async move {
        if let Err(err) = run_broadcast_loop(
            loop_config,
            loop_config_dir,
            loop_state_dir,
            loop_socket_dir,
            notify_loop,
        )
        .await
        {
            tracing::error!("broadcast loop error: {err}");
        }
    });

    future::pending::<()>().await;
    Ok(())
}

async fn run_broadcast_loop(
    shared_config: std::sync::Arc<tokio::sync::Mutex<SyConfigFile>>,
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    notify: std::sync::Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let mut client = NodeClient::connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!("connected to router");

    let mut last_version = {
        let guard = shared_config.lock().await;
        guard.version
    };
    let mut ticker = time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = ticker.tick() => {},
            _ = notify.notified() => {},
        }
        let config = {
            let guard = shared_config.lock().await;
            guard.clone()
        };
        if config.version <= last_version {
            continue;
        }
        broadcast_config_changed(
            &mut client,
            "routes",
            config.version,
            serde_json::json!({ "routes": config.routes }),
        )
        .await?;
        broadcast_config_changed(
            &mut client,
            "vpn",
            config.version,
            serde_json::json!({ "vpns": config.vpns }),
        )
        .await?;
        last_version = config.version;
    }
}

fn load_island(config_dir: &Path) -> Result<IslandFile, Box<dyn std::error::Error>> {
    let data = fs::read_to_string(config_dir.join("island.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

async fn broadcast_config_changed(
    client: &mut NodeClient,
    subsystem: &str,
    version: u64,
    config: serde_json::Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
            dst: Destination::Broadcast,
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

async fn run_http_server(
    listen: &str,
    shared_config: &std::sync::Arc<tokio::sync::Mutex<SyConfigFile>>,
    notify: &std::sync::Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(addr = %listen, "sy.admin http listening");
    loop {
        let (mut stream, _) = listener.accept().await?;
        let config = std::sync::Arc::clone(shared_config);
        let notify = std::sync::Arc::clone(&notify);
        tokio::spawn(async move {
            if let Err(err) = handle_http(&mut stream, &config, &notify).await {
                tracing::warn!("http handler error: {err}");
            }
        });
    }
}

async fn handle_http(
    stream: &mut tokio::net::TcpStream,
    shared_config: &std::sync::Arc<tokio::sync::Mutex<SyConfigFile>>,
    notify: &std::sync::Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (method, path, headers, body) = read_http_request(stream).await?;
    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => {
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("GET", "/config/routes") => {
            let config = {
                let guard = shared_config.lock().await;
                guard.clone()
            };
            respond_json(stream, 200, &serde_json::to_string(&config)?).await?;
        }
        ("PUT", "/config/routes") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            let mut current = shared_config.lock().await;
            if let Some(routes) = update.routes {
                current.routes = routes;
            }
            if let Some(vpns) = update.vpns {
                current.vpns = vpns;
            }
            let next_version = update
                .version
                .unwrap_or_else(|| current.version.saturating_add(1));
            current.version = next_version;
            current.updated_at = now_epoch_ms().to_string();
            notify.notify_one();
            tracing::info!(version = current.version, "config routes updated");
            respond_json(stream, 200, &serde_json::to_string(&*current)?).await?;
        }
        ("PUT", "/config/vpns") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            let mut current = shared_config.lock().await;
            if let Some(vpns) = update.vpns {
                current.vpns = vpns;
            }
            let next_version = update
                .version
                .unwrap_or_else(|| current.version.saturating_add(1));
            current.version = next_version;
            current.updated_at = now_epoch_ms().to_string();
            notify.notify_one();
            tracing::info!(version = current.version, "config vpns updated");
            respond_json(stream, 200, &serde_json::to_string(&*current)?).await?;
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
