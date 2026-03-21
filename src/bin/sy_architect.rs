use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::{admin_command, connect, AdminCommandRequest, NodeConfig, NodeError, NodeReceiver};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type ArchitectError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_ARCHITECT_LISTEN: &str = "127.0.0.1:3000";
const ROUTER_RECONNECT_DELAY_SECS: u64 = 2;

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    architect: Option<ArchitectSection>,
    ai_providers: Option<AiProvidersSection>,
}

#[derive(Debug, Deserialize)]
struct ArchitectSection {
    listen: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AiProvidersSection {
    openai: Option<OpenAiSection>,
}

#[derive(Debug, Deserialize)]
struct OpenAiSection {
    api_key: Option<String>,
}

struct ArchitectState {
    hive_id: String,
    node_name: String,
    listen: String,
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    router_connected: AtomicBool,
    ai_configured: AtomicBool,
    prompt_lock: Mutex<()>,
}

#[derive(Debug, Serialize)]
struct ArchitectStatus<'a> {
    status: &'a str,
    hive_id: &'a str,
    node_name: &'a str,
    listen: &'a str,
    router_connected: bool,
    ai_configured: bool,
}

#[derive(Debug, Deserialize)]
struct ChatRequest {
    message: String,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    status: String,
    mode: String,
    output: Value,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct PromptStore {
    prompts: HashMap<String, PromptSlot>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
struct PromptSlot {
    next_version: u64,
    draft: Option<StoredPromptVersion>,
    published: Option<StoredPromptVersion>,
    previous_published: Option<StoredPromptVersion>,
    history: Vec<PromptHistoryEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct StoredPromptVersion {
    version: u64,
    content: String,
    updated_at_ms: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct PromptHistoryEntry {
    version: u64,
    stage: String,
    updated_at_ms: u64,
}

#[derive(Debug, Deserialize)]
struct PromptCommand {
    op: String,
    prompt_id: Option<String>,
    content: Option<String>,
}

#[derive(Debug)]
struct ParsedAcmd {
    method: String,
    path: String,
    body: Option<Value>,
}

#[derive(Debug)]
struct AdminTranslation {
    admin_target: String,
    action: String,
    target_hive: String,
    params: Value,
}

#[tokio::main]
async fn main() -> Result<(), ArchitectError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_architect supports only Linux targets.");
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
    let listen = std::env::var("JSR_ARCHITECT_LISTEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            hive.architect
                .as_ref()
                .and_then(|section| section.listen.clone())
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_ARCHITECT_LISTEN.to_string());

    let state = Arc::new(ArchitectState {
        hive_id: hive.hive_id.clone(),
        node_name: format!("SY.architect@{}", hive.hive_id),
        listen: listen.clone(),
        config_dir,
        state_dir,
        socket_dir,
        router_connected: AtomicBool::new(false),
        ai_configured: AtomicBool::new(hive_ai_configured(&hive)),
        prompt_lock: Mutex::new(()),
    });

    let node_config = NodeConfig {
        name: "SY.architect".to_string(),
        router_socket: state.socket_dir.clone(),
        uuid_persistence_dir: state.state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: state.config_dir.clone(),
        version: "0.1.0".to_string(),
    };

    let router_state = Arc::clone(&state);
    tokio::spawn(async move {
        router_connect_loop(node_config, router_state).await;
    });

    tracing::info!(
        node = %state.node_name,
        listen = %state.listen,
        ai_configured = state.ai_configured.load(Ordering::Relaxed),
        "sy.architect starting"
    );

    run_http_server(&listen, state).await?;
    Ok(())
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, ArchitectError> {
    let hive_path = config_dir.join("hive.yaml");
    let data = fs::read_to_string(&hive_path)?;
    Ok(serde_yaml::from_str(&data)?)
}

fn hive_ai_configured(hive: &HiveFile) -> bool {
    hive.ai_providers
        .as_ref()
        .and_then(|providers| providers.openai.as_ref())
        .and_then(|openai| openai.api_key.as_ref())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

async fn router_connect_loop(config: NodeConfig, state: Arc<ArchitectState>) {
    loop {
        match connect(&config).await {
            Ok((_sender, receiver)) => {
                state.router_connected.store(true, Ordering::Relaxed);
                tracing::info!(node = %state.node_name, "sy.architect connected to router");
                if let Err(err) = router_recv_loop(receiver).await {
                    tracing::warn!(error = %err, "sy.architect router loop ended");
                }
                state.router_connected.store(false, Ordering::Relaxed);
            }
            Err(err) => {
                state.router_connected.store(false, Ordering::Relaxed);
                tracing::warn!(error = %err, "sy.architect router connect failed");
            }
        }
        time::sleep(Duration::from_secs(ROUTER_RECONNECT_DELAY_SECS)).await;
    }
}

async fn router_recv_loop(mut receiver: NodeReceiver) -> Result<(), NodeError> {
    loop {
        let msg = receiver.recv().await?;
        tracing::info!(
            src = %msg.routing.src,
            dst = ?msg.routing.dst,
            msg_type = %msg.meta.msg_type,
            msg = ?msg.meta.msg,
            "sy.architect received message"
        );
    }
}

async fn run_http_server(listen: &str, state: Arc<ArchitectState>) -> Result<(), ArchitectError> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(addr = %listen, "sy.architect http listening");
    loop {
        let (mut stream, _) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(err) = handle_http_connection(&mut stream, state).await {
                tracing::warn!(error = %err, "sy.architect http request failed");
            }
        });
    }
}

async fn handle_http_connection(
    stream: &mut tokio::net::TcpStream,
    state: Arc<ArchitectState>,
) -> Result<(), ArchitectError> {
    let request = read_http_request(stream).await?;
    let method = request.method.as_str();
    let path = request.path.as_str();

    match (method, path) {
        ("GET", _) if is_status_path(path) => {
            let status = ArchitectStatus {
                status: "ok",
                hive_id: &state.hive_id,
                node_name: &state.node_name,
                listen: &state.listen,
                router_connected: state.router_connected.load(Ordering::Relaxed),
                ai_configured: state.ai_configured.load(Ordering::Relaxed),
            };
            let body = serde_json::to_vec(&status)?;
            write_http_response(stream, "200 OK", "application/json", &body).await?;
        }
        ("POST", _) if is_chat_path(path) => {
            let req: ChatRequest = serde_json::from_slice(&request.body)?;
            let out = handle_chat_message(&state, req.message).await;
            let body = serde_json::to_vec(&out)?;
            write_http_response(stream, "200 OK", "application/json", &body).await?;
        }
        ("GET", _) if !is_api_path(path) => {
            let body = architect_index_html(&state);
            write_http_response(stream, "200 OK", "text/html; charset=utf-8", body.as_bytes())
                .await?;
        }
        _ => {
            write_http_response(
                stream,
                "404 Not Found",
                "application/json",
                br#"{"error":"not_found"}"#,
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_chat_message(state: &ArchitectState, message: String) -> ChatResponse {
    if let Some(raw) = message.strip_prefix("FCMD:") {
        match handle_fcmd(state, raw.trim()).await {
            Ok(output) => ChatResponse {
                status: "ok".to_string(),
                mode: "fcmd".to_string(),
                output,
            },
            Err(err) => ChatResponse {
                status: "error".to_string(),
                mode: "fcmd".to_string(),
                output: json!({ "error": err.to_string() }),
            },
        }
    } else if let Some(raw) = message.strip_prefix("ACMD:") {
        match handle_acmd(state, raw.trim()).await {
            Ok(output) => ChatResponse {
                status: "ok".to_string(),
                mode: "acmd".to_string(),
                output,
            },
            Err(err) => ChatResponse {
                status: "error".to_string(),
                mode: "acmd".to_string(),
                output: json!({ "error": err.to_string() }),
            },
        }
    } else {
        ChatResponse {
            status: "ok".to_string(),
            mode: "chat".to_string(),
            output: json!({
                "message": "AI chat path not implemented yet. Use FCMD: for prompt control or ACMD: for admin passthrough.",
                "ai_configured": state.ai_configured.load(Ordering::Relaxed),
            }),
        }
    }
}

async fn handle_fcmd(state: &ArchitectState, raw: &str) -> Result<Value, ArchitectError> {
    let cmd: PromptCommand = serde_json::from_str(raw)?;
    let prompt_id = cmd
        .prompt_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let _guard = state.prompt_lock.lock().await;
    let path = prompt_store_path(state);
    let mut store = load_prompt_store(&path)?;
    let now = now_epoch_ms();

    match cmd.op.as_str() {
        "prompt.get" => {
            let prompt_id = prompt_id.ok_or("prompt_id is required")?;
            let slot = store.prompts.get(prompt_id).cloned().unwrap_or_default();
            Ok(json!({
                "prompt_id": prompt_id,
                "draft": slot.draft,
                "published": slot.published,
                "previous_published": slot.previous_published,
            }))
        }
        "prompt.update_draft" => {
            let prompt_id = prompt_id.ok_or("prompt_id is required")?.to_string();
            let content = cmd
                .content
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or("content is required")?
                .to_string();
            let slot = store.prompts.entry(prompt_id.clone()).or_default();
            slot.next_version += 1;
            let version = StoredPromptVersion {
                version: slot.next_version,
                content,
                updated_at_ms: now,
            };
            slot.history.push(PromptHistoryEntry {
                version: version.version,
                stage: "draft".to_string(),
                updated_at_ms: now,
            });
            slot.draft = Some(version.clone());
            save_prompt_store(&path, &store)?;
            Ok(json!({
                "prompt_id": prompt_id,
                "draft": version,
            }))
        }
        "prompt.publish" => {
            let prompt_id = prompt_id.ok_or("prompt_id is required")?.to_string();
            let response = {
                let slot = store.prompts.entry(prompt_id.clone()).or_default();
                let draft = slot.draft.clone().ok_or("draft missing")?;
                slot.previous_published = slot.published.clone();
                slot.published = Some(draft.clone());
                slot.draft = None;
                slot.history.push(PromptHistoryEntry {
                    version: draft.version,
                    stage: "published".to_string(),
                    updated_at_ms: now,
                });
                json!({
                    "prompt_id": prompt_id,
                    "published": draft,
                    "previous_published": slot.previous_published.clone(),
                })
            };
            save_prompt_store(&path, &store)?;
            Ok(response)
        }
        "prompt.rollback" => {
            let prompt_id = prompt_id.ok_or("prompt_id is required")?.to_string();
            let response = {
                let slot = store.prompts.entry(prompt_id.clone()).or_default();
                let previous = slot
                    .previous_published
                    .clone()
                    .ok_or("previous_published missing")?;
                let current = slot.published.clone();
                slot.published = Some(previous.clone());
                slot.previous_published = current;
                slot.history.push(PromptHistoryEntry {
                    version: previous.version,
                    stage: "rollback".to_string(),
                    updated_at_ms: now,
                });
                json!({
                    "prompt_id": prompt_id,
                    "published": previous,
                    "previous_published": slot.previous_published.clone(),
                })
            };
            save_prompt_store(&path, &store)?;
            Ok(response)
        }
        "prompt.history" => {
            let prompt_id = prompt_id.ok_or("prompt_id is required")?;
            let slot = store.prompts.get(prompt_id).cloned().unwrap_or_default();
            Ok(json!({
                "prompt_id": prompt_id,
                "history": slot.history,
            }))
        }
        other => Err(format!("unsupported FCMD op: {other}").into()),
    }
}

async fn handle_acmd(state: &ArchitectState, raw: &str) -> Result<Value, ArchitectError> {
    let parsed = parse_acmd(raw)?;
    let translated = translate_acmd(&state.hive_id, parsed)?;
    execute_admin_translation(state, translated).await
}

fn parse_acmd(raw: &str) -> Result<ParsedAcmd, ArchitectError> {
    let mut text = raw.trim();
    if !text.starts_with("curl ") {
        return Err("ACMD must start with 'curl '".into());
    }
    let mut body = None;
    if let Some((head, body_raw)) = text.rsplit_once(" -d ") {
        text = head.trim();
        let body_raw = strip_wrapping_quotes(body_raw.trim());
        body = Some(serde_json::from_str(body_raw)?);
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() < 4 || tokens[0] != "curl" || tokens[1] != "-X" {
        return Err("ACMD syntax must be: curl -X METHOD /relative/path [-d '{...}']".into());
    }
    let method = tokens[2].trim().to_ascii_uppercase();
    let path = tokens[3].trim().to_string();
    if !path.starts_with('/') {
        return Err("ACMD path must be relative and start with '/'".into());
    }
    Ok(ParsedAcmd { method, path, body })
}

fn translate_acmd(local_hive_id: &str, parsed: ParsedAcmd) -> Result<AdminTranslation, ArchitectError> {
    let segments: Vec<&str> = parsed.path.split('/').filter(|segment| !segment.is_empty()).collect();
    if segments.len() < 2 || segments[0] != "hives" {
        return Err("ACMD path must start with /hives/{hive}/...".into());
    }
    let hive_id = segments[1].to_string();
    let admin_target = format!("SY.admin@{local_hive_id}");

    match (parsed.method.as_str(), segments.as_slice()) {
        ("GET", ["hives", _, "nodes"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_nodes".to_string(),
            target_hive: hive_id,
            params: json!({}),
        }),
        ("GET", ["hives", _, "nodes", node_name, "status"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_node_status".to_string(),
            target_hive: hive_id,
            params: json!({ "node_name": node_name }),
        }),
        ("GET", ["hives", _, "identity", "ilks"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_ilks".to_string(),
            target_hive: hive_id,
            params: json!({}),
        }),
        ("GET", ["hives", _, "identity", "ilks", ilk_id]) => Ok(AdminTranslation {
            admin_target,
            action: "get_ilk".to_string(),
            target_hive: hive_id,
            params: json!({ "ilk_id": ilk_id }),
        }),
        ("DELETE", ["hives", _, "nodes", node_name, "instance"]) => Ok(AdminTranslation {
            admin_target,
            action: "remove_node_instance".to_string(),
            target_hive: hive_id,
            params: json!({ "node_name": node_name }),
        }),
        ("DELETE", ["hives", _, "nodes", node_name]) => {
            let force = parsed
                .body
                .as_ref()
                .and_then(|value| value.get("force"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(AdminTranslation {
                admin_target,
                action: "kill_node".to_string(),
                target_hive: hive_id,
                params: json!({ "node_name": node_name, "force": force }),
            })
        }
        ("POST", ["hives", _, "nodes", node_name, "messages"]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("ACMD body for send message must be a JSON object".into());
            }
            params["node_name"] = Value::String((*node_name).to_string());
            Ok(AdminTranslation {
                admin_target,
                action: "send_node_message".to_string(),
                target_hive: hive_id,
                params,
            })
        }
        _ => Err(format!("unsupported ACMD path: {} {}", parsed.method, parsed.path).into()),
    }
}

async fn execute_admin_translation(
    state: &ArchitectState,
    translation: AdminTranslation,
) -> Result<Value, ArchitectError> {
    let node_config = NodeConfig {
        name: format!("SY.architect.acmd.{}", Uuid::new_v4().simple()),
        router_socket: state.socket_dir.clone(),
        uuid_persistence_dir: state.state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Ephemeral,
        config_dir: state.config_dir.clone(),
        version: "0.1.0".to_string(),
    };
    let (sender, mut receiver) = connect(&node_config).await?;
    let out = admin_command(
        &sender,
        &mut receiver,
        AdminCommandRequest {
            admin_target: &translation.admin_target,
            action: &translation.action,
            target: Some(&translation.target_hive),
            params: translation.params,
            request_id: None,
            timeout: Duration::from_secs(10),
        },
    )
    .await?;
    Ok(json!({
        "status": out.status,
        "action": out.action,
        "payload": out.payload,
        "error_code": out.error_code,
        "error_detail": out.error_detail,
        "request_id": out.request_id,
        "trace_id": out.trace_id,
    }))
}

fn prompt_store_path(state: &ArchitectState) -> PathBuf {
    json_router::paths::storage_root_dir()
        .join("nodes")
        .join("SY")
        .join(format!("SY.architect@{}", state.hive_id))
        .join("prompts.json")
}

fn load_prompt_store(path: &Path) -> Result<PromptStore, ArchitectError> {
    if !path.exists() {
        return Ok(PromptStore::default());
    }
    let data = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}

fn save_prompt_store(path: &Path, store: &PromptStore) -> Result<(), ArchitectError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(store)?)?;
    Ok(())
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn strip_wrapping_quotes(input: &str) -> &str {
    if input.len() >= 2 {
        let bytes = input.as_bytes();
        let first = bytes[0];
        let last = bytes[input.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return &input[1..input.len() - 1];
        }
    }
    input
}

fn is_api_path(path: &str) -> bool {
    path.contains("/api/")
        || path == "/api/status"
        || path == "/api/chat"
        || path.ends_with("/api/status")
        || path.ends_with("/api/chat")
        || path == "/healthz"
}

fn is_status_path(path: &str) -> bool {
    path == "/healthz" || path == "/api/status" || path.ends_with("/api/status")
}

fn is_chat_path(path: &str) -> bool {
    path == "/api/chat" || path.ends_with("/api/chat")
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Result<HttpRequest, ArchitectError> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0_u8; 4096];
    let mut header_end = None;
    let mut content_length = 0_usize;

    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if header_end.is_none() {
            header_end = find_header_end(&buf);
            if let Some(end) = header_end {
                content_length = parse_content_length(&buf[..end])?;
            }
        }
        if let Some(end) = header_end {
            if buf.len() >= end + 4 + content_length {
                break;
            }
        }
        if buf.len() > 1024 * 1024 {
            return Err("request too large".into());
        }
    }

    let end = header_end.ok_or("invalid http request")?;
    let header = String::from_utf8_lossy(&buf[..end]);
    let request_line = header.lines().next().ok_or("missing request line")?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_string();
    let path = request_parts.next().unwrap_or("/").to_string();
    let body_start = end + 4;
    let body_end = body_start + content_length;
    let body = if body_end <= buf.len() {
        buf[body_start..body_end].to_vec()
    } else {
        Vec::new()
    };
    Ok(HttpRequest { method, path, body })
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(header: &[u8]) -> Result<usize, ArchitectError> {
    let text = String::from_utf8_lossy(header);
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Content-Length:") {
            return Ok(value.trim().parse::<usize>()?);
        }
        if let Some(value) = line.strip_prefix("content-length:") {
            return Ok(value.trim().parse::<usize>()?);
        }
    }
    Ok(0)
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), ArchitectError> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn architect_index_html(state: &ArchitectState) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>SY.architect</title>
  <style>
    body {{ margin: 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; background: #efe5d2; color: #1e1a16; }}
    .shell {{ max-width: 960px; margin: 0 auto; padding: 24px; }}
    .card {{ background: #fbf8f1; border: 1px solid #d3c4a4; border-radius: 14px; padding: 16px; box-shadow: 0 10px 30px rgba(75, 55, 23, 0.08); }}
    .header {{ display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap; margin-bottom: 16px; }}
    .title {{ font-size: 28px; font-weight: 700; }}
    .meta {{ font-size: 13px; color: #5a4c35; }}
    .log {{ height: 360px; overflow: auto; white-space: pre-wrap; background: #181410; color: #f6f1e8; border-radius: 12px; padding: 12px; }}
    .row {{ display: flex; gap: 8px; margin-top: 12px; }}
    textarea {{ flex: 1; min-height: 84px; resize: vertical; border-radius: 12px; border: 1px solid #c8b48f; padding: 12px; font: inherit; background: #fffdf9; }}
    button {{ border: 0; border-radius: 12px; padding: 12px 18px; background: #1c1a18; color: #f8f1e5; font: inherit; cursor: pointer; }}
    button:hover {{ background: #3b2d1f; }}
    .pill {{ display: inline-block; padding: 4px 8px; border-radius: 999px; background: #e2d5bb; margin-right: 8px; }}
  </style>
</head>
<body>
  <div class="shell">
    <div class="card">
      <div class="header">
        <div>
          <div class="title">SY.architect</div>
          <div class="meta">Node: {node} · Hive: {hive} · Listen: {listen}</div>
        </div>
        <div class="meta">
          <span class="pill">Router connected: <span id="router">{router}</span></span>
          <span class="pill">AI configured: <span id="ai">{ai}</span></span>
        </div>
      </div>
      <div class="log" id="log">System: architect scaffold ready.
System: use FCMD:{{...}} for local prompt control.
System: use ACMD: curl -X GET /hives/{hive}/nodes for admin passthrough.</div>
      <div class="row">
        <textarea id="input" placeholder="Type a normal message, FCMD: {{...}}, or ACMD: curl -X GET /hives/{hive}/nodes"></textarea>
      </div>
      <div class="row">
        <button id="send">Send</button>
      </div>
    </div>
  </div>
  <script>
    const rawPath = window.location.pathname.replace(/\/+$/, "");
    const base = rawPath === "" ? "" : rawPath;
    const statusUrl = (base || "") + "/api/status";
    const chatUrl = (base || "") + "/api/chat";
    const log = document.getElementById("log");
    const input = document.getElementById("input");
    const send = document.getElementById("send");
    function append(label, value) {{
      log.textContent += "\\n" + label + ": " + value;
      log.scrollTop = log.scrollHeight;
    }}
    async function refreshStatus() {{
      try {{
        const res = await fetch(statusUrl);
        const data = await res.json();
        document.getElementById("router").textContent = String(data.router_connected);
        document.getElementById("ai").textContent = String(data.ai_configured);
      }} catch (_err) {{}}
    }}
    async function submit() {{
      const message = input.value.trim();
      if (!message) return;
      append("User", message);
      input.value = "";
      const res = await fetch(chatUrl, {{
        method: "POST",
        headers: {{ "Content-Type": "application/json" }},
        body: JSON.stringify({{ message }})
      }});
      const data = await res.json();
      append("Architect", JSON.stringify(data.output, null, 2));
      refreshStatus();
    }}
    send.addEventListener("click", submit);
    input.addEventListener("keydown", (event) => {{
      if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {{
        submit();
      }}
    }});
    refreshStatus();
    setInterval(refreshStatus, 5000);
  </script>
</body>
</html>"#,
        node = state.node_name,
        hive = state.hive_id,
        listen = state.listen,
        router = state.router_connected.load(Ordering::Relaxed),
        ai = state.ai_configured.load(Ordering::Relaxed),
    )
}
