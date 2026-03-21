use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use fluxbee_sdk::{
    admin_command, connect, AdminCommandRequest, NodeConfig, NodeError, NodeReceiver,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

    let app = Router::new()
        .route("/", any(root_handler))
        .route("/healthz", any(dynamic_handler))
        .route("/api/status", any(dynamic_handler))
        .route("/api/chat", any(dynamic_handler))
        .route("/*path", any(dynamic_handler))
        .with_state(Arc::clone(&state));

    let listener = TcpListener::bind(&listen).await?;
    tracing::info!(
        node = %state.node_name,
        listen = %state.listen,
        ai_configured = state.ai_configured.load(Ordering::Relaxed),
        "sy.architect axum listening"
    );
    axum::serve(listener, app).await?;
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

async fn root_handler(State(state): State<Arc<ArchitectState>>) -> Html<String> {
    Html(architect_index_html(&state))
}

async fn dynamic_handler(
    State(state): State<Arc<ArchitectState>>,
    uri: Uri,
    method: Method,
    request: Request,
) -> Response {
    let path = uri.path();
    match (method, path) {
        (Method::GET, _) if is_status_path(path) => {
            let status = ArchitectStatus {
                status: "ok",
                hive_id: &state.hive_id,
                node_name: &state.node_name,
                listen: &state.listen,
                router_connected: state.router_connected.load(Ordering::Relaxed),
                ai_configured: state.ai_configured.load(Ordering::Relaxed),
            };
            Json(status).into_response()
        }
        (Method::POST, _) if is_chat_path(path) => {
            let body = match axum::body::to_bytes(request.into_body(), 1024 * 1024).await {
                Ok(body) => body,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid body: {err}") })),
                    )
                        .into_response()
                }
            };
            let req: ChatRequest = match serde_json::from_slice(&body) {
                Ok(req) => req,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid json: {err}") })),
                    )
                        .into_response()
                }
            };
            let out = handle_chat_message(&state, req.message).await;
            Json(out).into_response()
        }
        (Method::GET, _) if !is_api_path(path) => Html(architect_index_html(&state)).into_response(),
        _ => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
    }
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

