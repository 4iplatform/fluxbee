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
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>SY.architect</title>
  <style>
    :root {{
      color-scheme: light dark;
      --bg: #ffffff;
      --panel: #ffffff;
      --panel-2: #f7f7f8;
      --text: #1f1f1f;
      --muted: #6b6b73;
      --line: #e7e7ea;
      --accent: #111111;
      --bubble-user: #f3f4f6;
      --bubble-system: #fafafb;
      --shadow: 0 12px 32px rgba(0, 0, 0, 0.06);
    }}
    @media (prefers-color-scheme: dark) {{
      :root {{
        --bg: #0f0f10;
        --panel: #171718;
        --panel-2: #1d1d1f;
        --text: #f3f3f4;
        --muted: #a0a0a8;
        --line: #2a2a2d;
        --accent: #ffffff;
        --bubble-user: #232326;
        --bubble-system: #1b1b1d;
        --shadow: 0 12px 32px rgba(0, 0, 0, 0.28);
      }}
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: var(--bg);
      color: var(--text);
    }}
    .page {{
      width: min(980px, calc(100vw - 32px));
      margin: 0 auto;
      padding: 24px 0 32px;
    }}
    .topbar {{
      display: flex;
      justify-content: space-between;
      gap: 16px;
      align-items: flex-start;
      padding: 0 4px 18px;
    }}
    .title {{
      font-size: 28px;
      font-weight: 650;
      letter-spacing: -0.02em;
      margin: 0 0 6px;
    }}
    .meta {{
      color: var(--muted);
      font-size: 13px;
    }}
    .status {{
      display: flex;
      gap: 8px;
      flex-wrap: wrap;
    }}
    .chip {{
      border: 1px solid var(--line);
      background: var(--panel);
      color: var(--muted);
      border-radius: 999px;
      padding: 8px 12px;
      font-size: 12px;
      line-height: 1;
    }}
    .shell {{
      border: 1px solid var(--line);
      background: var(--panel);
      border-radius: 20px;
      box-shadow: var(--shadow);
      overflow: hidden;
    }}
    .messages {{
      min-height: 56vh;
      max-height: 70vh;
      overflow: auto;
      padding: 20px;
      background: linear-gradient(to bottom, var(--panel), var(--panel-2));
    }}
    .msg {{
      max-width: 78%;
      border-radius: 18px;
      padding: 14px 16px;
      margin-bottom: 12px;
      white-space: pre-wrap;
      line-height: 1.45;
      border: 1px solid var(--line);
    }}
    .msg.user {{
      margin-left: auto;
      background: var(--bubble-user);
    }}
    .msg.architect {{
      background: var(--panel);
    }}
    .msg.system {{
      background: var(--bubble-system);
      color: var(--muted);
    }}
    .composer {{
      border-top: 1px solid var(--line);
      padding: 16px;
      background: var(--panel);
    }}
    textarea {{
      width: 100%;
      min-height: 110px;
      resize: vertical;
      border: 1px solid var(--line);
      border-radius: 16px;
      padding: 16px;
      font: inherit;
      background: var(--panel);
      color: var(--text);
      outline: none;
    }}
    textarea:focus {{
      border-color: var(--muted);
    }}
    .composer-row {{
      display: flex;
      justify-content: space-between;
      gap: 12px;
      align-items: center;
      margin-top: 12px;
    }}
    .hint {{
      color: var(--muted);
      font-size: 12px;
    }}
    button {{
      border: 0;
      background: var(--accent);
      color: var(--bg);
      border-radius: 999px;
      padding: 10px 18px;
      font: inherit;
      cursor: pointer;
    }}
    button:disabled {{
      opacity: 0.6;
      cursor: not-allowed;
    }}
  </style>
</head>
<body>
  <div class="page">
    <div class="topbar">
      <div>
        <h1 class="title">SY.architect</h1>
        <div class="meta">Node: {node} · Hive: {hive} · Listen: {listen}</div>
      </div>
      <div class="status">
        <div class="chip">Router: <span id="router">{router}</span></div>
        <div class="chip">AI: <span id="ai">{ai}</span></div>
      </div>
    </div>
    <div class="shell">
      <div id="messages" class="messages">
        <div class="msg system">Architect scaffold ready. Use FCMD for prompt control or ACMD for admin passthrough.</div>
        <div class="msg system">Examples: FCMD: {{"op":"prompt.get","prompt_id":"architect"}} or ACMD: curl -X GET /hives/{hive}/nodes</div>
      </div>
      <div class="composer">
        <textarea id="input" placeholder="Message, FCMD: {{...}}, or ACMD: curl -X GET /hives/{hive}/nodes"></textarea>
        <div class="composer-row">
          <div class="hint">Enter to send. Shift+Enter for newline.</div>
          <button id="send">Send</button>
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
    function addMessage(kind, text) {{
      const div = document.createElement("div");
      div.className = "msg " + kind;
      div.textContent = text;
      messages.appendChild(div);
      messages.scrollTop = messages.scrollHeight;
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
      addMessage("user", message);
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