fn architect_index_html(state: &ArchitectState) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>SY.architect</title>
  <style>
    :root {{
      --bg: #ffffff;
      --panel: #ffffff;
      --panel-alt: #f8f9fb;
      --panel-soft: #f3f6fc;
      --text: #171717;
      --muted: #667085;
      --line: #e6e9f0;
      --accent: #4575dc;
      --accent-soft: #e8f0ff;
      --logo-dark: #222222;
      --success: #1f7a4d;
      --success-soft: #e9f7ef;
      --warning: #8a5b00;
      --warning-soft: #fff5df;
      --bubble-user: #eef3ff;
      --bubble-architect: #ffffff;
      --bubble-system: #f8f9fb;
      --shadow: 0 18px 46px rgba(31, 42, 55, 0.08);
    }}
    * {{ box-sizing: border-box; }}
    html {{ scroll-behavior: smooth; }}
    body {{
      margin: 0;
      font-family: Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: var(--bg);
      color: var(--text);
      -webkit-font-smoothing: antialiased;
    }}
    .page {{
      width: min(1320px, calc(100vw - 32px));
      margin: 0 auto;
      padding: 22px 0 28px;
    }}
    .masthead {{
      display: flex;
      justify-content: space-between;
      gap: 18px;
      align-items: center;
      padding: 0 4px 18px;
    }}
    .brand {{
      display: flex;
      align-items: center;
      gap: 14px;
      min-width: 0;
    }}
    .brand-mark {{
      width: 56px;
      height: auto;
      display: block;
      flex: none;
    }}
    .brand-copy {{
      display: flex;
      flex-direction: column;
      gap: 3px;
      min-width: 0;
    }}
    .wordmark {{
      font-size: 2.2rem;
      line-height: 0.9;
      font-weight: 800;
      letter-spacing: -0.05em;
      color: var(--logo-dark);
    }}
    .wordmark span {{
      color: var(--accent);
    }}
    .product-line {{
      display: flex;
      align-items: center;
      gap: 10px;
      flex-wrap: wrap;
      color: var(--muted);
      font-size: 0.92rem;
    }}
    .process-pill {{
      display: inline-flex;
      align-items: center;
      gap: 6px;
      padding: 6px 10px;
      border-radius: 999px;
      background: var(--accent-soft);
      color: var(--accent);
      font-size: 0.76rem;
      font-weight: 700;
      letter-spacing: 0.12em;
      text-transform: uppercase;
    }}
    .topbar {{
      display: flex;
      justify-content: flex-end;
      gap: 16px;
      align-items: center;
    }}
    .status-strip {{
      display: flex;
      gap: 8px;
      flex-wrap: wrap;
      justify-content: flex-end;
    }}
    .chip {{
      border: 1px solid var(--line);
      background: var(--panel);
      color: var(--text);
      border-radius: 999px;
      padding: 8px 12px;
      font-size: 0.78rem;
      line-height: 1;
      display: inline-flex;
      gap: 6px;
      align-items: center;
      white-space: nowrap;
    }}
    .chip-label {{
      color: var(--muted);
      text-transform: uppercase;
      letter-spacing: 0.08em;
      font-size: 0.68rem;
      font-weight: 700;
    }}
    .chip-value {{
      font-weight: 700;
    }}
    .chip-value.ok {{
      color: var(--success);
    }}
    .chip-value.warn {{
      color: var(--warning);
    }}
    .workspace {{
      display: grid;
      grid-template-columns: 280px minmax(0, 1fr);
      gap: 18px;
      align-items: start;
    }}
    .sidebar,
    .shell {{
      border: 1px solid var(--line);
      background: var(--panel);
      border-radius: 24px;
      box-shadow: var(--shadow);
    }}
    .sidebar {{
      padding: 20px 18px;
      position: sticky;
      top: 22px;
    }}
    .sidebar-head {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 10px;
      margin-bottom: 18px;
    }}
    .sidebar-title {{
      font-size: 0.86rem;
      font-weight: 800;
      letter-spacing: 0.12em;
      text-transform: uppercase;
      color: var(--muted);
    }}
    .mini-pill {{
      border: 1px solid var(--line);
      border-radius: 999px;
      padding: 5px 9px;
      font-size: 0.72rem;
      color: var(--muted);
      background: var(--panel-alt);
    }}
    .history-list {{
      display: grid;
      gap: 10px;
    }}
    .history-card {{
      padding: 14px 14px 13px;
      border-radius: 18px;
      border: 1px solid var(--line);
      background: var(--panel-alt);
    }}
    .history-card.active {{
      background: linear-gradient(180deg, #f5f8ff 0%, #eef3ff 100%);
      border-color: #d7e4ff;
    }}
    .history-name {{
      font-size: 0.95rem;
      font-weight: 700;
      margin-bottom: 5px;
    }}
    .history-meta,
    .history-note,
    .meta-grid {{
      color: var(--muted);
      font-size: 0.82rem;
      line-height: 1.45;
    }}
    .history-note {{
      margin-top: 16px;
      padding: 14px;
      border-radius: 18px;
      background: var(--panel-soft);
      border: 1px dashed #d6dff0;
    }}
    .meta-grid {{
      margin-top: 18px;
      display: grid;
      gap: 8px;
      padding-top: 18px;
      border-top: 1px solid var(--line);
    }}
    .meta-grid strong {{
      color: var(--text);
      font-weight: 700;
    }}
    .shell {{
      overflow: hidden;
      min-height: calc(100vh - 110px);
      display: grid;
      grid-template-rows: auto minmax(0, 1fr) auto;
    }}
    .shell-head {{
      padding: 18px 20px;
      border-bottom: 1px solid var(--line);
      background: linear-gradient(180deg, #ffffff 0%, #fafbfd 100%);
    }}
    .shell-title-row {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 16px;
    }}
    .shell-kicker {{
      color: var(--accent);
      font-size: 0.78rem;
      font-weight: 700;
      letter-spacing: 0.1em;
      text-transform: uppercase;
      margin-bottom: 8px;
    }}
    .shell-title {{
      margin: 0;
      font-size: 1.8rem;
      line-height: 1;
      letter-spacing: -0.04em;
    }}
    .shell-subtitle {{
      margin-top: 8px;
      color: var(--muted);
      font-size: 0.98rem;
      max-width: 740px;
    }}
    .helper-chip {{
      border: 1px solid var(--line);
      border-radius: 16px;
      padding: 10px 12px;
      min-width: 190px;
      background: var(--panel-alt);
    }}
    .helper-chip strong {{
      display: block;
      font-size: 0.76rem;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
      margin-bottom: 4px;
    }}
    .helper-chip span {{
      display: block;
      font-size: 0.9rem;
      font-weight: 600;
      color: var(--text);
    }}
    .messages {{
      min-height: 58vh;
      max-height: 72vh;
      overflow: auto;
      padding: 22px 20px 10px;
      background:
        radial-gradient(circle at top right, rgba(69, 117, 220, 0.08), transparent 24%),
        linear-gradient(to bottom, #ffffff, #f8f9fb);
    }}
    .msg {{
      max-width: min(760px, 86%);
      border-radius: 20px;
      padding: 14px 16px 15px;
      margin-bottom: 14px;
      border: 1px solid var(--line);
      box-shadow: 0 8px 18px rgba(31, 42, 55, 0.04);
    }}
    .msg.user {{
      margin-left: auto;
      background: var(--bubble-user);
    }}
    .msg.architect {{
      background: var(--bubble-architect);
    }}
    .msg.system {{
      background: var(--bubble-system);
    }}
    .msg-label {{
      font-size: 0.72rem;
      font-weight: 800;
      letter-spacing: 0.12em;
      text-transform: uppercase;
      margin-bottom: 8px;
      color: var(--muted);
    }}
    .msg.user .msg-label {{
      color: var(--accent);
    }}
    .msg.architect .msg-label {{
      color: var(--logo-dark);
    }}
    .msg-body {{
      white-space: pre-wrap;
      line-height: 1.5;
      font-size: 0.96rem;
    }}
    .composer {{
      border-top: 1px solid var(--line);
      padding: 18px 20px 20px;
      background: #ffffff;
    }}
    textarea {{
      width: 100%;
      min-height: 112px;
      resize: vertical;
      border: 1px solid var(--line);
      border-radius: 18px;
      padding: 16px;
      font: inherit;
      background: var(--panel);
      color: var(--text);
      outline: none;
    }}
    textarea:focus {{
      border-color: #b8caef;
      box-shadow: 0 0 0 4px rgba(69, 117, 220, 0.12);
    }}
    .composer-row {{
      display: flex;
      justify-content: space-between;
      gap: 14px;
      align-items: center;
      margin-top: 12px;
    }}
    .hint {{
      color: var(--muted);
      font-size: 0.79rem;
      line-height: 1.45;
    }}
    button {{
      border: 0;
      background: var(--logo-dark);
      color: #ffffff;
      border-radius: 999px;
      padding: 11px 19px;
      font: inherit;
      cursor: pointer;
      font-weight: 700;
    }}
    button:disabled {{
      opacity: 0.6;
      cursor: not-allowed;
    }}
    @media (max-width: 1024px) {{
      .workspace {{
        grid-template-columns: 1fr;
      }}
      .sidebar {{
        position: static;
      }}
      .shell {{
        min-height: auto;
      }}
    }}
    @media (max-width: 760px) {{
      .page {{
        width: min(100vw - 20px, 1320px);
        padding-top: 14px;
      }}
      .masthead,
      .shell-title-row,
      .composer-row {{
        flex-direction: column;
        align-items: stretch;
      }}
      .topbar {{
        justify-content: flex-start;
      }}
      .brand-mark {{
        width: 48px;
      }}
      .wordmark {{
        font-size: 1.8rem;
      }}
      .messages {{
        padding-left: 14px;
        padding-right: 14px;
      }}
      .msg {{
        max-width: 100%;
      }}
    }}
  </style>
</head>
<body>
  <div class="page">
    <div class="masthead">
      <div class="brand">
        <svg class="brand-mark" viewBox="0 0 300 252" xmlns="http://www.w3.org/2000/svg" preserveAspectRatio="xMidYMid meet" aria-hidden="true">
          <defs>
            <clipPath id="clip-fluxbee-logo">
              <rect x="0" y="0" transform="scale(0.14648,0.14651)" width="2048" height="1720" fill="none"></rect>
            </clipPath>
          </defs>
          <g clip-path="url(#clip-fluxbee-logo)" fill="none" fill-rule="nonzero" stroke="none" stroke-width="1" stroke-linecap="butt" stroke-linejoin="miter" stroke-miterlimit="10" style="mix-blend-mode: normal">
            <path d="M267.10986,39.19186c4.25098,-0.42547 11.76855,0.06461 15.71631,1.60049c0.74268,2.29305 0.78662,5.3698 0.90527,7.58286c1.10742,20.63206 -12.15674,44.24256 -32.3833,50.8643c-1.47803,0.48422 -3.16406,0.92786 -4.60107,1.56709l-0.17725,0.08029c1.39893,1.51332 3.09814,2.91529 4.47217,4.44297c2.22217,2.30287 3.94922,5.0567 5.98096,7.49436c2.83008,3.39629 -0.94629,6.33546 -3.48486,8.83743c-4.4502,4.2947 -9.61963,6.68357 -15.5918,7.99411c-16.42383,3.60331 -32.31006,-5.18417 -45.6665,-13.7507c-4.05322,-2.59956 -7.91602,-5.67747 -12.01465,-8.35087c-0.53027,0.69124 -4.71973,6.35714 -4.1499,6.85059c2.55762,2.25408 5.30859,4.47842 7.68457,6.93293c24.5376,25.33801 19.94678,66.6499 -7.56152,87.86039c-3.66357,2.82181 -7.3125,5.28174 -10.18945,8.96651c-3.0498,3.906 -4.05469,7.93653 -5.82568,12.38756c-0.20215,0.50547 -0.40576,0.66663 -0.89209,0.819c-1.13232,-0.34723 -2.01416,-4.33528 -2.62793,-5.85753c-3.44678,-8.53577 -7.6626,-11.31949 -14.37876,-16.61295c-12.92358,-10.36863 -21.53232,-25.2586 -22.96025,-41.84958c-1.47847,-17.17849 2.57842,-30.10873 13.30239,-43.39264c2.57285,-3.00437 7.91616,-7.94928 11.54033,-9.6179c-1.33169,-2.20324 -2.73779,-3.88197 -4.3311,-5.87717c-3.03003,1.91725 -6.01611,4.36224 -9.04761,6.31949c-11.99033,7.74167 -25.35117,15.81447 -40.05674,15.9157c-10.96743,0.0756 -18.16025,-3.24846 -25.74727,-10.95614c-0.71895,-0.73051 -2.37393,-2.43341 -2.22759,-3.44976c0.61377,-4.26085 8.37935,-12.47825 11.35166,-15.13626c-2.35708,-1.30132 -5.05386,-1.88385 -7.55186,-3.09125c-9.08437,-4.39081 -15.73447,-10.3594 -20.91782,-18.98849c-6.35054,-10.57228 -9.59033,-26.19467 -6.61846,-38.22386c1.44814,-0.51235 2.85923,-0.68699 4.35879,-0.8275c28.28174,-2.65025 55.10786,10.09934 77.80093,25.77198c3.72275,2.57113 9.15278,5.68949 12.14751,9.14203c0.32739,-2.20529 1.53208,-5.84215 2.48247,-7.83412c2.91094,-6.25531 8.23594,-11.06075 14.75522,-13.31615c6.91113,-2.41671 16.31982,-1.67199 22.94092,1.43142c4.77246,2.23709 9.73975,7.30683 11.74365,12.21204c0.83203,2.0371 1.56445,4.90711 2.33936,7.14361c3.00293,-3.15132 10.5791,-8.02312 14.3877,-10.45859c5.31738,-3.39922 12.23584,-7.96965 17.84326,-10.82955c5.41113,-2.76072 12.54199,-5.653 18.24756,-7.79457c3.27979,-1.23114 8.60889,-3.33812 12.00293,-3.97984c5.34961,-1.01137 11.58838,-1.67155 16.99951,-2.02171z" fill="var(--logo-dark)"></path>
            <path d="M159.34131,115.3423c0.70166,0.13831 1.22021,0.46942 1.84131,0.79468c8.89746,4.67943 17.15771,11.70643 22.26855,20.44379c9.67236,16.53853 8.55908,37.9219 -2.45361,53.5415c-6.0249,8.54602 -12.64307,13.4527 -21.56104,18.73005l-0.271,0.08644c-0.31348,-0.0674 -0.31348,-0.06007 -0.59766,-0.22709c-13.72002,-7.97902 -24.55313,-19.41426 -28.33052,-35.24337c-3.53335,-14.92953 -0.33735,-31.70658 9.75059,-43.4719c3.9022,-4.55109 13.52036,-13.22473 19.35337,-14.65409z" fill="#ffffff"></path>
            <path d="M157.46631,143.65436c9.50391,-1.08184 18.0835,5.74941 19.16016,15.25655c1.07666,9.50567 -5.7583,18.08393 -15.26221,19.1564c-9.49951,1.071 -18.06812,-5.75791 -19.14419,-15.25772c-1.07593,-9.49981 5.74819,-18.07411 15.24624,-19.15522z" fill="var(--accent)"></path>
            <path d="M268.86035,48.40964c1.64648,-0.15633 3.73975,-0.09509 5.38623,-0.02945c1.11768,10.90647 -4.71094,23.94923 -11.68652,32.03609c-7.53809,8.73913 -19.48535,13.26839 -30.90674,12.62535c-3.85547,-0.21698 -7.53516,-0.73959 -11.36719,-1.13737c-10.5498,-0.70282 -21.07178,-1.94523 -31.65088,-1.28256c-2.84619,0.17581 -5.53125,0.66033 -8.41846,0.61813c3.00732,-2.37334 6.49805,-5.43866 9.39258,-7.59297c4.86475,-3.61986 10.32275,-7.16955 15.41895,-10.47924c13.60254,-8.83289 29.02441,-17.4425 44.63086,-21.98553c5.11084,-1.48753 13.75928,-2.29818 19.20117,-2.77244z" fill="#ffffff"></path>
            <path d="M47.75449,48.71057c2.65107,-0.2123 5.92266,-0.06754 8.59233,0.06534c11.17354,0.60963 22.31089,4.32312 32.41802,9.00666c6.43271,2.98093 12.84946,6.78598 19.05732,10.22871c2.97524,1.65001 5.902,3.41914 8.74189,5.2879c2.77969,1.8292 5.38315,3.87421 8.09121,5.8023c5.48232,3.89384 11.17764,7.58154 15.70327,12.62564c-1.09863,-0.1071 -2.24341,-0.14959 -3.34907,-0.20687c-7.97681,-0.45653 -15.73301,-1.18704 -23.78086,-0.81988c-2.27358,0.02154 -4.54819,0.50034 -6.80757,0.59542c-7.89185,0.33214 -15.91318,2.24324 -23.80313,1.55551c-3.94058,-0.34342 -8.06836,-1.28315 -11.61255,-2.92173c-14.89541,-6.88649 -22.95981,-22.86006 -23.93481,-38.6864c-0.03926,-0.63806 -0.00732,-1.65016 0.17988,-2.26844c0.06489,-0.2142 0.27495,-0.19677 0.50405,-0.26416z" fill="#ffffff"></path>
            <path d="M116.89512,100.13454c3.75439,-0.1027 7.53706,-0.0422 11.29424,-0.10974c2.3335,-0.0419 4.69819,-0.00864 6.99565,0.43323c-0.58096,0.38708 -1.1855,0.76992 -1.78271,1.13605c-5.40879,3.31585 -10.33813,7.43019 -15.91509,10.46489c-1.97285,0.96742 -3.89751,2.07973 -5.86143,3.0577c-2.62471,1.30718 -5.51953,2.92847 -8.27417,3.86703c-3.67573,1.26132 -7.52212,1.95505 -11.40674,2.05717c-8.87666,0.1846 -10.92759,-0.89504 -18.40327,-5.23603c0.83042,-1.92414 2.00039,-3.68286 3.45381,-5.19252c4.39409,-4.62361 9.17695,-6.75038 15.2877,-8.06034c8.27783,-1.77455 16.17817,-2.21745 24.61201,-2.41744z" fill="#ffffff"></path>
            <path d="M188.50635,100.13791c2.8125,-0.01084 5.65869,0.16805 8.47559,0.17992c5.46533,0.023 10.91455,-0.31515 16.3667,0.20438c12.79248,1.21868 26.56641,2.57392 33.70752,14.87811c-1.34619,1.60753 -3.25049,2.65025 -5.15186,3.46134c-3.91406,1.49969 -6.44531,1.91637 -10.53955,2.19196c-8.28516,0.55733 -15.55078,-2.64073 -22.70654,-6.44241c-2.78174,-1.4783 -5.68359,-2.6608 -8.2998,-4.43505c-2.82568,-1.89733 -5.70996,-3.73385 -8.50928,-5.66736c-2.19727,-1.5164 -4.62744,-2.59677 -6.76465,-4.17426c1.13379,-0.13391 2.28223,-0.1556 3.42188,-0.19662z" fill="#ffffff"></path>
            <path d="M159.8584,23.08188c1.15283,0.91614 4.39746,7.95954 5.12402,9.44194c0.6665,1.35904 1.72119,3.17681 2.47998,4.54494c2.44189,-1.27626 4.72266,-2.83647 7.14111,-4.13207c2.16943,-1.16257 4.53662,-2.16105 6.77197,-3.22062c-2.66455,3.35248 -5.24561,10.24146 -6.97705,14.2903c-0.49219,1.15129 -0.84814,3.60287 -1.50879,4.60354c-0.81445,0.21801 -3.82031,-0.79732 -4.82227,-1.06323c-2.39502,-0.6574 -4.85596,-1.04683 -7.33594,-1.1611c-3.31787,-0.07179 -6.43799,0.60861 -9.66064,1.31421c-1.02832,0.22504 -2.58984,1.12697 -3.39111,0.98676c-0.40576,-0.88933 -0.75293,-2.43854 -0.99609,-3.4417c-1.37139,-5.66414 -4.43774,-10.5084 -6.79819,-15.77872c0.73916,0.48876 1.47524,0.92434 2.25586,1.3419c3.7853,2.02494 7.44712,4.27492 11.1356,6.47625c0.58447,-1.59156 1.80322,-3.86864 2.52539,-5.49199c1.29785,-2.921 2.55908,-5.89226 4.05615,-8.71041z" fill="var(--logo-dark)"></path>
          </g>
        </svg>
        <div class="brand-copy">
          <div class="wordmark">flux<span>bee</span></div>
          <div class="product-line">
            <span class="process-pill">SY.architect</span>
            <span>archi · system architect interface</span>
          </div>
        </div>
      </div>
      <div class="topbar">
        <div class="status-strip">
          <div class="chip"><span class="chip-label">Router</span><span id="router" class="chip-value">{router}</span></div>
          <div class="chip"><span class="chip-label">AI</span><span id="ai" class="chip-value">{ai}</span></div>
          <div class="chip"><span class="chip-label">Hive</span><span class="chip-value">{hive}</span></div>
          <div class="chip"><span class="chip-label">Bind</span><span class="chip-value">{listen}</span></div>
        </div>
      </div>
    </div>
    <div class="workspace">
      <aside class="sidebar">
        <div class="sidebar-head">
          <div class="sidebar-title">Chat History</div>
          <div class="mini-pill">local</div>
        </div>
        <div class="history-list">
          <div class="history-card active">
            <div id="session-title" class="history-name">Current session</div>
            <div id="session-meta" class="history-meta">archi ready · waiting for first message</div>
          </div>
        </div>
        <div class="history-note">
          LanceDB-backed history is not wired yet. This rail is reserved for the session index the spec describes, so the layout already leaves that space in place.
        </div>
        <div class="meta-grid">
          <div><strong>Node</strong>: {node}</div>
          <div><strong>Listen</strong>: {listen}</div>
          <div><strong>Modes</strong>: chat, FCMD, ACMD</div>
        </div>
      </aside>
      <div class="shell">
        <div class="shell-head">
          <div class="shell-title-row">
            <div>
              <div class="shell-kicker">System Architect Terminal</div>
              <h1 class="shell-title">archi</h1>
              <div class="shell-subtitle">
                Fluxbee control surface for system design, prompt operations, and admin passthrough.
                The top status bar can grow without changing the composition, matching the spec direction.
              </div>
            </div>
            <div class="helper-chip">
              <strong>Quick Start</strong>
              <span>Use `FCMD:` for local prompt control or `ACMD:` for admin reads and actions.</span>
            </div>
          </div>
        </div>
        <div id="messages" class="messages"></div>
        <div class="composer">
          <textarea id="input" placeholder="Message, FCMD: {{...}}, or ACMD: curl -X GET /hives/{hive}/nodes"></textarea>
          <div class="composer-row">
            <div class="hint">Enter to send. Shift+Enter for newline. Chat history on the left is layout-ready even though persistent sessions are still pending.</div>
            <button id="send">Send</button>
          </div>
        </div>
      </div>
    </div>
  </div>
  <script>
    const rawPath = window.location.pathname.replace(/\/+$/, "");
    const base = rawPath === "" ? "" : rawPath;
    const statusUrl = (base || "") + "/api/status";
    const chatUrl = (base || "") + "/api/chat";
    const messages = document.getElementById("messages");
    const input = document.getElementById("input");
    const send = document.getElementById("send");
    const sessionTitle = document.getElementById("session-title");
    const sessionMeta = document.getElementById("session-meta");
    let userMessageCount = 0;

    function formatBoolChip(elementId, activeText, inactiveText, value) {{
      const element = document.getElementById(elementId);
      if (!element) return;
      element.textContent = value ? activeText : inactiveText;
      element.classList.remove("ok", "warn");
      element.classList.add(value ? "ok" : "warn");
    }}

    function updateSession(text) {{
      const compact = text.replace(/\s+/g, " ").trim();
      if (!compact) return;
      userMessageCount += 1;
      if (userMessageCount === 1) {{
        sessionTitle.textContent = compact.slice(0, 42) || "Current session";
      }}
      sessionMeta.textContent = userMessageCount === 1
        ? "first message captured locally"
        : userMessageCount + " local messages in this session";
    }}

    function addMessage(kind, text) {{
      const div = document.createElement("div");
      const label = document.createElement("div");
      const body = document.createElement("div");
      const labels = {{ user: "Operator", architect: "archi", system: "System" }};
      div.className = "msg " + kind;
      label.className = "msg-label";
      label.textContent = labels[kind] || "Message";
      body.className = "msg-body";
      body.textContent = text;
      div.appendChild(label);
      div.appendChild(body);
      messages.appendChild(div);
      messages.scrollTop = messages.scrollHeight;
    }}
    async function refreshStatus() {{
      try {{
        const res = await fetch(statusUrl);
        const data = await res.json();
        formatBoolChip("router", "connected", "offline", !!data.router_connected);
        formatBoolChip("ai", "configured", "missing", !!data.ai_configured);
      }} catch (_err) {{}}
    }}
    async function submit() {{
      const message = input.value.trim();
      if (!message) return;
      addMessage("user", message);
      updateSession(message);
      input.value = "";
      send.disabled = true;
      try {{
        const res = await fetch(chatUrl, {{
          method: "POST",
          headers: {{ "Content-Type": "application/json" }},
          body: JSON.stringify({{ message }})
        }});
        const data = await res.json();
        addMessage(data.status === "ok" ? "architect" : "system", JSON.stringify(data.output, null, 2));
      }} catch (err) {{
        addMessage("system", "Request failed: " + err);
      }} finally {{
        send.disabled = false;
        refreshStatus();
      }}
    }}
    send.addEventListener("click", submit);
    input.addEventListener("keydown", (event) => {{
      if (event.key === "Enter" && !event.shiftKey) {{
        event.preventDefault();
        submit();
      }}
    }});
    addMessage("system", "Fluxbee architect interface ready. Prompt control is local through FCMD, and admin passthrough is available through ACMD.");
    addMessage("architect", "I am archi. AI conversation is still pending, but the control-plane shell and prompt operations are live.");
    addMessage("system", "Example: FCMD: {{\"op\":\"prompt.get\",\"prompt_id\":\"architect\"}} or ACMD: curl -X GET /hives/{hive}/nodes");
    refreshStatus();
    setInterval(refreshStatus, 5000);
  </script>
</body>
</html>"##,
        node = state.node_name,
        hive = state.hive_id,
        listen = state.listen,
        router = state.router_connected.load(Ordering::Relaxed),
        ai = state.ai_configured.load(Ordering::Relaxed),
    )
}
