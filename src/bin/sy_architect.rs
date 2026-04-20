use std::collections::{hash_map::DefaultHasher, BTreeMap, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arrow_array::{Array, RecordBatch, RecordBatchIterator, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::any;
use axum::{Json, Router};
use fluxbee_ai_sdk::{
    build_openai_user_content_parts, extract_text, resolve_model_input_from_payload_with_options,
    ConversationSummary, FunctionCallingConfig, FunctionCallingRunner, FunctionRunInput,
    FunctionTool, FunctionToolDefinition, FunctionToolProvider, FunctionToolRegistry,
    ImmediateConversationMemory, ImmediateInteraction, ImmediateInteractionKind,
    ImmediateOperation, ImmediateRole, ModelInputOptions, ModelSettings, OpenAiResponsesClient,
};
use fluxbee_sdk::blob::{BlobConfig, BlobRef, BlobToolkit};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use fluxbee_sdk::{
    admin_command, build_node_secret_record, connect, list_ich_options_from_hive_config,
    load_node_secret_record_with_root, redacted_node_secret_record,
    save_node_secret_record_with_root, AdminCommandRequest, IdentityIchOption, NodeConfig,
    NodeError, NodeReceiver, NodeSecretDescriptor, NodeSecretError, NodeSecretRecord,
    NodeSecretWriteOptions, NodeSender, NODE_SECRET_REDACTION_TOKEN,
};
use futures::TryStreamExt;
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type ArchitectError = Box<dyn std::error::Error + Send + Sync>;

const DEFAULT_ARCHITECT_LISTEN: &str = "127.0.0.1:3000";
const DEFAULT_ARCHITECT_MODEL: &str = "gpt-5.4-mini";
const ROUTER_RECONNECT_DELAY_SECS: u64 = 2;
const CHAT_SESSIONS_TABLE: &str = "sessions";
const CHAT_MESSAGES_TABLE: &str = "messages";
const CHAT_OPERATIONS_TABLE: &str = "operations";
const CHAT_SESSION_PROFILES_TABLE: &str = "session_profiles";
const CHAT_MODE_OPERATOR: &str = "operator";
const CHAT_MODE_IMPERSONATION: &str = "impersonation";
const ARCHITECT_LOCAL_SECRET_KEY_OPENAI: &str = "openai_api_key";
const ARCHITECT_MAX_ATTACHMENTS: usize = 8;
const ARCHITECT_MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const ARCHITECT_MAX_ATTACHMENT_UPLOAD_BYTES: usize =
    ARCHITECT_MAX_ATTACHMENTS * ARCHITECT_MAX_ATTACHMENT_BYTES + (2 * 1024 * 1024);
const ARCHI_SYSTEM_PROMPT: &str = r#"You are archi, the Fluxbee system architect.

Operate as a concise technical assistant for the Fluxbee control plane.

Rules:
- Be direct and operational.
- Prefer short concrete answers over long explanations.
- Keep host/executor responsibilities separate. Normal Archi chat handles reads, SCMD, and staged writes. `executor_plan` execution is a separate host path and is not the same as normal chat mutation staging.
- If the user asks about system state, deployments, nodes, hives, identity, or operations, prefer SCMD/system operations when available.
- If you are unsure which Fluxbee action or path exists, inspect `/admin/actions` or `/admin/actions/{action}` before answering.
- The admin help endpoint includes a standardized request contract with path params, body fields, notes, and example SCMD. Use it instead of guessing payloads.
- Treat `path_patterns` from admin help as templates/documentation, not as literal executable paths.
- If admin help provides `example_scmd`, use that command shape as the primary execution scaffold.
- Never execute a path containing placeholders such as `{hive}` or `{node_name}` without replacing them with concrete values first.
- If both `example_scmd` and `request_contract` are present, prefer `example_scmd` for the path/body shape and use `request_contract` only to validate fields or extend the payload.
- Use the available read-only system tool when you need live Fluxbee state instead of guessing.
- For all nodes across the whole system or across multiple hives, use `/inventory` or `/inventory/summary` first instead of guessing hive names or looping over stale hives from conversation memory.
- Use `/hives/{hive}/nodes` only for one explicit hive.
- Distinguish runtime names from node instance names: `AI.chat` is a runtime/package; `AI.chat@motherbee` is a node instance.
- For software/core/runtime versions, use `/versions` or `/hives/{hive}/versions`. Do not infer versions from `/hives/{hive}/nodes`.
- When the operator asks for runtime/package info such as `AI.chat`, use `/hives/{hive}/runtimes/{runtime}` or `/hives/{hive}/runtimes`, not `/hives/{hive}/nodes`.
- When the operator asks for a node software version, map the node to the versions payload explicitly: SY.identity@hive -> core.components['sy-identity'].version; AI.chat@hive -> runtimes.runtimes['AI.chat'].current; IO.slack.T123@hive -> runtimes.runtimes['IO.slack'].current.
- For hive-scoped deployments or drift alerts, use `/hives/{hive}/deployments` or `/hives/{hive}/drift-alerts`. Do not synthesize or locally filter a hive-specific answer from `/deployments` or `/drift-alerts` when the hive endpoint exists.
- If a hive-specific endpoint returns an empty list, report exactly that it returned no recorded entries for that hive. Do not invent filtered rows from broader results.
- For drift alerts specifically, if `/hives/{hive}/drift-alerts` returns `entries: []`, answer that there are no recorded drift alerts for that hive. Do not infer drift from `/deployments`, `/versions`, `/nodes`, or from the global `/drift-alerts` list.
- Distinguish stored node config from live node control-plane config: `GET /hives/{hive}/nodes/{node_name}/config` reads persisted effective `config.json`; `POST /hives/{hive}/nodes/{node_name}/control/config-get` asks the node for live CONFIG_GET.
- If the operator asks for the current prompt, instructions, or config content of a managed node and `GET .../config` is available, try `GET .../config` first. Use `POST .../control/config-get` only when you need the node-defined live contract or the GET config path is unavailable.
- For live node-defined config contracts on nodes that expose CONFIG_GET, including `SY.storage`, `SY.identity`, `SY.cognition`, and `WF.*`, use `POST /hives/{hive}/nodes/{node_name}/control/config-get`. For live node-defined config updates, use `POST /hives/{hive}/nodes/{node_name}/control/config-set`.
- For `WF.*`, treat `CONFIG_SET` as boot-time only unless the node explicitly says otherwise in its `CONFIG_RESPONSE`. `WF v1` persists config and returns `restart_required`; it does not hot-apply `CONFIG_CHANGED`.
- For `SY.cognition`, prefer live CONFIG_GET/SET when the operator asks about semantic_tagger config, degraded semantics policy, or AI secret setup. The canonical secret field is `config.secrets.openai.api_key`; semantic controls live under `config.semantic_tagger.*`.
- For local architect OpenAI bootstrap, use `GET /architect/control/config-get` and `POST /architect/control/config-set`.
- If the operator provides a structured `executor_plan` or explicitly asks to run one, do not decompose it into SCMD or staged write tools unless they explicitly ask for that translation. Treat it as a host-level execution artifact that belongs in the dedicated executor-plan path.
- Do not suggest `CONFIRM` or `CANCEL` for executor-plan mode. That confirmation model belongs only to normal staged writes in chat.
- For `wf_rules_list_workflows`, use `GET /hives/{hive}/wf-rules` with a concrete hive id and no `workflow_name`.
- For `wf_rules_get_workflow`, use `GET /hives/{hive}/wf-rules?workflow_name=...`.
- For `wf_rules_get_status`, use `GET /hives/{hive}/wf-rules/status?workflow_name=...`.
- WF `workflow_name` must match `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$` — lowercase, digits, hyphens, dot-segments. Underscores are rejected. Convert snake_case names to kebab-case (e.g. `response_dispatch` → `response-dispatch`).
- When building a WF workflow `definition` object for `wf_rules_compile_apply` or `wf_rules_compile`, the required schema is:
  - Top-level: `wf_schema_version: "1"`, `workflow_type` (string, matches workflow_name), `description`, `input_schema` (JSON Schema of the event that creates instances), `initial_state`, `terminal_states: [...]`, `states: [...]`.
  - Each state: `name`, `description`, `entry_actions: []`, `exit_actions: []`, `transitions: []`.
  - Action types: `send_message` (fields: `type`, `target` as node instance name, `meta: {"msg":"...", "type":"..."}`, `payload`), `schedule_timer` (fields: `type`, `timer_key`, `fire_in` as duration string like `"10m"`, `missed_policy` as `"fire"` or `"drop"`), `cancel_timer` (fields: `type`, `timer_key`), `reschedule_timer` (fields: `type`, `timer_key`, `fire_in`), `set_variable` (fields: `type`, `name`, `value` as CEL expression string).
  - `send_message` critical: `msg` and `type` go INSIDE the `meta` object — never as top-level fields. There is no `type_meta` field. Unknown top-level fields cause a compile error.
  - In `send_message` payload, use `{ "$ref": "input.field" }`, `{ "$ref": "state.field" }`, or `{ "$ref": "event.field" }` for runtime values. Plain JSON values are literals.
  - Each transition: `event_match: {"msg":"..."}`, optional `guard` (CEL expression), `target_state`, `actions: []`.
  - CEL guard variables: `input` (creation event payload), `state` (workflow variables set by set_variable), `event` (current message — `event.payload` for user data, `event.user_payload` for timer metadata).
  - `tenant_id` is required on the first deploy when `auto_spawn: true` and the `WF.<name>@<hive>` node does not yet exist. Look it up from existing nodes or ask the operator.
- For mutations, use the write tool only to stage the action. Then instruct the operator to reply CONFIRM or CANCEL. Do not claim the mutation ran before confirmation.
- When calling the write tool for actions that require a body, always include the complete body in the tool call. For `wf_rules_compile_apply`, build and embed the full `definition` object directly in the body argument — never omit it, never ask the user to paste it separately. If the definition comes from an attachment, construct it from that content and pass it inline.
- Use specialized write tools for large-body mutations instead of `fluxbee_system_write`:
  - `fluxbee_deploy_workflow` — for `wf_rules_compile_apply` when passing a full workflow `definition`. Pass the definition object in `definition`, or if that is not possible, serialize it to a JSON string and pass it in `definition_json`.
  - `fluxbee_deploy_opa_policy` — for `opa_compile_apply` when passing a full OPA `rego` source string.
  - `fluxbee_set_node_config` — for `node_control_config_set` when passing a large node `config` object. Pass the config in `config`, or serialize to a JSON string and pass in `config_json`. Always do CONFIG_GET first to read the current `config_version`.
  - Use `fluxbee_system_write` only for mutations whose body is small and fits cleanly as an inline JSON object (route adds, vpn adds, kill_node, rollback, delete, etc.).
- Do not claim actions were executed unless they actually were.
- If information is missing, say what is missing.
- Keep answers useful for administrators and developers."#;
const FAVICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="16" fill="#ffffff"/>
  <path d="M18 33c0-8 6.5-14.5 14.5-14.5S47 25 47 33s-6.5 14.5-14.5 14.5S18 41 18 33Z" fill="#0070F3" opacity="0.14"/>
  <path d="M21 31.5c1.4-6.4 6.2-10.5 11.7-10.5 5.7 0 10.5 4.3 11.6 10.5-1.1 6.8-5.8 11.5-11.6 11.5-5.9 0-10.7-4.8-11.7-11.5Z" fill="#1f2a37"/>
  <circle cx="32.5" cy="33" r="7.5" fill="#ffffff"/>
  <circle cx="32.5" cy="33" r="4.2" fill="#0070F3"/>
  <path d="M24 22l4.8 5.4" stroke="#1f2a37" stroke-width="3.2" stroke-linecap="round"/>
  <path d="M41 22l-4.8 5.4" stroke="#1f2a37" stroke-width="3.2" stroke-linecap="round"/>
</svg>"##;

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    architect: Option<ArchitectSection>,
    ai_providers: Option<AiProvidersSection>,
    blob: Option<BlobSection>,
}

#[derive(Debug, Deserialize)]
struct ArchitectSection {
    listen: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlobSection {
    path: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct AiProvidersSection {
    openai: Option<OpenAiSection>,
}

#[derive(Debug, Deserialize, Clone)]
struct OpenAiSection {
    api_key: Option<String>,
    default_model: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct ArchitectNodeConfigFile {
    ai_providers: Option<AiProvidersSection>,
}

#[derive(Clone)]
struct ArchitectAiRuntime {
    model: String,
    instructions: String,
    model_settings: ModelSettings,
    client: OpenAiResponsesClient,
}

#[derive(Clone)]
struct ArchitectAdminToolContext {
    hive_id: String,
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    session_id: Option<String>,
    chat_lock: Arc<Mutex<()>>,
    pending_actions: Arc<Mutex<HashMap<String, PendingAdminAction>>>,
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
    ai_runtime: Arc<Mutex<Option<ArchitectAiRuntime>>>,
    chat_lock: Arc<Mutex<()>>,
    pending_actions: Arc<Mutex<HashMap<String, PendingAdminAction>>>,
    router_sender: Arc<Mutex<Option<NodeSender>>>,
    cached_status: Arc<RwLock<ArchitectStatus>>,
}

#[derive(Debug, Clone, Serialize)]
struct ArchitectStatus {
    status: String,
    hive_id: String,
    node_name: String,
    router_connected: bool,
    admin_available: bool,
    inventory_updated_at: Option<u64>,
    total_hives: Option<u32>,
    hives_alive: Option<u32>,
    hives_stale: Option<u32>,
    total_nodes: Option<u32>,
    nodes_by_status: BTreeMap<String, u32>,
    components: Vec<ArchitectComponentStatus>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ArchitectComponentStatus {
    key: String,
    label: String,
    status: String,
    node_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatRequest {
    session_id: Option<String>,
    message: String,
    #[serde(default)]
    attachments: Vec<ChatAttachmentRef>,
}

#[derive(Debug, Deserialize)]
struct ExecutorPlanRequest {
    session_id: Option<String>,
    execution_id: Option<String>,
    title: Option<String>,
    plan: Value,
    #[serde(default)]
    executor_options: Value,
    #[serde(default)]
    labels: Value,
}

#[derive(Debug, Serialize)]
struct ChatResponse {
    status: String,
    mode: String,
    output: Value,
    session_id: Option<String>,
    session_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ChatAttachmentRef {
    attachment_id: String,
    blob_ref: BlobRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UploadedAttachment {
    attachment_id: String,
    filename: String,
    mime: String,
    size: u64,
    blob_ref: BlobRef,
}

#[derive(Debug, Serialize)]
struct AttachmentUploadResponse {
    status: String,
    attachments: Vec<UploadedAttachment>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    title: Option<String>,
    chat_mode: Option<String>,
    effective_ich_id: Option<String>,
    effective_ilk: Option<String>,
    impersonation_target: Option<String>,
    thread_id: Option<String>,
    source_channel_kind: Option<String>,
    debug_enabled: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
struct SessionSummary {
    session_id: String,
    title: String,
    agent: String,
    created_at_ms: u64,
    last_activity_at_ms: u64,
    message_count: u64,
    last_message_preview: Option<String>,
    chat_mode: String,
    effective_ich_id: Option<String>,
    effective_ilk: Option<String>,
    impersonation_target: Option<String>,
    thread_id: Option<String>,
    source_channel_kind: Option<String>,
    debug_enabled: bool,
}

#[derive(Debug, Serialize)]
struct SessionListResponse {
    sessions: Vec<SessionSummary>,
}

#[derive(Debug, Serialize)]
struct SessionDetailResponse {
    session: SessionSummary,
    messages: Vec<PersistedChatMessage>,
}

#[derive(Debug, Serialize)]
struct SessionDeleteResponse {
    status: String,
    deleted_sessions: u64,
    deleted_messages: u64,
}

#[derive(Debug, Serialize)]
struct IdentityIchOptionsResponse {
    options: Vec<IdentityIchOption>,
}

#[derive(Debug, Serialize, Clone)]
struct PersistedChatMessage {
    message_id: String,
    session_id: String,
    role: String,
    content: String,
    timestamp_ms: u64,
    mode: String,
    metadata: Value,
}

#[derive(Debug, Clone)]
struct ChatSessionRecord {
    session_id: String,
    title: String,
    agent: String,
    created_at_ms: u64,
    last_activity_at_ms: u64,
    message_count: u64,
    last_message_preview: String,
    chat_mode: String,
    effective_ich_id: String,
    effective_ilk: String,
    impersonation_target: String,
    thread_id: String,
    source_channel_kind: String,
    debug_enabled: bool,
}

#[derive(Debug, Clone)]
struct ChatSessionProfileRecord {
    session_id: String,
    chat_mode: String,
    effective_ich_id: String,
    effective_ilk: String,
    impersonation_target: String,
    thread_id: String,
    source_channel_kind: String,
    debug_enabled: bool,
}

#[derive(Debug, Clone)]
struct ChatMessageRecord {
    message_id: String,
    session_id: String,
    role: String,
    content: String,
    timestamp_ms: u64,
    mode: String,
    metadata_json: String,
    seq: u64,
}

#[derive(Debug, Clone)]
struct ChatOperationRecord {
    operation_id: String,
    session_id: String,
    scope_id: String,
    origin: String,
    action: String,
    target_hive: String,
    params_json: String,
    params_hash: String,
    preview_command: String,
    status: String,
    created_at_ms: u64,
    updated_at_ms: u64,
    dispatched_at_ms: u64,
    completed_at_ms: u64,
    request_id: String,
    trace_id: String,
    error_summary: String,
}

#[derive(Debug)]
struct ParsedScmd {
    method: String,
    path: String,
    body: Option<Value>,
}

#[derive(Debug, Clone)]
struct AdminTranslation {
    admin_target: String,
    action: String,
    target_hive: String,
    params: Value,
}

#[derive(Debug, Clone)]
struct PendingAdminAction {
    operation_id: String,
    translation: AdminTranslation,
    preview_command: String,
    created_at_ms: u64,
}

struct ArchitectAdminReadToolsProvider {
    context: ArchitectAdminToolContext,
}

impl ArchitectAdminReadToolsProvider {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

impl FunctionToolProvider for ArchitectAdminReadToolsProvider {
    fn register_tools(&self, registry: &mut FunctionToolRegistry) -> fluxbee_ai_sdk::Result<()> {
        registry.register(Arc::new(ArchitectSystemGetTool::new(self.context.clone())))?;
        registry.register(Arc::new(ArchitectSystemWriteTool::new(
            self.context.clone(),
        )))?;
        registry.register(Arc::new(ArchitectDeployWorkflowTool::new(
            self.context.clone(),
        )))?;
        registry.register(Arc::new(ArchitectDeployOpaPolicyTool::new(
            self.context.clone(),
        )))?;
        registry.register(Arc::new(ArchitectSetNodeConfigTool::new(
            self.context.clone(),
        )))
    }
}

struct ArchitectSystemGetTool {
    context: ArchitectAdminToolContext,
}

impl ArchitectSystemGetTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

struct ArchitectSystemWriteTool {
    context: ArchitectAdminToolContext,
}

impl ArchitectSystemWriteTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl FunctionTool for ArchitectSystemGetTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_system_get".to_string(),
            description: format!(
                "Read live Fluxbee system state through SY.admin over socket for hive {hive}. Read-only. Supports GET paths and safe POST checks such as OPA policy validation or node CONFIG_GET control-plane discovery, including `SY.storage`, `SY.identity`, `SY.cognition`, `WF.*`, and read-only `SY.timer` operations. Use /admin/actions or /admin/actions/{{action}} when you need dynamic help; those responses include standardized request_contract metadata, body fields, notes, and example_scmd values. Treat path_patterns as templates only. Prefer example_scmd for real execution shape, and never execute placeholder paths like /hives/{{hive}}/... literally. Distinguish runtime names from node instances: `AI.chat` is a runtime, while `AI.chat@{hive}` is a node instance. Distinguish stored node config from live node control-plane config: GET /hives/{hive}/nodes/{{node_name}}/config reads persisted effective config.json, while POST /hives/{hive}/nodes/{{node_name}}/control/config-get asks the node for live CONFIG_GET. If the operator asks for current prompt/instructions/config content, prefer GET .../config first when available. For `WF.*`, use GET .../config when you want the persisted effective config or package metadata, and use POST .../control/config-get when you need the live node-defined contract. `WF v1` accepts CONFIG_SET but applies it only after restart. For workflow rules: list = GET /hives/{hive}/wf-rules, get one workflow = GET /hives/{hive}/wf-rules?workflow_name=..., get workflow status = GET /hives/{hive}/wf-rules/status?workflow_name=.... Use /inventory or /inventory/summary for system-wide node visibility, use /hives/{hive}/nodes for one-hive node lists, and do not invent /inventory/nodes as a canonical path. Use /versions or /hives/{hive}/versions for core and runtime versions, use /hives/{hive}/runtimes or /hives/{hive}/runtimes/AI.chat for runtime/package info, and use /hives/{hive}/deployments or /hives/{hive}/drift-alerts for hive-scoped views instead of locally filtering global lists. `SY.timer` read-only endpoints available through admin are /hives/{hive}/timer/help, /hives/{hive}/timer/timers, /hives/{hive}/timer/timers/{{timer_uuid}}, /hives/{hive}/timer/now, /hives/{hive}/timer/now-in, /hives/{hive}/timer/convert, /hives/{hive}/timer/parse, and /hives/{hive}/timer/format. Scheduling and owner-bound timer mutations are not exposed through admin in v1. If a hive endpoint returns zero entries, report zero entries instead of inventing filtered results. For drift alerts specifically, `entries: []` means no recorded drift alerts for that hive; do not infer drift from `/deployments`, `/versions`, `/nodes`, or the global `/drift-alerts` list. Example paths: /inventory, /inventory/summary, /inventory/{hive}, /versions, /hives/{hive}/versions, /hives/{hive}/runtimes, /hives/{hive}/runtimes/AI.chat, /hives/{hive}/deployments, /hives/{hive}/drift-alerts, /hives/{hive}/nodes, /hives/{hive}/nodes/AI.chat@{hive}/config, /hives/{hive}/nodes/WF.invoice@{hive}/config, /hives/{hive}/nodes/WF.invoice@{hive}/control/config-get, /hives/{hive}/nodes/WF.invoice@{hive}/control/config-set, /hives/{hive}/nodes/SY.admin@{hive}/status, /hives/{hive}/nodes/SY.storage@{hive}/control/config-get, /hives/{hive}/nodes/SY.storage@{hive}/control/config-set, /hives/{hive}/nodes/SY.identity@{hive}/control/config-get, /hives/{hive}/nodes/SY.identity@{hive}/control/config-set, /hives/{hive}/timer/help, /hives/{hive}/timer/timers, /hives/{hive}/timer/timers/{{timer_uuid}}, /hives/{hive}/timer/now, /admin/actions, /admin/actions/get_node_status, /admin/actions/get_versions, /admin/actions/wf_rules_list_workflows, /admin/actions/wf_rules_get_status, /config/storage",
                hive = self.context.hive_id,
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST"],
                        "description": "HTTP-like read method. Defaults to GET. POST is only for safe read/check operations."
                    },
                    "path": {
                        "type": "string",
                        "description": "Read-only Fluxbee path, for example /inventory, /versions, /hives/motherbee/nodes, or /admin/actions"
                    },
                    "body": {
                        "type": "object",
                        "description": "Optional JSON object for safe POST checks such as /hives/{hive}/opa/policy/check or /hives/{hive}/nodes/{node_name}/control/config-get"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let method = arguments
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_system_get requires a non-empty 'path'".to_string(),
                )
            })?;
        let body = arguments.get("body").cloned();
        if method != "GET" && method != "POST" {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(
                "fluxbee_system_get only supports GET or POST".to_string(),
            ));
        }
        if method == "GET" && body.is_some() {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(
                "fluxbee_system_get does not accept a body for GET requests".to_string(),
            ));
        }

        let parsed =
            parse_scmd(&scmd_raw_from_parts(&method, path, body.as_ref())).map_err(|err| {
                fluxbee_ai_sdk::AiSdkError::Protocol(format!("invalid system get path: {err}"))
            })?;
        let translation = translate_scmd(&self.context.hive_id, parsed).map_err(|err| {
            fluxbee_ai_sdk::AiSdkError::Protocol(format!("unsupported system get path: {err}"))
        })?;
        let output =
            execute_admin_translation_with_context(&self.context, translation, "tool.read")
                .await
                .map_err(|err| {
                    fluxbee_ai_sdk::AiSdkError::Protocol(format!("system get failed: {err}"))
                })?;
        Ok(output)
    }
}

#[async_trait]
impl FunctionTool for ArchitectSystemWriteTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_system_write".to_string(),
            description: format!(
                "Prepare a mutating Fluxbee admin action for hive {}. This tool does not execute immediately. It stages the action and requires explicit user confirmation in chat with CONFIRM or CANCEL. When mutation payloads are unclear, inspect /admin/actions/{{action}} first and use the request_contract/example_scmd guidance from SY.admin. Treat path_patterns as templates only; prefer example_scmd for the concrete execution shape and never send literal placeholder paths.",
                self.context.hive_id,
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "method": {
                        "type": "string",
                        "enum": ["POST", "PUT", "DELETE"],
                        "description": "Mutation method."
                    },
                    "path": {
                        "type": "string",
                        "description": "Mutating Fluxbee path, for example /hives/motherbee/nodes or /hives/motherbee/opa/policy/apply"
                    },
                    "body": {
                        "type": "object",
                        "description": "JSON body payload. Required whenever the action needs a body (e.g., wf_rules_compile_apply needs workflow_name and definition; opa_compile_apply needs rego; add_hive needs hive_id). Build this object completely from available context — for WF definitions, embed the full definition object here directly. Never omit the body on the assumption the system will supply it."
                    }
                },
                "required": ["method", "path"]
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let session_id = require_session_id(&self.context, "fluxbee_system_write")?;
        let method = arguments
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_system_write requires 'method'".to_string(),
                )
            })?
            .to_ascii_uppercase();
        if !matches!(method.as_str(), "POST" | "PUT" | "DELETE") {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(
                "fluxbee_system_write only supports POST, PUT, or DELETE".to_string(),
            ));
        }
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_system_write requires a non-empty 'path'".to_string(),
                )
            })?;
        let body = arguments.get("body").cloned();
        let raw_arguments = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());
        let raw = scmd_raw_from_parts(&method, path, body.as_ref());
        let parsed = parse_scmd(&raw).map_err(|err| {
            fluxbee_ai_sdk::AiSdkError::Protocol(format!("invalid system write path: {err}"))
        })?;
        let translation = translate_scmd(&self.context.hive_id, parsed).map_err(|err| {
            fluxbee_ai_sdk::AiSdkError::Protocol(format!("unsupported system write path: {err}"))
        })?;
        if !admin_action_allows_ai_write(&translation.action) {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "action '{}' is not enabled for AI write confirmation",
                translation.action
            )));
        }
        let preview_command = format!("SCMD: {raw}");
        stage_admin_write(
            &self.context,
            session_id,
            translation,
            &preview_command,
            &raw_arguments,
        )
        .await
    }
}

// ---- ArchitectDeployWorkflowTool -------------------------------------------
//
// Specialized write tool for wf_rules_compile_apply.
// Accepts the definition as either a JSON object (`definition`) or a
// JSON-encoded string (`definition_json`). The string path exists because the
// model readily produces large JSON as text but may refuse to embed it as a
// nested object in a tool call argument. Both paths produce identical behavior.

struct ArchitectDeployWorkflowTool {
    context: ArchitectAdminToolContext,
}

impl ArchitectDeployWorkflowTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl FunctionTool for ArchitectDeployWorkflowTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_deploy_workflow".to_string(),
            description: format!(
                "Compile and deploy a WF workflow definition on hive {} through SY.wf-rules. \
                Stages the deploy for user confirmation (CONFIRM / CANCEL). \
                Pass the definition as a JSON object in `definition` OR as a JSON-encoded string \
                in `definition_json` — use whichever form the model can emit cleanly.",
                self.context.hive_id,
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["workflow_name"],
                "properties": {
                    "hive": {
                        "type": "string",
                        "description": "Target hive id. Defaults to the local hive when omitted."
                    },
                    "workflow_name": {
                        "type": "string",
                        "description": "Workflow logical name. Must be lowercase letters, digits, hyphens, and optional dot-separated segments only — e.g. 'response-dispatch' or 'billing.invoice'. Underscores are not valid."
                    },
                    "definition": {
                        "type": "object",
                        "description": "Complete WF v1 workflow definition object. Use this when the object can be passed directly."
                    },
                    "definition_json": {
                        "type": "string",
                        "description": "Alternative to `definition`: the complete WF v1 definition serialized as a JSON string. Use this when passing the object directly is not possible."
                    },
                    "auto_spawn": {
                        "type": "boolean",
                        "description": "When true, spawn the WF node automatically if it does not exist after apply."
                    },
                    "tenant_id": {
                        "type": "string",
                        "description": "Required for the first deploy when auto_spawn=true and the WF node does not yet exist."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let session_id = require_session_id(&self.context, "fluxbee_deploy_workflow")?;
        let hive = arguments
            .get("hive")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.context.hive_id)
            .to_string();
        let workflow_name = arguments
            .get("workflow_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_deploy_workflow requires 'workflow_name'".to_string(),
                )
            })?
            .to_string();
        let definition = resolve_json_object_param(&arguments, "definition", "definition_json")
            .map_err(|err| {
                fluxbee_ai_sdk::AiSdkError::Protocol(format!("fluxbee_deploy_workflow: {err}"))
            })?;

        let mut body = json!({
            "workflow_name": workflow_name,
            "definition": definition,
        });
        if let Some(auto_spawn) = arguments.get("auto_spawn") {
            body["auto_spawn"] = auto_spawn.clone();
        }
        if let Some(tenant_id) = arguments
            .get("tenant_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            body["tenant_id"] = json!(tenant_id);
        }

        let translation = AdminTranslation {
            admin_target: format!("SY.admin@{hive}"),
            action: "wf_rules_compile_apply".to_string(),
            target_hive: hive.clone(),
            params: body.clone(),
        };
        let body_str = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
        let preview_command = format!("SCMD: curl -X POST /hives/{hive}/wf-rules -d '{body_str}'");
        let tool_arguments = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());
        stage_admin_write(
            &self.context,
            session_id,
            translation,
            &preview_command,
            &tool_arguments,
        )
        .await
    }
}

// ---- ArchitectDeployOpaPolicyTool -------------------------------------------
//
// Accepts OPA rego source as a named string parameter instead of embedding it
// inside the generic `body` object. OPA rego can be arbitrarily large.

struct ArchitectDeployOpaPolicyTool {
    context: ArchitectAdminToolContext,
}

impl ArchitectDeployOpaPolicyTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl FunctionTool for ArchitectDeployOpaPolicyTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_deploy_opa_policy".to_string(),
            description: format!(
                "Compile and apply an OPA policy on hive {} through SY.opa-rules. \
                Stages the deploy for user confirmation (CONFIRM / CANCEL). \
                Use this instead of fluxbee_system_write when the rego source is large.",
                self.context.hive_id,
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["rego"],
                "properties": {
                    "hive": {
                        "type": "string",
                        "description": "Target hive id. Defaults to the local hive when omitted."
                    },
                    "rego": {
                        "type": "string",
                        "description": "Complete OPA rego source text."
                    },
                    "entrypoint": {
                        "type": "string",
                        "description": "OPA entrypoint. Defaults to 'router/target' when omitted."
                    },
                    "version": {
                        "type": "integer",
                        "description": "Optional explicit version. A new version is assigned automatically when omitted."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let session_id = require_session_id(&self.context, "fluxbee_deploy_opa_policy")?;
        let hive = arguments
            .get("hive")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.context.hive_id)
            .to_string();
        let rego = arguments
            .get("rego")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_deploy_opa_policy requires 'rego'".to_string(),
                )
            })?
            .to_string();

        let mut body = json!({ "rego": rego });
        if let Some(entrypoint) = arguments
            .get("entrypoint")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            body["entrypoint"] = json!(entrypoint);
        }
        if let Some(version) = arguments.get("version").filter(|v| v.is_number()) {
            body["version"] = version.clone();
        }

        let translation = AdminTranslation {
            admin_target: format!("SY.admin@{hive}"),
            action: "opa_compile_apply".to_string(),
            target_hive: hive.clone(),
            params: body.clone(),
        };
        let rego_preview = if rego.len() > 120 {
            format!("{}...", &rego[..120])
        } else {
            rego.clone()
        };
        let preview_command = format!(
            "SCMD: curl -X POST /hives/{hive}/opa/policy -d '{{\"rego\":\"{rego_preview}\",…}}'"
        );
        let tool_arguments = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());
        stage_admin_write(
            &self.context,
            session_id,
            translation,
            &preview_command,
            &tool_arguments,
        )
        .await
    }
}

// ---- ArchitectSetNodeConfigTool ---------------------------------------------
//
// Accepts the node config object as a named first-class parameter for
// node_control_config_set. The `config` field is fully node-defined and can be
// arbitrarily large (AI.chat prompts, WF node config, IO adapter config, etc.).

struct ArchitectSetNodeConfigTool {
    context: ArchitectAdminToolContext,
}

impl ArchitectSetNodeConfigTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl FunctionTool for ArchitectSetNodeConfigTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_set_node_config".to_string(),
            description: format!(
                "Send a CONFIG_SET to a node on hive {} via node_control_config_set. \
                Stages the mutation for user confirmation (CONFIRM / CANCEL). \
                Use this instead of fluxbee_system_write when the config object is large \
                (AI node prompts, WF node config, IO adapter config, etc.). \
                Always do a CONFIG_GET first to read the current config_version before calling this.",
                self.context.hive_id,
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["node_name", "config"],
                "properties": {
                    "hive": {
                        "type": "string",
                        "description": "Target hive id. Defaults to the local hive when omitted."
                    },
                    "node_name": {
                        "type": "string",
                        "description": "Fully-qualified node name, e.g. 'AI.chat@motherbee'."
                    },
                    "config": {
                        "type": "object",
                        "description": "Node-defined config payload. Content is node-specific and not interpreted by SY.admin. Use this when the object can be passed directly."
                    },
                    "config_json": {
                        "type": "string",
                        "description": "Alternative to `config`: the node config serialized as a JSON string. Use this when passing the object directly is not possible."
                    },
                    "config_version": {
                        "type": "integer",
                        "description": "Monotonic config version. Use the value from CONFIG_GET + 1. Defaults to 1 when the node has no prior config."
                    },
                    "schema_version": {
                        "type": "integer",
                        "description": "Schema version understood by the node. Defaults to 1."
                    },
                    "apply_mode": {
                        "type": "string",
                        "enum": ["replace", "merge"],
                        "description": "Apply mode. 'replace' is the standard mode for most nodes."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let session_id = require_session_id(&self.context, "fluxbee_set_node_config")?;
        let hive = arguments
            .get("hive")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.context.hive_id)
            .to_string();
        let node_name = arguments
            .get("node_name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_set_node_config requires 'node_name'".to_string(),
                )
            })?
            .to_string();
        let config =
            resolve_json_object_param(&arguments, "config", "config_json").map_err(|err| {
                fluxbee_ai_sdk::AiSdkError::Protocol(format!("fluxbee_set_node_config: {err}"))
            })?;
        let schema_version = arguments
            .get("schema_version")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let config_version = arguments
            .get("config_version")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let apply_mode = arguments
            .get("apply_mode")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("replace")
            .to_string();

        let body = json!({
            "node_name": node_name,
            "schema_version": schema_version,
            "config_version": config_version,
            "apply_mode": apply_mode,
            "config": config,
        });
        let translation = AdminTranslation {
            admin_target: format!("SY.admin@{hive}"),
            action: "node_control_config_set".to_string(),
            target_hive: hive.clone(),
            params: body,
        };
        let preview_command = format!(
            "SCMD: curl -X POST /hives/{hive}/nodes/{node_name}/control/config-set -d '{{…config…}}'"
        );
        let tool_arguments = serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string());
        stage_admin_write(
            &self.context,
            session_id,
            translation,
            &preview_command,
            &tool_arguments,
        )
        .await
    }
}

// ---- shared staging helper --------------------------------------------------

async fn stage_admin_write(
    context: &ArchitectAdminToolContext,
    session_id: &str,
    translation: AdminTranslation,
    preview_command: &str,
    tool_arguments: &str,
) -> fluxbee_ai_sdk::Result<Value> {
    validate_mutating_translation_contract(context, &translation).await?;

    let operation_id = Uuid::new_v4().to_string();
    let scope_id = operation_scope_id(session_id);
    let normalized_params = normalize_json(&translation.params);
    let params_json =
        serde_json::to_string(&normalized_params).unwrap_or_else(|_| "{}".to_string());
    let params_hash_value = params_hash(&translation.params);
    if let Some(mut existing) = find_equivalent_operation_with_context(
        context,
        session_id,
        &translation.action,
        &translation.target_hive,
        &params_hash_value,
    )
    .await
    .map_err(|err| {
        fluxbee_ai_sdk::AiSdkError::Protocol(format!("failed to inspect prior operations: {err}"))
    })? {
        if existing.status == "timeout_unknown" && translation.action == "add_hive" {
            let params = normalized_params.clone();
            if let Some(hive_id) = params.get("hive_id").and_then(Value::as_str) {
                let output = execute_admin_translation_with_context(
                    context,
                    AdminTranslation {
                        admin_target: format!("SY.admin@{}", context.hive_id),
                        action: "get_hive".to_string(),
                        target_hive: context.hive_id.clone(),
                        params: json!({ "hive_id": hive_id }),
                    },
                    "tool.reconcile",
                )
                .await
                .map_err(|err| {
                    fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                        "failed to reconcile prior timeout: {err}"
                    ))
                })?;
                if output
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|value| value.eq_ignore_ascii_case("ok"))
                    .unwrap_or(false)
                {
                    existing.status = "succeeded_after_timeout".to_string();
                    existing.updated_at_ms = now_epoch_ms();
                    existing.completed_at_ms = existing.updated_at_ms;
                    existing.error_summary.clear();
                    save_operation_with_context(context, &existing)
                        .await
                        .map_err(|err| {
                            fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                                "failed to persist reconciled operation: {err}"
                            ))
                        })?;
                    return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                        "A previous add_hive operation already completed after a timeout for hive '{}'. Do not retry blindly; inspect the hive state first.",
                        hive_id
                    )));
                }
            }
        }
        return Err(fluxbee_ai_sdk::AiSdkError::Protocol(
            operation_error_message(&existing),
        ));
    }
    let created_at_ms = now_epoch_ms();
    let operation = ChatOperationRecord {
        operation_id: operation_id.clone(),
        session_id: session_id.to_string(),
        scope_id,
        origin: "ai_write_stage".to_string(),
        action: translation.action.clone(),
        target_hive: translation.target_hive.clone(),
        params_json: params_json.clone(),
        params_hash: params_hash_value,
        preview_command: preview_command.to_string(),
        status: "pending_confirm".to_string(),
        created_at_ms,
        updated_at_ms: created_at_ms,
        dispatched_at_ms: 0,
        completed_at_ms: 0,
        request_id: String::new(),
        trace_id: String::new(),
        error_summary: String::new(),
    };
    save_operation_with_context(context, &operation)
        .await
        .map_err(|err| {
            fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "failed to persist staged operation: {err}"
            ))
        })?;
    let pending = PendingAdminAction {
        operation_id: operation_id.clone(),
        translation: translation.clone(),
        preview_command: preview_command.to_string(),
        created_at_ms,
    };
    context
        .pending_actions
        .lock()
        .await
        .insert(session_id.to_string(), pending);
    tracing::info!(
        session_id = %session_id,
        action = %translation.action,
        target_hive = %translation.target_hive,
        tool_arguments = %tool_arguments,
        operation_id = %operation_id,
        preview_command = %preview_command,
        params = %params_json,
        "sy.architect staged admin write"
    );

    Ok(json!({
        "operation_id": operation_id,
        "pending_confirmation": true,
        "requires_confirmation": true,
        "action": translation.action,
        "preview_command": preview_command,
        "message": "Mutation prepared. Reply CONFIRM to execute it or CANCEL to discard it."
    }))
}

fn require_session_id<'a>(
    context: &'a ArchitectAdminToolContext,
    tool_name: &str,
) -> fluxbee_ai_sdk::Result<&'a str> {
    context
        .session_id
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "{tool_name} requires a live chat session"
            ))
        })
}

// Resolves a JSON object parameter from either a direct object field or a
// JSON-encoded string field. The string path exists for cases where the model
// refuses to pass large nested JSON as an object argument but can pass it as
// a string.
fn resolve_json_object_param(
    arguments: &Value,
    object_key: &str,
    string_key: &str,
) -> Result<Value, String> {
    if let Some(obj) = arguments.get(object_key).filter(|v| v.is_object()) {
        return Ok(obj.clone());
    }
    if let Some(s) = arguments
        .get(string_key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return serde_json::from_str(s)
            .map_err(|err| format!("'{string_key}' is not valid JSON: {err}"));
    }
    Err(format!(
        "one of '{object_key}' (object) or '{string_key}' (JSON string) is required"
    ))
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
    let node_config = load_architect_node_config(&hive.hive_id)?;
    let node_name = architect_node_name(&hive.hive_id);
    let ai_runtime = build_architect_ai_runtime(&node_name, node_config.as_ref(), &hive);
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

    let initial_status = ArchitectStatus {
        status: "loading".to_string(),
        hive_id: hive.hive_id.clone(),
        node_name: node_name.clone(),
        router_connected: false,
        admin_available: false,
        inventory_updated_at: None,
        total_hives: None,
        hives_alive: None,
        hives_stale: None,
        total_nodes: None,
        nodes_by_status: BTreeMap::new(),
        components: default_component_statuses(),
        error: None,
    };
    let state = Arc::new(ArchitectState {
        hive_id: hive.hive_id.clone(),
        node_name: node_name.clone(),
        listen: listen.clone(),
        config_dir,
        state_dir,
        socket_dir,
        router_connected: AtomicBool::new(false),
        ai_configured: AtomicBool::new(ai_runtime.is_some()),
        ai_runtime: Arc::new(Mutex::new(ai_runtime)),
        chat_lock: Arc::new(Mutex::new(())),
        pending_actions: Arc::new(Mutex::new(HashMap::new())),
        router_sender: Arc::new(Mutex::new(None)),
        cached_status: Arc::new(RwLock::new(initial_status)),
    });

    ensure_chat_storage(&state).await?;

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

    let refresh_state = Arc::clone(&state);
    tokio::spawn(async move {
        status_refresh_loop(refresh_state).await;
    });

    let app = Router::new()
        .route("/", any(root_handler))
        .route("/healthz", any(dynamic_handler))
        .route("/api/status", any(dynamic_handler))
        .route("/api/chat", any(dynamic_handler))
        .route("/api/executor/plan", any(dynamic_handler))
        .route("/api/attachments", any(dynamic_handler))
        .route("/api/sessions", any(dynamic_handler))
        .route("/api/sessions/*path", any(dynamic_handler))
        .route("/*path", any(dynamic_handler))
        .layer(DefaultBodyLimit::max(ARCHITECT_MAX_ATTACHMENT_UPLOAD_BYTES))
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

fn load_architect_node_config(
    hive_id: &str,
) -> Result<Option<ArchitectNodeConfigFile>, ArchitectError> {
    let path = architect_config_path(hive_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    let parsed = serde_json::from_str(&raw)?;
    Ok(Some(parsed))
}

fn architect_node_dir(hive_id: &str) -> PathBuf {
    json_router::paths::storage_root_dir()
        .join("nodes")
        .join("SY")
        .join(format!("SY.architect@{hive_id}"))
}

fn architect_node_name(hive_id: &str) -> String {
    format!("SY.architect@{hive_id}")
}

fn architect_config_path(hive_id: &str) -> PathBuf {
    architect_node_dir(hive_id).join("config.json")
}

fn build_architect_ai_runtime(
    node_name: &str,
    config: Option<&ArchitectNodeConfigFile>,
    hive: &HiveFile,
) -> Option<ArchitectAiRuntime> {
    let secrets = load_architect_secret_record(node_name).ok().flatten();
    let openai = merged_openai_section(config, hive, secrets.as_ref())?;
    let api_key = openai
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let model = openai
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ARCHITECT_MODEL)
        .to_string();

    Some(ArchitectAiRuntime {
        model,
        instructions: ARCHI_SYSTEM_PROMPT.to_string(),
        model_settings: ModelSettings {
            temperature: openai.temperature,
            top_p: openai.top_p,
            max_output_tokens: openai.max_tokens,
        },
        client: OpenAiResponsesClient::new(api_key),
    })
}

fn merged_openai_section(
    config: Option<&ArchitectNodeConfigFile>,
    hive: &HiveFile,
    secrets: Option<&NodeSecretRecord>,
) -> Option<OpenAiSection> {
    let config_openai = config
        .and_then(|cfg| cfg.ai_providers.as_ref())
        .and_then(|providers| providers.openai.clone());
    let hive_openai = hive
        .ai_providers
        .as_ref()
        .and_then(|providers| providers.openai.clone());
    let secret_api_key = secrets
        .and_then(|record| record.secrets.get(ARCHITECT_LOCAL_SECRET_KEY_OPENAI))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    match (config_openai, hive_openai) {
        (Some(cfg), Some(hive_cfg)) => Some(OpenAiSection {
            api_key: secret_api_key,
            default_model: cfg.default_model.or(hive_cfg.default_model),
            max_tokens: cfg.max_tokens.or(hive_cfg.max_tokens),
            temperature: cfg.temperature.or(hive_cfg.temperature),
            top_p: cfg.top_p.or(hive_cfg.top_p),
        }),
        (Some(cfg), None) => Some(OpenAiSection {
            api_key: secret_api_key,
            default_model: cfg.default_model,
            max_tokens: cfg.max_tokens,
            temperature: cfg.temperature,
            top_p: cfg.top_p,
        }),
        (None, Some(hive_cfg)) => Some(OpenAiSection {
            api_key: secret_api_key,
            default_model: hive_cfg.default_model,
            max_tokens: hive_cfg.max_tokens,
            temperature: hive_cfg.temperature,
            top_p: hive_cfg.top_p,
        }),
        (None, None) => secret_api_key.map(|api_key| OpenAiSection {
            api_key: Some(api_key),
            default_model: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }),
    }
}

fn architect_nodes_root() -> PathBuf {
    json_router::paths::storage_root_dir().join("nodes")
}

fn load_architect_secret_record(
    node_name: &str,
) -> Result<Option<NodeSecretRecord>, ArchitectError> {
    match load_node_secret_record_with_root(node_name, architect_nodes_root()) {
        Ok(record) => Ok(Some(record)),
        Err(NodeSecretError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Box::new(err)),
    }
}

async fn refresh_architect_ai_runtime(state: &ArchitectState) -> Result<bool, ArchitectError> {
    let hive = load_hive(&state.config_dir)?;
    let node_config = load_architect_node_config(&hive.hive_id)?;
    let runtime = build_architect_ai_runtime(&state.node_name, node_config.as_ref(), &hive);
    state
        .ai_configured
        .store(runtime.is_some(), Ordering::Relaxed);
    *state.ai_runtime.lock().await = runtime;
    Ok(state.ai_configured.load(Ordering::Relaxed))
}

fn load_identity_ich_options(
    state: &ArchitectState,
) -> Result<Vec<IdentityIchOption>, ArchitectError> {
    list_ich_options_from_hive_config(&state.config_dir)
        .map_err(|err| -> ArchitectError { Box::new(err) })
}

fn admin_tool_context(
    state: &ArchitectState,
    session_id: Option<&str>,
) -> ArchitectAdminToolContext {
    ArchitectAdminToolContext {
        hive_id: state.hive_id.clone(),
        config_dir: state.config_dir.clone(),
        state_dir: state.state_dir.clone(),
        socket_dir: state.socket_dir.clone(),
        session_id: session_id.map(str::to_string),
        chat_lock: Arc::clone(&state.chat_lock),
        pending_actions: Arc::clone(&state.pending_actions),
    }
}

fn confirmation_requested(input: &str) -> bool {
    matches!(
        input.trim().to_ascii_uppercase().as_str(),
        "CONFIRM" | "OK CONFIRM"
    )
}

fn cancellation_requested(input: &str) -> bool {
    matches!(
        input.trim().to_ascii_uppercase().as_str(),
        "CANCEL" | "ABORT"
    )
}

async fn take_pending_action(
    state: &ArchitectState,
    session_id: &str,
) -> Option<PendingAdminAction> {
    state.pending_actions.lock().await.remove(session_id)
}

async fn clear_pending_action(
    state: &ArchitectState,
    session_id: &str,
) -> Option<PendingAdminAction> {
    state.pending_actions.lock().await.remove(session_id)
}

async fn router_connect_loop(config: NodeConfig, state: Arc<ArchitectState>) {
    loop {
        match connect(&config).await {
            Ok((sender, receiver)) => {
                *state.router_sender.lock().await = Some(sender);
                state.router_connected.store(true, Ordering::Relaxed);
                tracing::info!(node = %state.node_name, "sy.architect connected to router");
                if let Err(err) = router_recv_loop(receiver, Arc::clone(&state)).await {
                    tracing::warn!(error = %err, "sy.architect router loop ended");
                }
                *state.router_sender.lock().await = None;
                state.router_connected.store(false, Ordering::Relaxed);
            }
            Err(err) => {
                *state.router_sender.lock().await = None;
                state.router_connected.store(false, Ordering::Relaxed);
                tracing::warn!(error = %err, "sy.architect router connect failed");
            }
        }
        time::sleep(Duration::from_secs(ROUTER_RECONNECT_DELAY_SECS)).await;
    }
}

async fn router_recv_loop(
    mut receiver: NodeReceiver,
    state: Arc<ArchitectState>,
) -> Result<(), NodeError> {
    loop {
        let msg = receiver.recv().await?;
        tracing::info!(
            src = %msg.routing.src,
            dst = ?msg.routing.dst,
            msg_type = %msg.meta.msg_type,
            msg = ?msg.meta.msg,
            "sy.architect received message"
        );
        if let Some(session_id) = router_message_session_id(&msg) {
            if let Err(err) = persist_router_incoming_message(&state, &session_id, &msg).await {
                tracing::warn!(
                    error = %err,
                    session_id = %session_id,
                    trace_id = %msg.routing.trace_id,
                    "failed to persist incoming router message for impersonation chat"
                );
            }
        }
    }
}

async fn root_handler(State(state): State<Arc<ArchitectState>>) -> Html<String> {
    Html(architect_index_html(&state))
}

fn architect_blob_root(state: &ArchitectState) -> Result<PathBuf, ArchitectError> {
    let hive = load_hive(&state.config_dir)?;
    if let Some(path) = hive
        .blob
        .as_ref()
        .and_then(|blob| blob.path.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("FLUXBEE_BLOB_ROOT") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    if let Ok(path) = std::env::var("BLOB_ROOT") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    Ok(BlobConfig::default().blob_root)
}

fn architect_blob_toolkit(state: &ArchitectState) -> Result<BlobToolkit, ArchitectError> {
    let mut cfg = BlobConfig::default();
    cfg.blob_root = architect_blob_root(state)?;
    BlobToolkit::new(cfg).map_err(|err| -> ArchitectError {
        format!("failed to initialize blob toolkit for archi attachments: {err}").into()
    })
}

fn normalize_attachment_mime(provided: Option<&str>, filename: &str) -> Option<String> {
    let provided = provided
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if provided.is_some() {
        return provided;
    }
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())?;
    let mime = match extension.as_str() {
        "txt" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return None,
    };
    Some(mime.to_string())
}

fn architect_attachment_mime_supported(mime: &str) -> bool {
    matches!(
        mime,
        "text/plain"
            | "text/markdown"
            | "text/csv"
            | "application/json"
            | "application/pdf"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
            | "image/png"
            | "image/jpeg"
            | "image/webp"
            | "image/gif"
    )
}

async fn handle_attachment_upload(
    state: &ArchitectState,
    request: Request,
) -> Result<AttachmentUploadResponse, ArchitectError> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|err| format!("invalid multipart upload: {err}"))?;
    let toolkit = architect_blob_toolkit(state)?;
    let mut attachments = Vec::new();
    let mut files_seen = 0usize;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| format!("failed to read multipart field: {err}"))?
    {
        let Some(filename) = field
            .file_name()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        else {
            continue;
        };

        files_seen += 1;
        if files_seen > ARCHITECT_MAX_ATTACHMENTS {
            tracing::warn!(
                count = files_seen,
                max = ARCHITECT_MAX_ATTACHMENTS,
                "archi: attachment upload rejected — too many files"
            );
            return Err(format!(
                "too many attachments: max {} files per upload",
                ARCHITECT_MAX_ATTACHMENTS
            )
            .into());
        }

        let mime =
            normalize_attachment_mime(field.content_type(), &filename).ok_or_else(|| {
                tracing::warn!(filename = %filename, "archi: attachment upload rejected — unrecognized mime");
                format!("unsupported attachment mime for '{filename}'")
            })?;
        if !architect_attachment_mime_supported(&mime) {
            tracing::warn!(filename = %filename, mime = %mime, "archi: attachment upload rejected — mime not allowed");
            return Err(format!("unsupported attachment mime: {mime}").into());
        }

        let data = field
            .bytes()
            .await
            .map_err(|err| format!("failed to read uploaded file '{filename}': {err}"))?;
        if data.is_empty() {
            tracing::warn!(filename = %filename, "archi: attachment upload rejected — empty file");
            return Err(format!("attachment '{filename}' is empty").into());
        }
        if data.len() > ARCHITECT_MAX_ATTACHMENT_BYTES {
            tracing::warn!(
                filename = %filename,
                size = data.len(),
                max = ARCHITECT_MAX_ATTACHMENT_BYTES,
                "archi: attachment upload rejected — file too large"
            );
            return Err(format!(
                "attachment '{}' exceeds max size {} bytes",
                filename, ARCHITECT_MAX_ATTACHMENT_BYTES
            )
            .into());
        }

        let blob_ref = toolkit
            .put_bytes(data.as_ref(), &filename, &mime)
            .and_then(|blob_ref| {
                toolkit.promote(&blob_ref)?;
                Ok(blob_ref)
            })
            .map_err(|err| format!("failed to persist attachment '{filename}': {err}"))?;
        tracing::info!(
            filename = %blob_ref.filename_original,
            mime = %blob_ref.mime,
            size = blob_ref.size,
            blob_name = %blob_ref.blob_name,
            "archi: attachment uploaded"
        );
        attachments.push(UploadedAttachment {
            attachment_id: format!("att_{}", Uuid::new_v4().simple()),
            filename,
            mime,
            size: blob_ref.size,
            blob_ref,
        });
    }

    if attachments.is_empty() {
        return Err("multipart upload did not contain any file fields".into());
    }

    Ok(AttachmentUploadResponse {
        status: "ok".to_string(),
        attachments,
    })
}

async fn dynamic_handler(
    State(state): State<Arc<ArchitectState>>,
    uri: Uri,
    method: Method,
    request: Request,
) -> Response {
    let path = uri.path();
    match (method, path) {
        (Method::GET, _) if is_favicon_path(path) => serve_favicon(path),
        (Method::GET, _) if is_status_path(path) => {
            let status = state.cached_status.read().await.clone();
            Json(status).into_response()
        }
        (Method::GET, _) if is_identity_ich_options_path(path) => {
            match load_identity_ich_options(&state) {
                Ok(options) => Json(IdentityIchOptionsResponse { options }).into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to load identity options: {err}") })),
                )
                    .into_response(),
            }
        }
        (Method::GET, _) if is_sessions_collection_path(path) => {
            match list_chat_sessions(&state).await {
                Ok(sessions) => Json(SessionListResponse { sessions }).into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to list sessions: {err}") })),
                )
                    .into_response(),
            }
        }
        (Method::POST, _) if is_sessions_collection_path(path) => {
            let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
                Ok(body) => body,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid body: {err}") })),
                    )
                        .into_response()
                }
            };
            let req = if body.is_empty() {
                CreateSessionRequest {
                    title: None,
                    chat_mode: None,
                    effective_ich_id: None,
                    effective_ilk: None,
                    impersonation_target: None,
                    thread_id: None,
                    source_channel_kind: None,
                    debug_enabled: None,
                }
            } else {
                match serde_json::from_slice::<CreateSessionRequest>(&body) {
                    Ok(req) => req,
                    Err(err) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": format!("invalid json: {err}") })),
                        )
                            .into_response()
                    }
                }
            };
            match create_chat_session(&state, req).await {
                Ok(detail) => Json(detail).into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to create session: {err}") })),
                )
                    .into_response(),
            }
        }
        (Method::DELETE, _) if is_sessions_collection_path(path) => {
            match clear_chat_sessions(&state).await {
                Ok(result) => Json(result).into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to clear sessions: {err}") })),
                )
                    .into_response(),
            }
        }
        (Method::GET, _) if is_session_detail_path(path) => {
            let Some(session_id) = session_id_from_path(path) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "missing session id" })),
                )
                    .into_response();
            };
            match load_chat_session(&state, session_id).await {
                Ok(Some(detail)) => Json(detail).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "session_not_found" })),
                )
                    .into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to load session: {err}") })),
                )
                    .into_response(),
            }
        }
        (Method::DELETE, _) if is_session_detail_path(path) => {
            let Some(session_id) = session_id_from_path(path) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "missing session id" })),
                )
                    .into_response();
            };
            match delete_chat_session(&state, session_id).await {
                Ok(result) => Json(result).into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to delete session: {err}") })),
                )
                    .into_response(),
            }
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
            let out =
                handle_chat_message(&state, req.session_id, req.message, req.attachments).await;
            Json(out).into_response()
        }
        (Method::POST, _) if is_executor_plan_path(path) => {
            let body = match axum::body::to_bytes(request.into_body(), 4 * 1024 * 1024).await {
                Ok(body) => body,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid body: {err}") })),
                    )
                        .into_response()
                }
            };
            let req: ExecutorPlanRequest = match serde_json::from_slice(&body) {
                Ok(req) => req,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid json: {err}") })),
                    )
                        .into_response()
                }
            };
            let out = handle_executor_plan_request(&state, req).await;
            Json(out).into_response()
        }
        (Method::POST, _) if is_attachments_path(path) => {
            match handle_attachment_upload(&state, request).await {
                Ok(response) => Json(response).into_response(),
                Err(err) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": err.to_string() })),
                )
                    .into_response(),
            }
        }
        (Method::GET, _) if !is_api_path(path) => {
            Html(architect_index_html(&state)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
    }
}

async fn handle_chat_message(
    state: &ArchitectState,
    session_id: Option<String>,
    message: String,
    attachments: Vec<ChatAttachmentRef>,
) -> ChatResponse {
    let (resolved_session_id, mut session) =
        match resolve_chat_session(state, session_id, None).await {
            Ok(values) => values,
            Err(err) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "chat".to_string(),
                    output: json!({ "error": format!("session unavailable: {err}") }),
                    session_id: None,
                    session_title: None,
                }
            }
        };

    let trimmed_message = message.trim();
    let response = if confirmation_requested(trimmed_message) {
        match take_pending_action(state, &resolved_session_id).await {
            Some(pending) => {
                let action_name = pending.translation.action.clone();
                let preview_command = pending.preview_command.clone();
                let pending_created_at_ms = pending.created_at_ms;
                let mut output = match execute_tracked_admin_translation(
                    state,
                    &resolved_session_id,
                    "ai_confirmed",
                    pending.translation,
                    preview_command.clone(),
                    Some(pending.operation_id),
                )
                .await
                {
                    Ok(output) => output,
                    Err(err) => json!({
                        "status": "error",
                        "action": action_name,
                        "confirmed_command": preview_command,
                        "error": err.to_string()
                    }),
                };
                output["confirmed_command"] = Value::String(preview_command);
                output["pending_created_at_ms"] = json!(pending_created_at_ms);
                output["confirmed_at_ms"] = json!(now_epoch_ms());
                let status = chat_status_from_command_output(&output);
                ChatResponse {
                    status,
                    mode: "scmd".to_string(),
                    output,
                    session_id: Some(resolved_session_id.clone()),
                    session_title: Some(session.title.clone()),
                }
            }
            None => ChatResponse {
                status: "error".to_string(),
                mode: "chat".to_string(),
                output: json!({
                    "message": "There is no pending action to confirm in this chat."
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
        }
    } else if cancellation_requested(trimmed_message) {
        match clear_pending_action(state, &resolved_session_id).await {
            Some(pending) => {
                let now = now_epoch_ms();
                let params_json =
                    serde_json::to_string(&normalize_json(&pending.translation.params))
                        .unwrap_or_else(|_| "{}".to_string());
                let record = ChatOperationRecord {
                    operation_id: pending.operation_id,
                    session_id: resolved_session_id.clone(),
                    scope_id: operation_scope_id(&resolved_session_id),
                    origin: "ai_write_stage".to_string(),
                    action: pending.translation.action,
                    target_hive: pending.translation.target_hive,
                    params_json,
                    params_hash: params_hash(&pending.translation.params),
                    preview_command: pending.preview_command.clone(),
                    status: "canceled".to_string(),
                    created_at_ms: pending.created_at_ms,
                    updated_at_ms: now,
                    dispatched_at_ms: 0,
                    completed_at_ms: now,
                    request_id: String::new(),
                    trace_id: String::new(),
                    error_summary: String::new(),
                };
                let save_result = save_operation(state, &record).await;
                ChatResponse {
                    status: "ok".to_string(),
                    mode: "chat".to_string(),
                    output: json!({
                        "message": format!("Pending action discarded: {}", pending.preview_command),
                        "pending_created_at_ms": pending.created_at_ms,
                        "operation_id": record.operation_id,
                        "tracking_saved": save_result.is_ok(),
                    }),
                    session_id: Some(resolved_session_id.clone()),
                    session_title: Some(session.title.clone()),
                }
            }
            None => ChatResponse {
                status: "error".to_string(),
                mode: "chat".to_string(),
                output: json!({
                    "message": "There is no pending action to cancel in this chat."
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
        }
    } else if let Some(raw) = message.strip_prefix("SCMD:") {
        match handle_scmd(state, &resolved_session_id, raw.trim()).await {
            Ok(output) => {
                let status = chat_status_from_command_output(&output);
                ChatResponse {
                    status,
                    mode: "scmd".to_string(),
                    output,
                    session_id: Some(resolved_session_id.clone()),
                    session_title: Some(session.title.clone()),
                }
            }
            Err(err) => ChatResponse {
                status: "error".to_string(),
                mode: "scmd".to_string(),
                output: json!({
                    "action": "scmd_error",
                    "error_code": "SCMD_EXECUTION_FAILED",
                    "error_detail": err.to_string(),
                    "payload": serde_json::Value::Null,
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
        }
    } else if message.trim_start().starts_with("ACMD:") {
        ChatResponse {
            status: "error".to_string(),
            mode: "chat".to_string(),
            output: json!({
                "error": "ACMD was renamed to SCMD. Use SCMD: curl -X GET /hives/{hive}/nodes"
            }),
            session_id: Some(resolved_session_id.clone()),
            session_title: Some(session.title.clone()),
        }
    } else if message.trim_start().starts_with("FCMD:") {
        ChatResponse {
            status: "error".to_string(),
            mode: "chat".to_string(),
            output: json!({
                "error": "FCMD prompt operations were removed. Architect prompts now ship with the project and change only through a rebuild."
            }),
            session_id: Some(resolved_session_id.clone()),
            session_title: Some(session.title.clone()),
        }
    } else if let Some(plan_value) = try_extract_executor_plan_json(trimmed_message) {
        let execution_id = Uuid::new_v4().to_string();
        let plan = match validate_architect_executor_plan_shape(&plan_value) {
            Ok(p) => p,
            Err(err) => {
                let response = ChatResponse {
                    status: "error".to_string(),
                    mode: "executor".to_string(),
                    output: json!({
                        "error": err.to_string(),
                        "execution_id": execution_id,
                        "phase": "architect_precheck",
                    }),
                    session_id: Some(resolved_session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
                if let Err(err) =
                    persist_chat_exchange(state, &mut session, message, &attachments, &response)
                        .await
                {
                    tracing::warn!(error = %err, "failed to persist executor plan exchange");
                }
                return ChatResponse {
                    session_title: Some(session.title),
                    ..response
                };
            }
        };
        let context = admin_tool_context(state, Some(&resolved_session_id));
        match execute_executor_plan_with_context(
            &context,
            execution_id.clone(),
            plan,
            json!({}),
            json!({}),
        )
        .await
        {
            Ok(output) => ChatResponse {
                status: chat_status_from_command_output(&output),
                mode: "executor".to_string(),
                output,
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
            Err(err) => {
                let error_text = err.to_string();
                let phase = if error_text.starts_with("executor plan validation failed:") {
                    Some("admin_validate")
                } else {
                    None
                };
                ChatResponse {
                    status: "error".to_string(),
                    mode: "executor".to_string(),
                    output: json!({
                        "error": error_text,
                        "execution_id": execution_id,
                        "phase": phase,
                    }),
                    session_id: Some(resolved_session_id.clone()),
                    session_title: Some(session.title.clone()),
                }
            }
        }
    } else if session.chat_mode == CHAT_MODE_IMPERSONATION {
        match handle_impersonation_chat(state, &session, message.trim()).await {
            Ok(output) => ChatResponse {
                status: "ok".to_string(),
                mode: "dispatch".to_string(),
                output,
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
            Err(err) => ChatResponse {
                status: "error".to_string(),
                mode: "chat".to_string(),
                output: json!({
                    "error": err.to_string(),
                    "message": err.to_string(),
                    "router_connected": state.router_connected.load(Ordering::Relaxed),
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
        }
    } else {
        match handle_ai_chat(state, &session, message.trim(), &attachments).await {
            Ok(output) => ChatResponse {
                status: "ok".to_string(),
                mode: "chat".to_string(),
                output,
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
            Err(err) => ChatResponse {
                status: "error".to_string(),
                mode: "chat".to_string(),
                output: json!({
                    "error": err.to_string(),
                    "message": err.to_string(),
                    "ai_configured": state.ai_configured.load(Ordering::Relaxed),
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            },
        }
    };

    let new_title = update_title_from_message(&session.title, &message);
    if new_title != session.title {
        session.title = new_title;
    }
    if let Err(err) =
        persist_chat_exchange(state, &mut session, message, &attachments, &response).await
    {
        tracing::warn!(error = %err, session_id = %resolved_session_id, "failed to persist chat exchange");
    }

    ChatResponse {
        session_title: Some(session.title),
        ..response
    }
}

async fn handle_executor_plan_request(
    state: &ArchitectState,
    req: ExecutorPlanRequest,
) -> ChatResponse {
    let (resolved_session_id, mut session) =
        match resolve_chat_session(state, req.session_id, req.title.clone()).await {
            Ok(values) => values,
            Err(err) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "executor".to_string(),
                    output: json!({ "error": format!("session unavailable: {err}") }),
                    session_id: None,
                    session_title: None,
                }
            }
        };

    let execution_id = req
        .execution_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let plan = match validate_architect_executor_plan_shape(&req.plan) {
        Ok(plan) => plan,
        Err(err) => {
            let response = ChatResponse {
                status: "error".to_string(),
                mode: "executor".to_string(),
                output: json!({
                    "error": err.to_string(),
                    "execution_id": execution_id,
                    "phase": "architect_precheck",
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            };
            let plan_text = render_executor_plan_submission(&execution_id, &req.plan);
            let _ = persist_chat_exchange(state, &mut session, plan_text, &[], &response).await;
            return response;
        }
    };

    let context = admin_tool_context(state, Some(&resolved_session_id));
    let response = match execute_executor_plan_with_context(
        &context,
        execution_id.clone(),
        plan,
        req.executor_options,
        req.labels,
    )
    .await
    {
        Ok(output) => ChatResponse {
            status: chat_status_from_command_output(&output),
            mode: "executor".to_string(),
            output,
            session_id: Some(resolved_session_id.clone()),
            session_title: Some(session.title.clone()),
        },
        Err(err) => {
            let error_text = err.to_string();
            let phase = if error_text.starts_with("executor plan validation failed:") {
                Some("admin_validate")
            } else {
                None
            };
            ChatResponse {
                status: "error".to_string(),
                mode: "executor".to_string(),
                output: json!({
                    "error": error_text,
                    "execution_id": execution_id,
                    "phase": phase,
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            }
        }
    };

    let plan_text = render_executor_plan_submission(&execution_id, &req.plan);
    if let Err(err) = persist_chat_exchange(state, &mut session, plan_text, &[], &response).await {
        tracing::warn!(error = %err, session_id = %resolved_session_id, "failed to persist executor plan exchange");
    }

    ChatResponse {
        session_title: Some(session.title),
        ..response
    }
}

async fn handle_ai_chat(
    state: &ArchitectState,
    session: &ChatSessionRecord,
    input: &str,
    attachments: &[ChatAttachmentRef],
) -> Result<Value, ArchitectError> {
    let runtime = state
        .ai_runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> ArchitectError {
            "AI provider not configured. Chat remains available for SCMD system operations."
                .to_string()
                .into()
        })?;
    let tool_context = admin_tool_context(state, Some(&session.session_id));
    let tool_provider = ArchitectAdminReadToolsProvider::new(tool_context);
    let mut tools = FunctionToolRegistry::new();
    tool_provider
        .register_tools(&mut tools)
        .map_err(|err| -> ArchitectError {
            format!("AI tool registration failed: {err}").into()
        })?;
    let memory = build_session_immediate_memory(state, session).await?;

    let model = runtime.client.clone().function_model(
        runtime.model.clone(),
        Some(runtime.instructions.clone()),
        runtime.model_settings.clone(),
    );
    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let current_user_parts =
        if attachments.is_empty() {
            None
        } else {
            let payload = TextV1Payload::new(
                input,
                attachments
                    .iter()
                    .map(|attachment| attachment.blob_ref.clone())
                    .collect(),
            )
            .to_value()
            .map_err(|err| -> ArchitectError {
                format!("failed to build text payload with attachments: {err}").into()
            })?;
            let resolved = resolve_model_input_from_payload_with_options(
                &payload,
                &ModelInputOptions {
                    multimodal: true,
                    max_attachments: ARCHITECT_MAX_ATTACHMENTS,
                    max_attachment_bytes: ARCHITECT_MAX_ATTACHMENT_BYTES as u64,
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| -> ArchitectError {
                format!("failed to resolve attachment payload for model input: {err}").into()
            })?;
            Some(build_openai_user_content_parts(&resolved).await.map_err(
                |err| -> ArchitectError {
                    format!("failed to build OpenAI input parts from attachments: {err}").into()
                },
            )?)
        };

    let result = runner
        .run_with_input(
            &model,
            &tools,
            FunctionRunInput {
                current_user_message: input.to_string(),
                current_user_parts,
                immediate_memory: Some(memory),
            },
        )
        .await
        .map_err(|err| -> ArchitectError { format!("AI request failed: {err}").into() })?;
    let pending_confirmation_staged = state
        .pending_actions
        .lock()
        .await
        .contains_key(&session.session_id);

    let tool_results = result
        .items
        .iter()
        .filter_map(|item| match item {
            fluxbee_ai_sdk::FunctionLoopItem::ToolResult { result } => Some(json!({
                "call_id": result.call_id.clone(),
                "name": result.name.clone(),
                "arguments": result.arguments.clone(),
                "output": result.output.clone(),
                "is_error": result.is_error,
                "summary": summarize_tool_result(result.name.as_str(), &result.arguments, &result.output, result.is_error),
            })),
            _ => None,
        })
        .collect::<Vec<_>>();
    let raw_message = result
        .final_assistant_text
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| {
            if tool_results.is_empty() {
                "The AI run completed without a textual response.".to_string()
            } else {
                "I executed live system lookups, but the model returned no final text.".to_string()
            }
        });
    let message = if pending_confirmation_staged {
        latest_staged_confirmation_message(&result.items).unwrap_or(raw_message)
    } else if text_suggests_pending_confirmation(&raw_message) {
        if tool_results.iter().any(|result| {
            result
                .get("is_error")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        }) {
            "I could not prepare the mutation in this turn. Review the tool errors and try again."
                .to_string()
        } else {
            "The model suggested a confirmation, but no pending action was staged in this chat."
                .to_string()
        }
    } else {
        raw_message
    };

    Ok(json!({
        "message": message,
        "provider": "openai",
        "model": runtime.model,
        "tool_results": tool_results,
    }))
}

async fn handle_impersonation_chat(
    state: &ArchitectState,
    session: &ChatSessionRecord,
    input: &str,
) -> Result<Value, ArchitectError> {
    let sender = state
        .router_sender
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> ArchitectError {
            "Router is not connected, so the impersonated message could not be dispatched."
                .to_string()
                .into()
        })?;
    let effective_ilk =
        none_if_empty(&session.effective_ilk).ok_or_else(|| -> ArchitectError {
            "Impersonation dispatch requires an effective_ilk in the session profile."
                .to_string()
                .into()
        })?;
    let thread_id = none_if_empty(&session.thread_id).ok_or_else(|| -> ArchitectError {
        "Impersonation dispatch requires a thread_id so the downstream node can keep continuity."
            .to_string()
            .into()
    })?;
    let effective_ich_id = none_if_empty(&session.effective_ich_id);
    let source_channel_kind = none_if_empty(&session.source_channel_kind);
    let impersonation_target = none_if_empty(&session.impersonation_target);
    let trace_id = Uuid::new_v4().to_string();

    let mut context = serde_json::Map::new();
    context.insert("thread_id".to_string(), Value::String(thread_id.clone()));
    context.insert(
        "chat_mode".to_string(),
        Value::String(CHAT_MODE_IMPERSONATION.to_string()),
    );
    context.insert(
        "session_id".to_string(),
        Value::String(session.session_id.clone()),
    );
    context.insert("src_ilk".to_string(), Value::String(effective_ilk.clone()));
    if let Some(value) = effective_ich_id.clone() {
        context.insert("effective_ich_id".to_string(), Value::String(value.clone()));
        context.insert("ich_id".to_string(), Value::String(value));
    }
    if let Some(value) = source_channel_kind.clone() {
        context.insert("source_channel_kind".to_string(), Value::String(value));
    }
    if let Some(value) = impersonation_target.clone() {
        context.insert("impersonation_target".to_string(), Value::String(value));
    }

    let payload =
        TextV1Payload::new(input, vec![])
            .to_value()
            .map_err(|err| -> ArchitectError {
                format!("failed to build impersonation text payload: {err}").into()
            })?;
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Resolve,
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            src_ilk: Some(effective_ilk.clone()),
            ich: effective_ich_id.clone(),
            thread_id: Some(thread_id.clone()),
            scope: effective_ich_id.clone(),
            target: impersonation_target.clone(),
            action: None,
            priority: None,
            context: Some(Value::Object(context)),
            ..Meta::default()
        },
        payload,
    };
    sender.send(msg).await.map_err(|err| -> ArchitectError {
        format!("failed to dispatch impersonation message via router: {err}").into()
    })?;

    Ok(json!({
        "suppress_echo": true,
        "suppress_response_row": true,
        "dispatch": {
            "status": "sent",
            "trace_id": trace_id,
            "thread_id": thread_id,
            "effective_ilk": effective_ilk,
            "effective_ich_id": effective_ich_id,
            "source_channel_kind": source_channel_kind,
            "impersonation_target": impersonation_target,
            "route": "resolve",
            "reply_capture": "session_persisted",
        }
    }))
}

fn router_message_session_id(msg: &Message) -> Option<String> {
    msg.meta
        .context
        .as_ref()
        .and_then(|ctx| ctx.get("session_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn router_message_label(msg: &Message) -> String {
    if msg.meta.msg_type.eq_ignore_ascii_case("system") {
        return "System".to_string();
    }
    msg.meta
        .src_ilk
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "archi".to_string())
}

fn router_message_text(msg: &Message) -> String {
    if msg.meta.msg_type.eq_ignore_ascii_case("system")
        && msg.meta.msg.as_deref() == Some("UNREACHABLE")
    {
        let reason = msg
            .payload
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let original_dst = msg
            .payload
            .get("original_dst")
            .and_then(Value::as_str)
            .unwrap_or("resolve");
        return format!(
            "Router returned UNREACHABLE: reason={reason}, original_dst={original_dst}"
        );
    }
    if let Some(text) = extract_text(&msg.payload)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return text;
    }
    if let Some(text) = msg
        .payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return text.to_string();
    }
    serde_json::to_string_pretty(&msg.payload)
        .or_else(|_| serde_json::to_string(&msg.payload))
        .unwrap_or_else(|_| "{\"error\":\"unserializable router payload\"}".to_string())
}

async fn persist_router_incoming_message(
    state: &ArchitectState,
    session_id: &str,
    msg: &Message,
) -> Result<(), ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let profiles = ensure_session_profiles_table(&db).await?;
    let messages = ensure_messages_table(&db).await?;
    let Some(mut session) = load_session_record(&sessions, &profiles, session_id).await? else {
        return Ok(());
    };
    if session.chat_mode != CHAT_MODE_IMPERSONATION {
        return Ok(());
    }

    let now = now_epoch_ms();
    let content = router_message_text(msg);
    let label = router_message_label(msg);
    let role = if msg.meta.msg_type.eq_ignore_ascii_case("system") {
        "system"
    } else {
        "architect"
    };
    let row = ChatMessageRecord {
        message_id: Uuid::new_v4().to_string(),
        session_id: session.session_id.clone(),
        role: role.to_string(),
        content: content.clone(),
        timestamp_ms: now,
        mode: "chat".to_string(),
        metadata_json: json!({
            "kind": "router_message",
            "label": label,
            "trace_id": msg.routing.trace_id,
            "routing_src": msg.routing.src,
            "src_ilk": msg.meta.src_ilk,
            "scope": msg.meta.scope,
            "target": msg.meta.target,
            "context": msg.meta.context,
            "payload": msg.payload,
        })
        .to_string(),
        seq: session.message_count + 1,
    };
    append_message_record(&messages, &row).await?;
    session.message_count += 1;
    session.last_activity_at_ms = now;
    session.last_message_preview = preview_text(&content, 88);
    upsert_session_record(&sessions, &session).await?;
    Ok(())
}

fn latest_staged_confirmation_message(
    items: &[fluxbee_ai_sdk::FunctionLoopItem],
) -> Option<String> {
    items.iter().rev().find_map(|item| match item {
        fluxbee_ai_sdk::FunctionLoopItem::ToolResult { result }
            if !result.is_error && tool_output_requests_confirmation(&result.output) =>
        {
            result
                .output
                .get("message")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
        }
        _ => None,
    })
}

fn tool_output_requests_confirmation(output: &Value) -> bool {
    output
        .get("pending_confirmation")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        || output
            .get("requires_confirmation")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

fn text_suggests_pending_confirmation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("confirm")
        && (lower.contains("cancel")
            || lower.contains("prepared")
            || lower.contains("pend")
            || lower.contains("abort"))
}

fn summarize_tool_result(name: &str, arguments: &Value, output: &Value, is_error: bool) -> String {
    let arguments_summary = summarize_tool_value(arguments, 120);
    let output_summary = summarize_tool_value(output, 160);
    if is_error {
        format!("{name} failed | input: {arguments_summary} | output: {output_summary}")
    } else {
        format!("{name} ok | input: {arguments_summary} | output: {output_summary}")
    }
}

fn summarize_tool_value(value: &Value, max_chars: usize) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(text) => preview_text(text, max_chars),
        other => {
            let serialized = serde_json::to_string(other)
                .unwrap_or_else(|_| "{\"error\":\"unserializable\"}".to_string());
            preview_text(&serialized, max_chars)
        }
    }
}

async fn build_session_immediate_memory(
    state: &ArchitectState,
    session: &ChatSessionRecord,
) -> Result<ImmediateConversationMemory, ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let messages_table = ensure_messages_table(&db).await?;
    let operations_table = ensure_operations_table(&db).await?;
    let recent_messages = load_session_messages(&messages_table, &session.session_id).await?;
    let operations = load_session_operations(&operations_table, &session.session_id).await?;
    let summary = build_conversation_summary(session, &recent_messages, &operations);

    Ok(ImmediateConversationMemory {
        thread_id: none_if_empty(&session.thread_id),
        scope_id: session_scope_id(session),
        summary: Some(summary),
        recent_interactions: recent_messages_to_immediate(recent_messages),
        active_operations: operations_to_immediate(&session.session_id, operations),
    })
}

fn build_conversation_summary(
    session: &ChatSessionRecord,
    messages: &[PersistedChatMessage],
    operations: &[ChatOperationRecord],
) -> ConversationSummary {
    let goal = if session_title_is_generic(session) {
        messages
            .iter()
            .rev()
            .filter(|message| message.role == "user")
            .filter_map(message_focus_text)
            .next()
    } else {
        Some(preview_text(&session.title, 180))
    };
    let current_focus = operations
        .iter()
        .find(|record| !is_terminal_operation_status(&record.status))
        .map(operation_focus_summary)
        .or_else(|| messages.iter().rev().filter_map(message_focus_text).next());

    let mut decisions = Vec::new();
    if session.chat_mode == CHAT_MODE_IMPERSONATION {
        decisions.push(
            "This conversation runs in impersonation/debug mode and may simulate another ILK or node context."
                .to_string(),
        );
    }
    if operations
        .iter()
        .any(|record| record.status == "pending_confirm")
    {
        decisions.push(
            "There are staged mutations in this conversation that require CONFIRM or CANCEL."
                .to_string(),
        );
    }
    if operations
        .iter()
        .any(|record| record.status == "timeout_unknown")
    {
        decisions.push(
            "Timeout-unknown operations should be inspected before retrying the same mutation."
                .to_string(),
        );
    }

    let mut confirmed_facts = Vec::new();
    if let Some(effective_ich_id) = none_if_empty(&session.effective_ich_id) {
        confirmed_facts.push(format!("Effective ICH context: {effective_ich_id}"));
    }
    if let Some(effective_ilk) = none_if_empty(&session.effective_ilk) {
        confirmed_facts.push(format!("Effective ILK context: {effective_ilk}"));
    }
    if let Some(impersonation_target) = none_if_empty(&session.impersonation_target) {
        confirmed_facts.push(format!("Impersonation target: {impersonation_target}"));
    }
    if let Some(source_channel_kind) = none_if_empty(&session.source_channel_kind) {
        confirmed_facts.push(format!("Source channel kind: {source_channel_kind}"));
    }
    if let Some(thread_id) = none_if_empty(&session.thread_id) {
        confirmed_facts.push(format!("Thread context: {thread_id}"));
    }

    confirmed_facts.extend(
        operations
            .iter()
            .filter(|record| {
                matches!(
                    record.status.as_str(),
                    "succeeded" | "succeeded_after_timeout"
                )
            })
            .take(3)
            .map(|record| {
                format!(
                    "Recent operation succeeded: {}",
                    operation_focus_summary(record)
                )
            })
            .collect::<Vec<_>>(),
    );

    let open_questions = operations
        .iter()
        .filter(|record| matches!(record.status.as_str(), "timeout_unknown" | "failed"))
        .take(3)
        .map(|record| {
            if record.error_summary.trim().is_empty() {
                format!(
                    "Review operation state: {}",
                    operation_focus_summary(record)
                )
            } else {
                format!(
                    "{}: {}",
                    operation_focus_summary(record),
                    preview_text(&record.error_summary, 180)
                )
            }
        })
        .collect::<Vec<_>>();

    ConversationSummary {
        goal,
        current_focus,
        decisions,
        confirmed_facts,
        open_questions,
    }
}

fn recent_messages_to_immediate(messages: Vec<PersistedChatMessage>) -> Vec<ImmediateInteraction> {
    let mut interactions = messages
        .iter()
        .filter_map(immediate_interaction_from_message)
        .collect::<Vec<_>>();
    let start = interactions.len().saturating_sub(10);
    interactions.drain(0..start);
    interactions
}

fn session_title_is_generic(session: &ChatSessionRecord) -> bool {
    matches!(
        session.title.trim(),
        "" | "Untitled chat" | "Impersonation chat" | "Operator chat"
    )
}

fn message_focus_text(message: &PersistedChatMessage) -> Option<String> {
    immediate_interaction_from_message(message)
        .map(|interaction| interaction.content)
        .filter(|content| !is_low_signal_control_text(content))
        .map(|content| preview_text(&content, 180))
}

fn persisted_message_attachments(message: &PersistedChatMessage) -> Vec<UploadedAttachment> {
    message
        .metadata
        .get("attachments")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<UploadedAttachment>(item.clone()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn render_user_message_with_attachment_summary(
    content: &str,
    attachments: &[ChatAttachmentRef],
) -> String {
    if attachments.is_empty() {
        return content.to_string();
    }
    let mut lines = Vec::new();
    if !content.trim().is_empty() {
        lines.push(content.trim().to_string());
    }
    lines.extend(attachments.iter().map(|attachment| {
        format!(
            "Attachment: {} ({}, {} bytes)",
            attachment.blob_ref.filename_original,
            attachment.blob_ref.mime,
            attachment.blob_ref.size
        )
    }));
    lines.join("\n")
}

fn render_persisted_user_message(message: &PersistedChatMessage) -> String {
    let attachments = persisted_message_attachments(message);
    if attachments.is_empty() {
        return message.content.clone();
    }
    let mut lines = Vec::new();
    if !message.content.trim().is_empty() {
        lines.push(message.content.trim().to_string());
    }
    lines.extend(attachments.iter().map(|attachment| {
        format!(
            "Attachment: {} ({}, {} bytes)",
            attachment.filename, attachment.mime, attachment.size
        )
    }));
    lines.join("\n")
}

fn is_low_signal_control_text(content: &str) -> bool {
    matches!(
        content.trim().to_ascii_uppercase().as_str(),
        "CONFIRM" | "OK CONFIRM" | "CANCEL" | "ABORT"
    )
}

fn immediate_interaction_from_message(
    message: &PersistedChatMessage,
) -> Option<ImmediateInteraction> {
    let role = match message.role.as_str() {
        "user" => ImmediateRole::User,
        "architect" => ImmediateRole::Assistant,
        _ => ImmediateRole::System,
    };

    if let Some(response) = message.metadata.get("response") {
        if let Some(interaction) = immediate_interaction_from_response(
            role.clone(),
            response,
            &message.content,
            &message.mode,
        ) {
            return Some(interaction);
        }
    }

    let rendered_content = render_persisted_user_message(message);
    let content = rendered_content.trim();
    if content.is_empty() {
        return None;
    }

    let kind = match role {
        ImmediateRole::System => ImmediateInteractionKind::SystemNote,
        _ => ImmediateInteractionKind::Text,
    };

    Some(ImmediateInteraction {
        role,
        kind,
        content: content.to_string(),
    })
}

fn immediate_interaction_from_response(
    role: ImmediateRole,
    response: &Value,
    fallback_content: &str,
    fallback_mode: &str,
) -> Option<ImmediateInteraction> {
    let mode = response
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or(fallback_mode);
    let status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let output = response.get("output");

    if mode == "chat" {
        if let Some(message) = output
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                let content = fallback_content.trim();
                (!content.is_empty()).then_some(content)
            })
        {
            return Some(ImmediateInteraction {
                role,
                kind: ImmediateInteractionKind::Text,
                content: message.to_string(),
            });
        }

        let tool_names = output
            .and_then(|value| value.get("tool_results"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !tool_names.is_empty() {
            return Some(ImmediateInteraction {
                role: ImmediateRole::System,
                kind: ImmediateInteractionKind::ToolResult,
                content: format!("AI used live system tools: {}", tool_names.join(", ")),
            });
        }

        return None;
    }

    if mode == "scmd" || mode == "acmd" {
        let action = output
            .and_then(|value| value.get("action"))
            .and_then(Value::as_str)
            .unwrap_or("system_command");
        let detail = output.and_then(response_detail_text).or_else(|| {
            let content = fallback_content.trim();
            (!content.is_empty()).then_some(content.to_string())
        });
        let kind = if status.eq_ignore_ascii_case("ok") {
            ImmediateInteractionKind::ToolResult
        } else {
            ImmediateInteractionKind::ToolError
        };
        let content = if let Some(detail) = detail {
            if status.eq_ignore_ascii_case("ok") {
                format!("System command {action}: {detail}")
            } else {
                format!("System command {action} failed: {detail}")
            }
        } else if status.eq_ignore_ascii_case("ok") {
            format!("System command {action} completed.")
        } else {
            format!("System command {action} failed.")
        };

        return Some(ImmediateInteraction {
            role: ImmediateRole::System,
            kind,
            content,
        });
    }

    let content = fallback_content.trim();
    if content.is_empty() {
        None
    } else {
        Some(ImmediateInteraction {
            role,
            kind: if status.eq_ignore_ascii_case("error") {
                ImmediateInteractionKind::ToolError
            } else {
                ImmediateInteractionKind::SystemNote
            },
            content: content.to_string(),
        })
    }
}

fn response_detail_text(output: &Value) -> Option<String> {
    output
        .get("error_detail")
        .and_then(value_to_compact_text)
        .or_else(|| output.get("message").and_then(value_to_compact_text))
        .or_else(|| {
            output
                .get("payload")
                .and_then(|value| value.get("message"))
                .and_then(value_to_compact_text)
        })
        .or_else(|| {
            output
                .get("payload")
                .and_then(|value| value.get("error_code"))
                .and_then(value_to_compact_text)
        })
}

fn value_to_compact_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| preview_text(trimmed, 220))
        }
        other => {
            let serialized = serde_json::to_string(other).ok()?;
            let trimmed = serialized.trim();
            (!trimmed.is_empty()).then(|| preview_text(trimmed, 220))
        }
    }
}

fn operations_to_immediate(
    session_id: &str,
    operations: Vec<ChatOperationRecord>,
) -> Vec<ImmediateOperation> {
    let mut active = operations
        .iter()
        .filter(|record| !is_terminal_operation_status(&record.status))
        .take(8)
        .map(|record| operation_record_to_immediate(session_id, record))
        .collect::<Vec<_>>();

    if active.len() < 8 {
        for record in operations
            .iter()
            .filter(|record| is_terminal_operation_status(&record.status))
            .take(8 - active.len())
        {
            active.push(operation_record_to_immediate(session_id, record));
        }
    }

    active
}

fn operation_record_to_immediate(
    session_id: &str,
    record: &ChatOperationRecord,
) -> ImmediateOperation {
    ImmediateOperation {
        operation_id: record.operation_id.clone(),
        resource_scope: Some(operation_resource_scope(record)),
        origin_thread_id: None,
        origin_session_id: Some(session_id.to_string()),
        scope_id: if record.scope_id.trim().is_empty() {
            None
        } else {
            Some(record.scope_id.clone())
        },
        action: record.action.clone(),
        target: operation_target(record),
        status: record.status.clone(),
        summary: operation_model_summary(record),
        created_at_ms: Some(record.created_at_ms),
        updated_at_ms: Some(record.updated_at_ms),
    }
}

fn operation_resource_scope(record: &ChatOperationRecord) -> String {
    if record.action == "add_hive" {
        if let Ok(params) = serde_json::from_str::<Value>(&record.params_json) {
            if let Some(hive_id) = params.get("hive_id").and_then(Value::as_str) {
                return format!("hive:{hive_id}");
            }
        }
    }
    format!("hive:{}", record.target_hive)
}

fn operation_target(record: &ChatOperationRecord) -> Option<String> {
    if record.action == "add_hive" {
        if let Ok(params) = serde_json::from_str::<Value>(&record.params_json) {
            if let Some(hive_id) = params.get("hive_id").and_then(Value::as_str) {
                return Some(hive_id.to_string());
            }
        }
    }
    if record.target_hive.trim().is_empty() {
        None
    } else {
        Some(record.target_hive.clone())
    }
}

fn operation_focus_summary(record: &ChatOperationRecord) -> String {
    let target = operation_target(record).unwrap_or_else(|| record.target_hive.clone());
    if target.trim().is_empty() {
        format!("{} [{}]", record.action, record.status)
    } else {
        format!("{} {} [{}]", record.action, target, record.status)
    }
}

fn operation_model_summary(record: &ChatOperationRecord) -> String {
    match record.status.as_str() {
        "pending_confirm" => format!(
            "Prepared and waiting for confirmation: {}",
            record.preview_command
        ),
        "dispatched" | "running" => format!("Running: {}", operation_focus_summary(record)),
        "timeout_unknown" => format!(
            "Timed out locally and may still be running: {}",
            operation_focus_summary(record)
        ),
        "failed" if !record.error_summary.trim().is_empty() => {
            format!("Failed: {}", preview_text(&record.error_summary, 180))
        }
        _ => operation_focus_summary(record),
    }
}

async fn handle_scmd(
    state: &ArchitectState,
    session_id: &str,
    raw: &str,
) -> Result<Value, ArchitectError> {
    if let Some(output) = handle_meta_scmd(raw) {
        return Ok(output);
    }
    let parsed = parse_scmd(raw)?;
    if let Some(output) = handle_local_architect_scmd(state, &parsed).await? {
        return Ok(output);
    }
    let translated = translate_scmd(&state.hive_id, parsed)?;
    if is_mutating_admin_action(&translated.action) {
        execute_tracked_admin_translation(
            state,
            session_id,
            "scmd",
            translated,
            format!("SCMD: {raw}"),
            None,
        )
        .await
    } else {
        execute_admin_translation(state, translated).await
    }
}

fn handle_meta_scmd(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if !trimmed.eq_ignore_ascii_case("help")
        && !trimmed.eq_ignore_ascii_case("?")
        && !trimmed.eq_ignore_ascii_case("scmd help")
    {
        return None;
    }

    Some(json!({
        "status": "ok",
        "action": "scmd_help",
        "payload": {
            "syntax": "SCMD: curl -X METHOD /relative/path [-d '{...}']",
            "notes": [
                "Usa siempre paths relativos a SY.admin, por ejemplo /hives/motherbee/nodes.",
                "Para descubrir el catÃ¡logo dinÃ¡mico completo, usÃ¡ GET /admin/actions.",
                "Para ayuda detallada de una acciÃ³n, usÃ¡ GET /admin/actions/{action}."
            ],
            "examples": [
                "SCMD: curl -X GET /admin/actions",
                "SCMD: curl -X GET /hive/status",
                "SCMD: curl -X GET /hives",
                "SCMD: curl -X GET /inventory/summary",
                "SCMD: curl -X GET /hives/motherbee/versions",
                "SCMD: curl -X GET /hives/motherbee/runtimes/AI.chat",
                "SCMD: curl -X GET /hives/motherbee/nodes",
                "SCMD: curl -X GET /hives/motherbee/nodes/AI.chat@motherbee/status",
                "SCMD: curl -X GET /hives/motherbee/nodes/SY.frontdesk.gov@motherbee/config",
                "SCMD: curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/control/config-get -d '{\"requested_by\":\"archi\"}'",
                "SCMD: curl -X GET /hives/motherbee/identity/ilks",
                "SCMD: curl -X GET /hives/motherbee/deployments",
                "SCMD: curl -X GET /hives/motherbee/drift-alerts",
                "SCMD: curl -X GET /hives/motherbee/timer/help",
                "SCMD: curl -X GET /hives/motherbee/timer/timers?status_filter=pending&limit=20",
                "SCMD: curl -X GET /hives/motherbee/timer/timers/9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001",
                "SCMD: curl -X GET /hives/motherbee/timer/now",
                "SCMD: curl -X GET /config/storage"
            ],
            "mutation_examples": [
                "SCMD: curl -X POST /hives -d '{\"hive_id\":\"worker-220\",\"address\":\"192.168.8.220\"}'",
                "SCMD: curl -X POST /hives/motherbee/sync-hint -d '{\"channel\":\"blob\",\"wait_for_idle\":true,\"timeout_ms\":30000}'",
                "SCMD: curl -X POST /hives/motherbee/nodes -d '{\"node_name\":\"AI.chat@motherbee\",\"runtime_version\":\"current\"}'",
                "SCMD: curl -X PUT /hives/motherbee/nodes/SY.frontdesk.gov@motherbee/config -d '{\"openai\":{\"default_model\":\"gpt-4.1-mini\"}}'"
            ]
        }
    }))
}

async fn handle_local_architect_scmd(
    state: &ArchitectState,
    parsed: &ParsedScmd,
) -> Result<Option<Value>, ArchitectError> {
    let segments: Vec<&str> = parsed
        .path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match (parsed.method.as_str(), segments.as_slice()) {
        ("GET", ["architect", "control", "config-get"]) => {
            Ok(Some(handle_architect_local_config_get(state).await?))
        }
        ("POST", ["architect", "control", "config-set"]) => {
            let body = parsed.body.clone().unwrap_or_else(|| json!({}));
            if !body.is_object() {
                return Err(
                    "SCMD body for architect local config-set must be a JSON object".into(),
                );
            }
            Ok(Some(handle_architect_local_config_set(state, body).await?))
        }
        _ => Ok(None),
    }
}

async fn handle_architect_local_config_get(
    state: &ArchitectState,
) -> Result<Value, ArchitectError> {
    let hive = load_hive(&state.config_dir)?;
    let node_config = load_architect_node_config(&state.hive_id)?;
    let secret_record = load_architect_secret_record(&state.node_name)?;
    let merged = merged_openai_section(node_config.as_ref(), &hive, secret_record.as_ref());
    let configured = merged
        .as_ref()
        .and_then(|openai| openai.api_key.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let api_key_source = if secret_record
        .as_ref()
        .and_then(|record| record.secrets.get(ARCHITECT_LOCAL_SECRET_KEY_OPENAI))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        "local_file"
    } else {
        "missing"
    };
    let descriptor = NodeSecretDescriptor {
        field: "ai_providers.openai.api_key".to_string(),
        storage_key: ARCHITECT_LOCAL_SECRET_KEY_OPENAI.to_string(),
        required: true,
        configured,
        value_redacted: true,
        persistence: "local_file".to_string(),
    };
    Ok(json!({
        "status": "ok",
        "action": "architect_local_config_get",
        "payload": {
            "ok": true,
            "node_name": state.node_name,
            "config_version": 1,
            "state": if configured { "configured" } else { "missing_secret" },
            "config": {
                "ai_providers": {
                    "openai": {
                        "api_key": if configured { Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()) } else { Value::Null },
                        "default_model": merged.as_ref().and_then(|openai| openai.default_model.clone()).map(Value::String).unwrap_or(Value::Null),
                        "max_tokens": merged.as_ref().and_then(|openai| openai.max_tokens.map(Value::from)).unwrap_or(Value::Null),
                        "temperature": merged.as_ref().and_then(|openai| openai.temperature.map(Value::from)).unwrap_or(Value::Null),
                        "top_p": merged.as_ref().and_then(|openai| openai.top_p.map(Value::from)).unwrap_or(Value::Null)
                    }
                }
            },
            "contract": {
                "node_family": "SY",
                "node_kind": "SY.architect",
                "supports": ["CONFIG_GET", "CONFIG_SET"],
                "required_fields": ["config.ai_providers.openai.api_key"],
                "optional_fields": [],
                "secrets": [descriptor],
                "notes": [
                    "This local control path bootstraps archi's own OpenAI key only from local secrets.json.",
                    "Secret values are persisted in local secrets.json and always returned redacted."
                ]
            },
            "secret_record": secret_record.map(|record| redacted_node_secret_record(&record)),
            "api_key_source": api_key_source
        }
    }))
}

async fn handle_architect_local_config_set(
    state: &ArchitectState,
    body: Value,
) -> Result<Value, ArchitectError> {
    let api_key = extract_architect_openai_api_key(&body)?.ok_or_else(|| -> ArchitectError {
        "architect local config-set requires config.ai_providers.openai.api_key".into()
    })?;
    let updated_by_ilk = body
        .get("updated_by_ilk")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let updated_by_label = body
        .get("updated_by_label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| Some("archi".to_string()));
    let trace_id = body
        .get("trace_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut secrets = load_architect_secret_record(&state.node_name)?
        .map(|record| record.secrets)
        .unwrap_or_default();
    secrets.insert(
        ARCHITECT_LOCAL_SECRET_KEY_OPENAI.to_string(),
        Value::String(api_key),
    );
    let record = build_node_secret_record(
        secrets,
        &NodeSecretWriteOptions {
            updated_by_ilk,
            updated_by_label,
            trace_id: Some(trace_id.clone()),
        },
    );
    let path = save_node_secret_record_with_root(&state.node_name, architect_nodes_root(), &record)
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let ai_configured = refresh_architect_ai_runtime(state).await?;

    Ok(json!({
        "status": "ok",
        "action": "architect_local_config_set",
        "payload": {
            "ok": true,
            "node_name": state.node_name,
            "state": if ai_configured { "configured" } else { "missing_secret" },
            "trace_id": trace_id,
            "ai_configured": ai_configured,
            "persisted_path": path,
            "stored_secrets": [{
                "field": "ai_providers.openai.api_key",
                "storage_key": ARCHITECT_LOCAL_SECRET_KEY_OPENAI,
                "value_redacted": true
            }],
            "message": "Architect local OpenAI key stored in secrets.json and runtime reloaded."
        }
    }))
}

fn extract_architect_openai_api_key(body: &Value) -> Result<Option<String>, ArchitectError> {
    if let Some(value) = body
        .get("config")
        .and_then(|config| config.get("ai_providers"))
        .and_then(|providers| providers.get("openai"))
        .and_then(|openai| openai.get("api_key"))
        .and_then(Value::as_str)
    {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    if let Some(value) = body.get("api_key").and_then(Value::as_str) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Ok(Some(trimmed.to_string()));
        }
    }
    Ok(None)
}

fn scmd_raw_from_parts(method: &str, path: &str, body: Option<&Value>) -> String {
    match body {
        Some(value) => format!(
            "curl -X {} {} -d '{}'",
            method,
            path,
            serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
        ),
        None => format!("curl -X {} {}", method, path),
    }
}

fn admin_action_allows_ai_write(action: &str) -> bool {
    matches!(
        action,
        "add_hive"
            | "remove_hive"
            | "add_route"
            | "delete_route"
            | "add_vpn"
            | "delete_vpn"
            | "run_node"
            | "kill_node"
            | "remove_node_instance"
            | "remove_runtime_version"
            | "set_node_config"
            | "node_control_config_set"
            | "send_node_message"
            | "set_storage"
            | "update"
            | "sync_hint"
            | "opa_compile_apply"
            | "opa_compile"
            | "opa_apply"
            | "opa_rollback"
            | "wf_rules_compile_apply"
            | "wf_rules_compile"
            | "wf_rules_apply"
            | "wf_rules_rollback"
            | "wf_rules_delete"
    )
}

async fn reconcile_timeout_unknown_operation(
    state: &ArchitectState,
    record: &mut ChatOperationRecord,
) -> Result<bool, ArchitectError> {
    if record.status != "timeout_unknown" {
        return Ok(false);
    }

    if record.action == "add_hive" {
        let params: Value = serde_json::from_str(&record.params_json)
            .unwrap_or_else(|_| Value::Object(Default::default()));
        if let Some(hive_id) = params.get("hive_id").and_then(Value::as_str) {
            let output = execute_admin_translation(
                state,
                AdminTranslation {
                    admin_target: format!("SY.admin@{}", state.hive_id),
                    action: "get_hive".to_string(),
                    target_hive: state.hive_id.clone(),
                    params: json!({ "hive_id": hive_id }),
                },
            )
            .await?;
            if output
                .get("status")
                .and_then(Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("ok"))
                .unwrap_or(false)
            {
                record.status = "succeeded_after_timeout".to_string();
                record.updated_at_ms = now_epoch_ms();
                record.completed_at_ms = record.updated_at_ms;
                record.error_summary.clear();
                save_operation(state, record).await?;
                return Ok(true);
            }
        }
    }

    Ok(false)
}

async fn fetch_admin_action_contract(
    context: &ArchitectAdminToolContext,
    action_name: &str,
) -> Result<Value, ArchitectError> {
    let output = execute_admin_translation_with_context(
        context,
        AdminTranslation {
            admin_target: format!("SY.admin@{}", context.hive_id),
            action: "get_admin_action_help".to_string(),
            target_hive: context.hive_id.clone(),
            params: json!({ "action_name": action_name }),
        },
        "tool.help",
    )
    .await?;

    output
        .get("payload")
        .and_then(|payload| payload.get("entry"))
        .and_then(|entry| entry.get("request_contract"))
        .cloned()
        .ok_or_else(|| format!("missing request_contract for admin action '{action_name}'").into())
}

async fn validate_mutating_translation_contract(
    context: &ArchitectAdminToolContext,
    translation: &AdminTranslation,
) -> Result<(), fluxbee_ai_sdk::AiSdkError> {
    let contract = fetch_admin_action_contract(context, &translation.action)
        .await
        .map_err(|err| {
            fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "failed to fetch admin action contract: {err}"
            ))
        })?;
    let body = contract.get("body").cloned().unwrap_or_else(|| json!({}));
    let required = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let required_fields = body
        .get("required_fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if required_fields.is_empty() && !required {
        return Ok(());
    }
    let params = translation.params.as_object().ok_or_else(|| {
        fluxbee_ai_sdk::AiSdkError::Protocol(format!(
            "action '{}' requires a JSON object body",
            translation.action
        ))
    })?;
    let missing = required_fields
        .iter()
        .filter_map(|field| field.get("name").and_then(Value::as_str))
        .filter(|name| match params.get(*name) {
            Some(Value::Null) | None => true,
            Some(Value::String(value)) if value.trim().is_empty() => true,
            _ => false,
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
            "action '{}' is missing required fields from SY.admin request_contract: {}",
            translation.action,
            missing.join(", ")
        )));
    }
    Ok(())
}

fn operation_status_from_output(output: &Value) -> String {
    match output
        .get("status")
        .and_then(Value::as_str)
        .map(|value| value.to_ascii_lowercase())
    {
        Some(status) if status == "ok" => "succeeded".to_string(),
        Some(status) if status == "not_found" => "not_found".to_string(),
        Some(status) if status == "error" => "failed".to_string(),
        Some(status) => status,
        None => "succeeded".to_string(),
    }
}

fn parse_scmd(raw: &str) -> Result<ParsedScmd, ArchitectError> {
    let mut text = raw.trim();
    if !text.starts_with("curl ") {
        return Err("SCMD must start with 'curl '".into());
    }
    let mut body = None;
    if let Some((head, body_raw)) = text.rsplit_once(" -d ") {
        text = head.trim();
        let body_raw = strip_wrapping_quotes(body_raw.trim());
        body = Some(serde_json::from_str(body_raw)?);
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() < 4 || tokens[0] != "curl" || tokens[1] != "-X" {
        return Err("SCMD syntax must be: curl -X METHOD /relative/path [-d '{...}']".into());
    }
    let method = tokens[2].trim().to_ascii_uppercase();
    let path = tokens[3].trim().to_string();
    if !path.starts_with('/') {
        return Err("SCMD path must be relative and start with '/'".into());
    }
    Ok(ParsedScmd { method, path, body })
}

fn chat_status_from_command_output(output: &Value) -> String {
    output
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ok")
        .to_string()
}

fn is_mutating_admin_action(action: &str) -> bool {
    admin_action_allows_ai_write(action)
}

fn operation_scope_id(session_id: &str) -> String {
    format!("chat:{session_id}")
}

fn normalize_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(normalize_json).collect()),
        Value::Object(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                if let Some(entry) = map.get(&key) {
                    out.insert(key, normalize_json(entry));
                }
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn params_hash(value: &Value) -> String {
    let normalized = serde_json::to_string(&normalize_json(value)).unwrap_or_else(|_| "{}".into());
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn is_terminal_operation_status(status: &str) -> bool {
    matches!(
        status,
        "succeeded" | "succeeded_after_timeout" | "failed" | "canceled" | "replaced" | "not_found"
    )
}

fn is_ambiguous_operation_status(status: &str) -> bool {
    matches!(status, "dispatched" | "running" | "timeout_unknown")
}

fn operation_error_message(record: &ChatOperationRecord) -> String {
    match record.status.as_str() {
        "pending_confirm" => format!(
            "There is already a prepared operation waiting for confirmation in this chat: {}",
            record.preview_command
        ),
        "dispatched" | "running" => format!(
            "An equivalent operation is already running in this chat (operation {}). Wait for it to finish before retrying.",
            record.operation_id
        ),
        "timeout_unknown" => format!(
            "An equivalent operation timed out locally but may still be running (operation {}). Inspect the result before retrying.",
            record.operation_id
        ),
        _ => format!(
            "An equivalent operation is already tracked in this chat (operation {}).",
            record.operation_id
        ),
    }
}

fn env_timeout_secs(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
}

fn scmd_query_params(raw_query: Option<&str>) -> serde_json::Map<String, Value> {
    let mut params = serde_json::Map::new();
    let Some(query) = raw_query else {
        return params;
    };
    for pair in query.split('&').filter(|segment| !segment.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some(parts) => parts,
            None => continue,
        };
        if key.is_empty() || value.is_empty() {
            continue;
        }
        params.insert(key.to_string(), Value::String(value.to_string()));
    }
    params
}

fn architect_admin_action_timeout(action: &str) -> Duration {
    match action {
        "executor_validate_plan" | "executor_execute_plan" => Duration::from_secs(
            env_timeout_secs("JSR_ADMIN_EXECUTOR_TIMEOUT_SECS").unwrap_or(120),
        ),
        "add_hive" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS").unwrap_or(180))
        }
        "update" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_UPDATE_TIMEOUT_SECS").unwrap_or(60))
        }
        "sync_hint" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_SYNC_HINT_TIMEOUT_SECS").unwrap_or(45))
        }
        "list_admin_actions" | "get_admin_action_help" | "hive_status" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_TIMEOUT_SECS").unwrap_or(5))
        }
        _ => Duration::from_secs(env_timeout_secs("JSR_ADMIN_ORCH_TIMEOUT_SECS").unwrap_or(30)),
    }
}

fn try_extract_executor_plan_json(message: &str) -> Option<Value> {
    let trimmed = message.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    if value.get("kind").and_then(Value::as_str) == Some("executor_plan") {
        Some(value)
    } else {
        None
    }
}

fn validate_architect_executor_plan_shape(plan: &Value) -> Result<Value, ArchitectError> {
    let obj = plan
        .as_object()
        .ok_or_else(|| -> ArchitectError { "executor plan must be a JSON object".into() })?;
    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| -> ArchitectError { "executor plan requires string field 'kind'".into() })?;
    if kind != "executor_plan" {
        return Err(format!("unsupported executor plan kind '{kind}'").into());
    }
    let version = obj
        .get("plan_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| -> ArchitectError {
            "executor plan requires string field 'plan_version'".into()
        })?;
    if version.is_empty() {
        return Err("executor plan plan_version must be non-empty".into());
    }
    let execution = obj
        .get("execution")
        .and_then(Value::as_object)
        .ok_or_else(|| -> ArchitectError { "executor plan requires object field 'execution'".into() })?;
    let steps = execution
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| -> ArchitectError {
            "executor plan execution.steps must be an array".into()
        })?;
    if steps.is_empty() {
        return Err("executor plan execution.steps must not be empty".into());
    }
    Ok(plan.clone())
}

fn render_executor_plan_submission(execution_id: &str, plan: &Value) -> String {
    let pretty = serde_json::to_string_pretty(plan)
        .unwrap_or_else(|_| serde_json::to_string(plan).unwrap_or_else(|_| "{}".to_string()));
    format!("EXECUTOR_PLAN {execution_id}\n{pretty}")
}

fn translate_scmd(
    local_hive_id: &str,
    parsed: ParsedScmd,
) -> Result<AdminTranslation, ArchitectError> {
    let (path_only, raw_query) = match parsed.path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (parsed.path.as_str(), None),
    };
    let segments: Vec<&str> = path_only
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let admin_target = format!("SY.admin@{local_hive_id}");

    match (parsed.method.as_str(), segments.as_slice()) {
        ("GET", ["admin", "actions"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_admin_actions".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("GET", ["admin", "actions", action_name]) => Ok(AdminTranslation {
            admin_target,
            action: "get_admin_action_help".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({ "action_name": action_name }),
        }),
        ("GET", ["hive", "status"]) => Ok(AdminTranslation {
            admin_target,
            action: "hive_status".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("GET", ["inventory"]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({ "scope": "global" }),
        }),
        ("GET", ["inventory", "summary"]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({ "scope": "summary" }),
        }),
        ("GET", ["inventory", "nodes"]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({ "scope": "global" }),
        }),
        ("GET", ["inventory", hive_id]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "scope": "hive", "filter_hive": hive_id }),
        }),
        ("GET", ["hives"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_hives".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("POST", ["hives"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for add_hive must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "add_hive".to_string(),
                target_hive: local_hive_id.to_string(),
                params,
            })
        }
        ("GET", ["versions"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_versions".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("GET", ["deployments"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_deployments".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("GET", ["drift-alerts"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_drift_alerts".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("GET", ["config", "storage"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_storage".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id]) => Ok(AdminTranslation {
            admin_target,
            action: "get_hive".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({ "hive_id": hive_id }),
        }),
        ("GET", ["hives", hive_id, "status"]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "scope": "hive", "filter_hive": hive_id }),
        }),
        ("DELETE", ["hives", hive_id]) => Ok(AdminTranslation {
            admin_target,
            action: "remove_hive".to_string(),
            target_hive: local_hive_id.to_string(),
            params: json!({ "hive_id": hive_id }),
        }),
        ("GET", ["hives", hive_id, "nodes"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_nodes".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("POST", ["hives", hive_id, "nodes"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for run_node must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "run_node".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("GET", ["hives", hive_id, "inventory", "summary"]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "scope": "summary" }),
        }),
        ("GET", ["hives", hive_id, "inventory", "hive"]) => Ok(AdminTranslation {
            admin_target,
            action: "inventory".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "scope": "hive", "filter_hive": hive_id }),
        }),
        ("GET", ["hives", hive_id, "nodes", node_name, "status"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_node_status".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "node_name": node_name }),
        }),
        ("GET", ["hives", hive_id, "nodes", node_name, "config"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_node_config".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "node_name": node_name }),
        }),
        ("GET", ["hives", hive_id, "nodes", node_name, "state"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_node_state".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "node_name": node_name }),
        }),
        ("GET", ["hives", hive_id, "identity", "ilks"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_ilks".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "identity", "ilks", ilk_id]) => Ok(AdminTranslation {
            admin_target,
            action: "get_ilk".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "ilk_id": ilk_id }),
        }),
        ("GET", ["hives", hive_id, "versions"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_versions".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "wf-rules"]) => {
            let params = Value::Object(scmd_query_params(raw_query));
            let action = if params
                .get("workflow_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
            {
                "wf_rules_get_workflow"
            } else {
                "wf_rules_list_workflows"
            };
            Ok(AdminTranslation {
                admin_target,
                action: action.to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("GET", ["hives", hive_id, "wf-rules", "status"]) => Ok(AdminTranslation {
            admin_target,
            action: "wf_rules_get_status".to_string(),
            target_hive: (*hive_id).to_string(),
            params: Value::Object(scmd_query_params(raw_query)),
        }),
        ("POST", ["hives", hive_id, "wf-rules"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for wf_rules compile_apply must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "wf_rules_compile_apply".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "wf-rules", "compile"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for wf_rules compile must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "wf_rules_compile".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "wf-rules", "apply"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for wf_rules apply must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "wf_rules_apply".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "wf-rules", "rollback"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for wf_rules rollback must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "wf_rules_rollback".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "wf-rules", "delete"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for wf_rules delete must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "wf_rules_delete".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("GET", ["hives", hive_id, "runtimes"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_runtimes".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "runtimes", runtime]) => Ok(AdminTranslation {
            admin_target,
            action: "get_runtime".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "runtime": runtime }),
        }),
        ("DELETE", ["hives", hive_id, "runtimes", runtime, "versions", version]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for remove_runtime_version must be a JSON object".into());
            }
            params["runtime"] = Value::String((*runtime).to_string());
            params["runtime_version"] = Value::String((*version).to_string());
            Ok(AdminTranslation {
                admin_target,
                action: "remove_runtime_version".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("GET", ["hives", hive_id, "routes"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_routes".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("POST", ["hives", hive_id, "routes"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for add_route must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "add_route".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("DELETE", ["hives", hive_id, "routes"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for delete_route must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "delete_route".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("DELETE", ["hives", hive_id, "routes", prefix]) => Ok(AdminTranslation {
            admin_target,
            action: "delete_route".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "prefix": prefix }),
        }),
        ("GET", ["hives", hive_id, "vpns"]) => Ok(AdminTranslation {
            admin_target,
            action: "list_vpns".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("POST", ["hives", hive_id, "vpns"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for add_vpn must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "add_vpn".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("DELETE", ["hives", hive_id, "vpns"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for delete_vpn must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "delete_vpn".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("DELETE", ["hives", hive_id, "vpns", pattern]) => Ok(AdminTranslation {
            admin_target,
            action: "delete_vpn".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "pattern": pattern }),
        }),
        ("GET", ["hives", hive_id, "deployments"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_deployments".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "drift-alerts"]) => Ok(AdminTranslation {
            admin_target,
            action: "get_drift_alerts".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "opa", "policy"]) => Ok(AdminTranslation {
            admin_target,
            action: "opa_get_policy".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "opa", "status"]) => Ok(AdminTranslation {
            admin_target,
            action: "opa_get_status".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "timer", "help"]) => Ok(AdminTranslation {
            admin_target,
            action: "timer_help".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("GET", ["hives", hive_id, "timer", "timers"]) => {
            let mut params = json!({});
            if let Some(query) = raw_query {
                for pair in query.split('&').filter(|segment| !segment.is_empty()) {
                    let (key, value) = match pair.split_once('=') {
                        Some(parts) => parts,
                        None => continue,
                    };
                    if value.is_empty() {
                        continue;
                    }
                    match key {
                        "owner_l2_name" => {
                            params["owner_l2_name"] = Value::String(value.to_string())
                        }
                        "status_filter" => {
                            params["status_filter"] = Value::String(value.to_string())
                        }
                        "limit" => {
                            if let Ok(limit) = value.parse::<u64>() {
                                params["limit"] = json!(limit);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(AdminTranslation {
                admin_target,
                action: "timer_list".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("GET", ["hives", hive_id, "timer", "timers", timer_uuid]) => Ok(AdminTranslation {
            admin_target,
            action: "timer_get".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "timer_uuid": timer_uuid }),
        }),
        ("GET", ["hives", hive_id, "timer", "now"]) => Ok(AdminTranslation {
            admin_target,
            action: "timer_now".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({}),
        }),
        ("POST", ["hives", hive_id, "timer", "now-in"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for timer now-in must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "timer_now_in".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "timer", "convert"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for timer convert must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "timer_convert".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "timer", "parse"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for timer parse must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "timer_parse".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "timer", "format"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for timer format must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "timer_format".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "opa", "policy", "check"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for opa check must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "opa_check".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "opa", "policy"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for opa compile_apply must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "opa_compile_apply".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "opa", "policy", "compile"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for opa compile must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "opa_compile".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "opa", "policy", "apply"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for opa apply must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "opa_apply".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "opa", "policy", "rollback"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for opa rollback must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "opa_rollback".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "update"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for update must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "update".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "sync-hint"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for sync_hint must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "sync_hint".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("DELETE", ["hives", hive_id, "nodes", node_name, "instance"]) => Ok(AdminTranslation {
            admin_target,
            action: "remove_node_instance".to_string(),
            target_hive: (*hive_id).to_string(),
            params: json!({ "node_name": node_name }),
        }),
        ("DELETE", ["hives", hive_id, "nodes", node_name]) => {
            let force = parsed
                .body
                .as_ref()
                .and_then(|value| value.get("force"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(AdminTranslation {
                admin_target,
                action: "kill_node".to_string(),
                target_hive: (*hive_id).to_string(),
                params: json!({ "node_name": node_name, "force": force }),
            })
        }
        ("POST", ["hives", hive_id, "nodes", node_name, "messages"]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for send message must be a JSON object".into());
            }
            params["node_name"] = Value::String((*node_name).to_string());
            Ok(AdminTranslation {
                admin_target,
                action: "send_node_message".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "nodes", node_name, "control", "config-get"]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for node_control_config_get must be a JSON object".into());
            }
            params["node_name"] = Value::String((*node_name).to_string());
            Ok(AdminTranslation {
                admin_target,
                action: "node_control_config_get".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "nodes", node_name, "control", "config-set"]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for node_control_config_set must be a JSON object".into());
            }
            params["node_name"] = Value::String((*node_name).to_string());
            Ok(AdminTranslation {
                admin_target,
                action: "node_control_config_set".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("PUT", ["hives", hive_id, "nodes", node_name, "config"]) => {
            let config = parsed.body.unwrap_or_else(|| json!({}));
            if !config.is_object() {
                return Err("SCMD body for set_node_config must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "set_node_config".to_string(),
                target_hive: (*hive_id).to_string(),
                params: json!({
                    "node_name": node_name,
                    "config": config,
                    "replace": false,
                    "notify": true,
                }),
            })
        }
        ("PUT", ["config", "storage"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for set_storage must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "set_storage".to_string(),
                target_hive: local_hive_id.to_string(),
                params,
            })
        }
        _ => Err(format!("unsupported SCMD path: {} {}", parsed.method, parsed.path).into()),
    }
}

async fn execute_admin_translation(
    state: &ArchitectState,
    translation: AdminTranslation,
) -> Result<Value, ArchitectError> {
    execute_admin_translation_with_context(&admin_tool_context(state, None), translation, "scmd")
        .await
}

async fn execute_tracked_admin_translation(
    state: &ArchitectState,
    session_id: &str,
    origin: &str,
    translation: AdminTranslation,
    preview_command: String,
    existing_operation_id: Option<String>,
) -> Result<Value, ArchitectError> {
    let params_hash_value = params_hash(&translation.params);
    let existing_match = find_equivalent_operation(
        state,
        session_id,
        &translation.action,
        &translation.target_hive,
        &params_hash_value,
    )
    .await?;
    if let Some(mut existing) = existing_match.clone() {
        let existing_id = existing.operation_id.clone();
        let current_id = existing_operation_id.as_deref();
        if current_id != Some(existing_id.as_str()) {
            if existing.status == "timeout_unknown" {
                let _ = reconcile_timeout_unknown_operation(state, &mut existing).await;
            }
            if !is_terminal_operation_status(&existing.status) {
                return Ok(json!({
                    "status": "error",
                    "action": translation.action,
                    "error_code": "OPERATION_ALREADY_TRACKED",
                    "error_detail": operation_error_message(&existing),
                    "operation_id": existing.operation_id,
                    "confirmed_command": preview_command,
                }));
            }
        }
    }

    let now = now_epoch_ms();
    let params_json =
        serde_json::to_string(&normalize_json(&translation.params)).unwrap_or_else(|_| "{}".into());
    let created_at_ms = existing_match
        .as_ref()
        .filter(|record| existing_operation_id.as_deref() == Some(record.operation_id.as_str()))
        .map(|record| record.created_at_ms)
        .unwrap_or(now);
    let origin_value = existing_match
        .as_ref()
        .filter(|record| existing_operation_id.as_deref() == Some(record.operation_id.as_str()))
        .map(|record| record.origin.clone())
        .unwrap_or_else(|| origin.to_string());
    let mut operation = ChatOperationRecord {
        operation_id: existing_operation_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        session_id: session_id.to_string(),
        scope_id: operation_scope_id(session_id),
        origin: origin_value,
        action: translation.action.clone(),
        target_hive: translation.target_hive.clone(),
        params_json,
        params_hash: params_hash_value,
        preview_command: preview_command.clone(),
        status: "dispatched".to_string(),
        created_at_ms,
        updated_at_ms: now,
        dispatched_at_ms: now,
        completed_at_ms: 0,
        request_id: String::new(),
        trace_id: String::new(),
        error_summary: String::new(),
    };
    save_operation(state, &operation).await?;

    match execute_admin_translation(state, translation).await {
        Ok(mut output) => {
            operation.status = operation_status_from_output(&output);
            operation.updated_at_ms = now_epoch_ms();
            if is_terminal_operation_status(&operation.status) {
                operation.completed_at_ms = operation.updated_at_ms;
            }
            operation.request_id = output
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            operation.trace_id = output
                .get("trace_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            operation.error_summary = output
                .get("error_detail")
                .and_then(Value::as_str)
                .or_else(|| output.get("error_code").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            save_operation(state, &operation).await?;
            output["operation_id"] = Value::String(operation.operation_id);
            output["operation_status"] = Value::String(operation.status);
            Ok(output)
        }
        Err(err) => {
            let err_text = err.to_string();
            operation.status = if err_text.contains("timeout waiting ADMIN_COMMAND_RESPONSE") {
                "timeout_unknown".to_string()
            } else {
                "failed".to_string()
            };
            operation.updated_at_ms = now_epoch_ms();
            if is_terminal_operation_status(&operation.status) {
                operation.completed_at_ms = operation.updated_at_ms;
            }
            operation.error_summary = err_text.clone();
            save_operation(state, &operation).await?;
            Ok(json!({
                "status": if operation.status == "timeout_unknown" { "error" } else { "error" },
                "action": operation.action,
                "error_code": if operation.status == "timeout_unknown" { "TIMEOUT_UNKNOWN" } else { "EXECUTION_FAILED" },
                "error_detail": if operation.status == "timeout_unknown" {
                    "The operation timed out locally but may still be running. Inspect system state before retrying."
                } else {
                    err_text.as_str()
                },
                "operation_id": operation.operation_id,
                "operation_status": operation.status,
                "confirmed_command": preview_command,
            }))
        }
    }
}

async fn execute_admin_translation_with_context(
    context: &ArchitectAdminToolContext,
    translation: AdminTranslation,
    purpose: &str,
) -> Result<Value, ArchitectError> {
    execute_admin_action_with_context(
        context,
        &translation.admin_target,
        &translation.action,
        Some(&translation.target_hive),
        translation.params,
        purpose,
    )
    .await
}

async fn execute_executor_plan_with_context(
    context: &ArchitectAdminToolContext,
    execution_id: String,
    plan: Value,
    executor_options: Value,
    labels: Value,
) -> Result<Value, ArchitectError> {
    let admin_target = format!("SY.admin@{}", context.hive_id);
    let validate_output = execute_admin_action_with_context(
        context,
        &admin_target,
        "executor_validate_plan",
        None,
        plan.clone(),
        "executor.validate",
    )
    .await?;
    let validate_status = validate_output
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    if !validate_status.eq_ignore_ascii_case("ok") {
        return Err(format!(
            "executor plan validation failed: {}",
            response_detail_text(&validate_output).unwrap_or_else(|| "unknown validation error".to_string())
        )
        .into());
    }

    let execute_output = execute_admin_action_with_context(
        context,
        &admin_target,
        "executor_execute_plan",
        None,
        json!({
            "execution_id": execution_id,
            "plan": plan,
            "executor_options": if executor_options.is_object() { executor_options } else { json!({}) },
            "labels": if labels.is_object() { labels } else { json!({}) }
        }),
        "executor.run",
    )
    .await?;
    Ok(json!({
        "status": execute_output.get("status").cloned().unwrap_or_else(|| json!("error")),
        "action": "executor_execute_plan",
        "payload": execute_output.get("payload").cloned().unwrap_or(Value::Null),
        "validation": validate_output.get("payload").cloned().unwrap_or(Value::Null),
        "error_code": execute_output.get("error_code").cloned().unwrap_or(Value::Null),
        "error_detail": execute_output.get("error_detail").cloned().unwrap_or(Value::Null),
        "message": execute_output
            .get("payload")
            .and_then(|payload| payload.get("summary"))
            .and_then(|summary| summary.get("final_message"))
            .and_then(Value::as_str)
            .unwrap_or("Executor plan finished"),
    }))
}

async fn execute_admin_action_with_context(
    context: &ArchitectAdminToolContext,
    admin_target: &str,
    action: &str,
    target: Option<&str>,
    params: Value,
    purpose: &str,
) -> Result<Value, ArchitectError> {
    let params_json =
        serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string());
    let timeout = architect_admin_action_timeout(action);
    tracing::info!(
        purpose = %purpose,
        admin_target = %admin_target,
        action = %action,
        target_hive = ?target,
        timeout_ms = timeout.as_millis(),
        params = %params_json,
        "sy.architect dispatching admin action"
    );
    let node_config = NodeConfig {
        name: format!("SY.architect.{purpose}.{}", Uuid::new_v4().simple()),
        router_socket: context.socket_dir.clone(),
        uuid_persistence_dir: context.state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Ephemeral,
        config_dir: context.config_dir.clone(),
        version: "0.1.0".to_string(),
    };
    let (sender, mut receiver) = connect(&node_config).await?;
    let out = admin_command(
        &sender,
        &mut receiver,
        AdminCommandRequest {
            admin_target,
            action,
            target,
            params,
            request_id: None,
            timeout,
        },
    )
    .await
    .map_err(|err| -> ArchitectError {
        tracing::warn!(
            purpose = %purpose,
            admin_target = %admin_target,
            action = %action,
            target_hive = ?target,
            error = %err,
            "sy.architect admin action failed"
        );
        Box::new(err)
    })?;
    let payload_json = serde_json::to_string(&out.payload).unwrap_or_else(|_| "null".to_string());
    tracing::info!(
        purpose = %purpose,
        admin_target = %admin_target,
        action = %action,
        target_hive = ?target,
        status = %out.status,
        error_code = ?out.error_code,
        error_detail = ?out.error_detail,
        request_id = ?out.request_id,
        trace_id = ?out.trace_id,
        payload = %payload_json,
        "sy.architect admin action response"
    );
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

const STATUS_REFRESH_INTERVAL_SECS: u64 = 30;

async fn status_refresh_loop(state: Arc<ArchitectState>) {
    loop {
        let fresh = build_architect_status(&state).await;
        *state.cached_status.write().await = fresh;
        time::sleep(Duration::from_secs(STATUS_REFRESH_INTERVAL_SECS)).await;
    }
}

async fn build_architect_status(state: &ArchitectState) -> ArchitectStatus {
    let mut status = ArchitectStatus {
        status: "ok".to_string(),
        hive_id: state.hive_id.clone(),
        node_name: state.node_name.clone(),
        router_connected: state.router_connected.load(Ordering::Relaxed),
        admin_available: false,
        inventory_updated_at: None,
        total_hives: None,
        hives_alive: None,
        hives_stale: None,
        total_nodes: None,
        nodes_by_status: BTreeMap::new(),
        components: default_component_statuses(),
        error: None,
    };

    match fetch_inventory_status_data(state).await {
        Ok((summary, hive)) => {
            status.admin_available = true;
            apply_inventory_summary(&mut status, &summary);
            if let Some(hive) = hive.as_ref() {
                status.components = parse_component_statuses(hive);
            }
        }
        Err(err) => {
            status.status = "degraded".to_string();
            status.error = Some(err.to_string());
        }
    }

    status
}

async fn fetch_inventory_status_data(
    state: &ArchitectState,
) -> Result<(Value, Option<Value>), ArchitectError> {
    let node_config = NodeConfig {
        name: format!("SY.architect.status.{}", Uuid::new_v4().simple()),
        router_socket: state.socket_dir.clone(),
        uuid_persistence_dir: state.state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Ephemeral,
        config_dir: state.config_dir.clone(),
        version: "0.1.0".to_string(),
    };
    let (sender, mut receiver) = connect(&node_config)
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let admin_target = format!("SY.admin@{}", state.hive_id);

    let summary = admin_command(
        &sender,
        &mut receiver,
        AdminCommandRequest {
            admin_target: &admin_target,
            action: "inventory",
            target: None,
            params: json!({ "scope": "summary" }),
            request_id: None,
            timeout: Duration::from_secs(5),
        },
    )
    .await
    .map_err(|err| -> ArchitectError { Box::new(err) })?;
    ensure_admin_ok("inventory summary", &summary)?;

    let hive = admin_command(
        &sender,
        &mut receiver,
        AdminCommandRequest {
            admin_target: &admin_target,
            action: "inventory",
            target: None,
            params: json!({ "scope": "hive", "filter_hive": state.hive_id }),
            request_id: None,
            timeout: Duration::from_secs(5),
        },
    )
    .await;
    let hive_payload = match hive {
        Ok(result) => {
            if ensure_admin_ok("inventory hive", &result).is_ok() {
                Some(result.payload)
            } else {
                None
            }
        }
        Err(_) => None,
    };

    Ok((summary.payload, hive_payload))
}

fn ensure_admin_ok(
    label: &str,
    result: &fluxbee_sdk::AdminCommandResult,
) -> Result<(), ArchitectError> {
    if result.status == "ok" {
        return Ok(());
    }
    Err(format!(
        "{label} failed: status={}, error_code={:?}, error_detail={:?}",
        result.status, result.error_code, result.error_detail
    )
    .into())
}

fn apply_inventory_summary(status: &mut ArchitectStatus, payload: &Value) {
    status.inventory_updated_at = payload.get("updated_at").and_then(Value::as_u64);
    status.total_hives = payload
        .get("total_hives")
        .and_then(Value::as_u64)
        .map(|value| value as u32);
    status.hives_alive = payload
        .get("hives_alive")
        .and_then(Value::as_u64)
        .map(|value| value as u32);
    status.hives_stale = payload
        .get("hives_stale")
        .and_then(Value::as_u64)
        .map(|value| value as u32);
    status.total_nodes = payload
        .get("total_nodes")
        .and_then(Value::as_u64)
        .map(|value| value as u32);

    if let Some(map) = payload.get("nodes_by_status").and_then(Value::as_object) {
        for (key, value) in map {
            if let Some(count) = value.as_u64() {
                status.nodes_by_status.insert(key.clone(), count as u32);
            }
        }
    }
}

fn default_component_statuses() -> Vec<ArchitectComponentStatus> {
    [
        ("RT.gateway", "router"),
        ("SY.orchestrator", "orchestrator"),
        ("SY.admin", "admin"),
        ("SY.identity", "identity"),
        ("SY.storage", "storage"),
    ]
    .into_iter()
    .map(|(key, label)| ArchitectComponentStatus {
        key: key.to_string(),
        label: label.to_string(),
        status: "missing".to_string(),
        node_name: None,
    })
    .collect()
}

fn parse_component_statuses(payload: &Value) -> Vec<ArchitectComponentStatus> {
    let Some(nodes) = payload.get("nodes").and_then(Value::as_array) else {
        return default_component_statuses();
    };

    default_component_statuses()
        .into_iter()
        .map(|component| {
            let best = nodes
                .iter()
                .filter(|node| node_matches_component(node, &component.key))
                .max_by_key(|node| {
                    component_status_rank(node.get("status").and_then(Value::as_str))
                });
            match best {
                Some(node) => ArchitectComponentStatus {
                    status: node
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    node_name: node
                        .get("node_name")
                        .or_else(|| node.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    ..component
                },
                None => component,
            }
        })
        .collect()
}

fn node_matches_component(node: &Value, key: &str) -> bool {
    node.get("name")
        .and_then(Value::as_str)
        .map(|name| name == key || name.starts_with(&format!("{key}@")))
        .unwrap_or(false)
}

fn component_status_rank(status: Option<&str>) -> u8 {
    match status.unwrap_or_default().to_ascii_lowercase().as_str() {
        "alive" | "running" | "healthy" | "ok" | "active" => 3,
        "starting" | "unknown" => 2,
        "stale" | "degraded" => 1,
        _ => 0,
    }
}

async fn ensure_chat_storage(state: &ArchitectState) -> Result<(), ArchitectError> {
    let db = open_architect_db(state).await?;
    let _ = ensure_sessions_table(&db).await?;
    let _ = ensure_session_profiles_table(&db).await?;
    let _ = ensure_messages_table(&db).await?;
    let _ = ensure_operations_table(&db).await?;
    Ok(())
}

async fn open_architect_db(state: &ArchitectState) -> Result<Connection, ArchitectError> {
    let path = architect_db_path(state);
    fs::create_dir_all(&path)?;
    lancedb::connect(path.to_string_lossy().as_ref())
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })
}

fn architect_db_path(state: &ArchitectState) -> PathBuf {
    architect_node_dir(&state.hive_id).join("architect.lance")
}

async fn ensure_sessions_table(db: &Connection) -> Result<lancedb::Table, ArchitectError> {
    match db.open_table(CHAT_SESSIONS_TABLE).execute().await {
        Ok(table) => Ok(table),
        Err(_) => db
            .create_empty_table(CHAT_SESSIONS_TABLE, chat_sessions_schema())
            .execute()
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) }),
    }
}

async fn ensure_messages_table(db: &Connection) -> Result<lancedb::Table, ArchitectError> {
    match db.open_table(CHAT_MESSAGES_TABLE).execute().await {
        Ok(table) => Ok(table),
        Err(_) => db
            .create_empty_table(CHAT_MESSAGES_TABLE, chat_messages_schema())
            .execute()
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) }),
    }
}

async fn ensure_operations_table(db: &Connection) -> Result<lancedb::Table, ArchitectError> {
    match db.open_table(CHAT_OPERATIONS_TABLE).execute().await {
        Ok(table) => Ok(table),
        Err(_) => db
            .create_empty_table(CHAT_OPERATIONS_TABLE, chat_operations_schema())
            .execute()
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) }),
    }
}

async fn ensure_session_profiles_table(db: &Connection) -> Result<lancedb::Table, ArchitectError> {
    match db.open_table(CHAT_SESSION_PROFILES_TABLE).execute().await {
        Ok(table) => {
            if session_profiles_schema_matches(&table).await? {
                Ok(table)
            } else {
                db.drop_table(CHAT_SESSION_PROFILES_TABLE, &[])
                    .await
                    .map_err(|err| -> ArchitectError { Box::new(err) })?;
                db.create_empty_table(CHAT_SESSION_PROFILES_TABLE, chat_session_profiles_schema())
                    .execute()
                    .await
                    .map_err(|err| -> ArchitectError { Box::new(err) })
            }
        }
        Err(_) => db
            .create_empty_table(CHAT_SESSION_PROFILES_TABLE, chat_session_profiles_schema())
            .execute()
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) }),
    }
}

async fn session_profiles_schema_matches(table: &lancedb::Table) -> Result<bool, ArchitectError> {
    let schema = table
        .schema()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let required = [
        "session_id",
        "chat_mode",
        "effective_ich_id",
        "effective_ilk",
        "impersonation_target",
        "thread_id",
        "source_channel_kind",
        "debug_enabled",
    ];
    Ok(required
        .iter()
        .all(|field| schema.column_with_name(field).is_some()))
}

fn chat_sessions_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("session_id", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("agent", DataType::Utf8, false),
        Field::new("created_at_ms", DataType::UInt64, false),
        Field::new("last_activity_at_ms", DataType::UInt64, false),
        Field::new("message_count", DataType::UInt64, false),
        Field::new("last_message_preview", DataType::Utf8, false),
    ]))
}

fn chat_session_profiles_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("session_id", DataType::Utf8, false),
        Field::new("chat_mode", DataType::Utf8, false),
        Field::new("effective_ich_id", DataType::Utf8, false),
        Field::new("effective_ilk", DataType::Utf8, false),
        Field::new("impersonation_target", DataType::Utf8, false),
        Field::new("thread_id", DataType::Utf8, false),
        Field::new("source_channel_kind", DataType::Utf8, false),
        Field::new("debug_enabled", DataType::Boolean, false),
    ]))
}

fn chat_messages_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("message_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("role", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("timestamp_ms", DataType::UInt64, false),
        Field::new("mode", DataType::Utf8, false),
        Field::new("metadata_json", DataType::Utf8, false),
        Field::new("seq", DataType::UInt64, false),
    ]))
}

fn chat_operations_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("operation_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("scope_id", DataType::Utf8, false),
        Field::new("origin", DataType::Utf8, false),
        Field::new("action", DataType::Utf8, false),
        Field::new("target_hive", DataType::Utf8, false),
        Field::new("params_json", DataType::Utf8, false),
        Field::new("params_hash", DataType::Utf8, false),
        Field::new("preview_command", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("created_at_ms", DataType::UInt64, false),
        Field::new("updated_at_ms", DataType::UInt64, false),
        Field::new("dispatched_at_ms", DataType::UInt64, false),
        Field::new("completed_at_ms", DataType::UInt64, false),
        Field::new("request_id", DataType::Utf8, false),
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("error_summary", DataType::Utf8, false),
    ]))
}

async fn list_chat_sessions(state: &ArchitectState) -> Result<Vec<SessionSummary>, ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions_table = ensure_sessions_table(&db).await?;
    let profiles_table = ensure_session_profiles_table(&db).await?;
    let batches = sessions_table
        .query()
        .select(Select::columns(&[
            "session_id",
            "title",
            "agent",
            "created_at_ms",
            "last_activity_at_ms",
            "message_count",
            "last_message_preview",
        ]))
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let mut sessions = parse_session_batches(
        &batches,
        &load_session_profile_map(&profiles_table, None).await?,
    )?;
    sessions.sort_by(|a, b| {
        b.last_activity_at_ms
            .cmp(&a.last_activity_at_ms)
            .then_with(|| b.created_at_ms.cmp(&a.created_at_ms))
    });
    Ok(sessions)
}

async fn create_chat_session(
    state: &ArchitectState,
    req: CreateSessionRequest,
) -> Result<SessionDetailResponse, ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let profiles = ensure_session_profiles_table(&db).await?;
    let _messages = ensure_messages_table(&db).await?;
    let now = now_epoch_ms();
    let CreateSessionRequest {
        title,
        chat_mode,
        effective_ich_id,
        effective_ilk,
        impersonation_target,
        thread_id,
        source_channel_kind,
        debug_enabled,
    } = req;
    let record = ChatSessionRecord {
        session_id: Uuid::new_v4().to_string(),
        title: sanitize_session_title(title.as_deref()),
        agent: "architect".to_string(),
        created_at_ms: now,
        last_activity_at_ms: now,
        message_count: 0,
        last_message_preview: String::new(),
        ..build_session_profile(CreateSessionRequest {
            title: None,
            chat_mode,
            effective_ich_id,
            effective_ilk,
            impersonation_target,
            thread_id,
            source_channel_kind,
            debug_enabled,
        })?
    };
    upsert_session_record(&sessions, &record).await?;
    upsert_session_profile_record(&profiles, &session_profile_from_record(&record)).await?;
    Ok(SessionDetailResponse {
        session: session_record_to_summary(&record),
        messages: Vec::new(),
    })
}

async fn load_chat_session(
    state: &ArchitectState,
    session_id: &str,
) -> Result<Option<SessionDetailResponse>, ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let profiles = ensure_session_profiles_table(&db).await?;
    let messages = ensure_messages_table(&db).await?;
    let Some(session) = load_session_record(&sessions, &profiles, session_id).await? else {
        return Ok(None);
    };
    let persisted = load_session_messages(&messages, session_id).await?;
    Ok(Some(SessionDetailResponse {
        session: session_record_to_summary(&session),
        messages: persisted,
    }))
}

async fn delete_chat_session(
    state: &ArchitectState,
    session_id: &str,
) -> Result<SessionDeleteResponse, ArchitectError> {
    state.pending_actions.lock().await.remove(session_id);
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let profiles = ensure_session_profiles_table(&db).await?;
    let messages = ensure_messages_table(&db).await?;
    let operations = ensure_operations_table(&db).await?;
    let filter = session_filter(session_id);

    let deleted_messages = count_session_messages(&messages, session_id).await?;
    let deleted_sessions = if load_session_record(&sessions, &profiles, session_id)
        .await?
        .is_some()
    {
        1
    } else {
        0
    };

    if deleted_sessions > 0 {
        sessions
            .delete(&filter)
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) })?;
    }
    if deleted_messages > 0 {
        messages
            .delete(&filter)
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) })?;
    }
    operations
        .delete(&filter)
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    profiles
        .delete(&filter)
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;

    Ok(SessionDeleteResponse {
        status: "ok".to_string(),
        deleted_sessions,
        deleted_messages,
    })
}

async fn clear_chat_sessions(
    state: &ArchitectState,
) -> Result<SessionDeleteResponse, ArchitectError> {
    state.pending_actions.lock().await.clear();
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let profiles = ensure_session_profiles_table(&db).await?;
    let messages = ensure_messages_table(&db).await?;
    let operations = ensure_operations_table(&db).await?;

    let deleted_sessions = count_all_rows(&sessions).await?;
    let deleted_messages = count_all_rows(&messages).await?;

    if deleted_sessions > 0 {
        sessions
            .delete("session_id IS NOT NULL")
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) })?;
    }
    if deleted_messages > 0 {
        messages
            .delete("message_id IS NOT NULL")
            .await
            .map_err(|err| -> ArchitectError { Box::new(err) })?;
    }
    operations
        .delete("operation_id IS NOT NULL")
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    profiles
        .delete("session_id IS NOT NULL")
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;

    Ok(SessionDeleteResponse {
        status: "ok".to_string(),
        deleted_sessions,
        deleted_messages,
    })
}

async fn resolve_chat_session(
    state: &ArchitectState,
    session_id: Option<String>,
    title: Option<String>,
) -> Result<(String, ChatSessionRecord), ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let profiles = ensure_session_profiles_table(&db).await?;
    let _messages = ensure_messages_table(&db).await?;

    if let Some(session_id) = session_id {
        if let Some(session) = load_session_record(&sessions, &profiles, &session_id).await? {
            return Ok((session_id, session));
        }
        return Err(format!("session not found: {session_id}").into());
    }

    let now = now_epoch_ms();
    let record = ChatSessionRecord {
        session_id: Uuid::new_v4().to_string(),
        title: sanitize_session_title(title.as_deref()),
        agent: "architect".to_string(),
        created_at_ms: now,
        last_activity_at_ms: now,
        message_count: 0,
        last_message_preview: String::new(),
        ..default_session_profile()
    };
    upsert_session_record(&sessions, &record).await?;
    upsert_session_profile_record(&profiles, &session_profile_from_record(&record)).await?;
    Ok((record.session_id.clone(), record))
}

async fn persist_chat_exchange(
    state: &ArchitectState,
    session: &mut ChatSessionRecord,
    user_message: String,
    attachments: &[ChatAttachmentRef],
    response: &ChatResponse,
) -> Result<(), ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let messages = ensure_messages_table(&db).await?;

    let now = now_epoch_ms();
    let user_message = sanitize_user_message_for_persistence(&user_message);
    let user_metadata = if attachments.is_empty() {
        json!({ "kind": "text" })
    } else {
        json!({
            "kind": "text_with_attachments",
            "attachments": attachments
                .iter()
                .map(|attachment| json!({
                    "attachment_id": attachment.attachment_id,
                    "filename": attachment.blob_ref.filename_original,
                    "mime": attachment.blob_ref.mime,
                    "size": attachment.blob_ref.size,
                    "blob_ref": attachment.blob_ref,
                }))
                .collect::<Vec<_>>(),
        })
    };
    let user_row = ChatMessageRecord {
        message_id: Uuid::new_v4().to_string(),
        session_id: session.session_id.clone(),
        role: "user".to_string(),
        content: user_message.clone(),
        timestamp_ms: now,
        mode: "text".to_string(),
        metadata_json: user_metadata.to_string(),
        seq: session.message_count + 1,
    };
    append_message_record(&messages, &user_row).await?;

    let suppress_response_row = response
        .output
        .get("suppress_response_row")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if suppress_response_row {
        session.message_count += 1;
        session.last_activity_at_ms = now;
        session.last_message_preview = preview_text(
            &render_user_message_with_attachment_summary(&user_message, attachments),
            88,
        );
        upsert_session_record(&sessions, session).await?;
        return Ok(());
    }

    let response_role = if response.status == "error" {
        "system"
    } else {
        "architect"
    };
    let response_row = ChatMessageRecord {
        message_id: Uuid::new_v4().to_string(),
        session_id: session.session_id.clone(),
        role: response_role.to_string(),
        content: response
            .output
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        timestamp_ms: now_epoch_ms(),
        mode: response.mode.clone(),
        metadata_json: json!({
            "kind": "response",
            "response": response,
        })
        .to_string(),
        seq: session.message_count + 2,
    };
    append_message_record(&messages, &response_row).await?;

    session.message_count += 2;
    session.last_activity_at_ms = response_row.timestamp_ms;
    session.last_message_preview = preview_text(
        if response_row.content.is_empty() {
            &response.mode
        } else {
            &response_row.content
        },
        88,
    );
    upsert_session_record(&sessions, session).await?;
    Ok(())
}

async fn load_session_record(
    session_table: &lancedb::Table,
    profiles_table: &lancedb::Table,
    session_id: &str,
) -> Result<Option<ChatSessionRecord>, ArchitectError> {
    let filter = format!("session_id = '{}'", session_id.replace('\'', "''"));
    let batches = session_table
        .query()
        .only_if(filter)
        .select(Select::columns(&[
            "session_id",
            "title",
            "agent",
            "created_at_ms",
            "last_activity_at_ms",
            "message_count",
            "last_message_preview",
        ]))
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let profile_map = load_session_profile_map(profiles_table, Some(session_id)).await?;
    let mut rows = parse_session_records(&batches, &profile_map)?;
    Ok(rows.pop())
}

async fn load_session_messages(
    table: &lancedb::Table,
    session_id: &str,
) -> Result<Vec<PersistedChatMessage>, ArchitectError> {
    let filter = session_filter(session_id);
    let batches = table
        .query()
        .only_if(filter)
        .select(Select::columns(&[
            "message_id",
            "session_id",
            "role",
            "content",
            "timestamp_ms",
            "mode",
            "metadata_json",
            "seq",
        ]))
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let mut rows = parse_message_records(&batches)?;
    rows.sort_by_key(|row| (row.seq, row.timestamp_ms));
    Ok(rows.into_iter().map(message_record_to_persisted).collect())
}

async fn load_session_operations(
    table: &lancedb::Table,
    session_id: &str,
) -> Result<Vec<ChatOperationRecord>, ArchitectError> {
    let filter = session_filter(session_id);
    let batches = table
        .query()
        .only_if(filter)
        .select(Select::columns(&[
            "operation_id",
            "session_id",
            "scope_id",
            "origin",
            "action",
            "target_hive",
            "params_json",
            "params_hash",
            "preview_command",
            "status",
            "created_at_ms",
            "updated_at_ms",
            "dispatched_at_ms",
            "completed_at_ms",
            "request_id",
            "trace_id",
            "error_summary",
        ]))
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let mut rows = parse_operation_records(&batches)?;
    rows.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| b.created_at_ms.cmp(&a.created_at_ms))
    });
    Ok(rows)
}

async fn find_equivalent_operation(
    state: &ArchitectState,
    session_id: &str,
    action: &str,
    target_hive: &str,
    params_hash_value: &str,
) -> Result<Option<ChatOperationRecord>, ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let operations = ensure_operations_table(&db).await?;
    let rows = load_session_operations(&operations, session_id).await?;
    Ok(rows.into_iter().find(|row| {
        row.action == action
            && row.target_hive == target_hive
            && row.params_hash == params_hash_value
            && !is_terminal_operation_status(&row.status)
    }))
}

async fn save_operation(
    state: &ArchitectState,
    record: &ChatOperationRecord,
) -> Result<(), ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let operations = ensure_operations_table(&db).await?;
    upsert_operation_record(&operations, record).await
}

async fn open_architect_db_for_hive(hive_id: &str) -> Result<Connection, ArchitectError> {
    let path = architect_node_dir(hive_id).join("architect.lance");
    fs::create_dir_all(&path)?;
    lancedb::connect(path.to_string_lossy().as_ref())
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })
}

async fn find_equivalent_operation_with_context(
    context: &ArchitectAdminToolContext,
    session_id: &str,
    action: &str,
    target_hive: &str,
    params_hash_value: &str,
) -> Result<Option<ChatOperationRecord>, ArchitectError> {
    let _guard = context.chat_lock.lock().await;
    let db = open_architect_db_for_hive(&context.hive_id).await?;
    let operations = ensure_operations_table(&db).await?;
    let rows = load_session_operations(&operations, session_id).await?;
    Ok(rows.into_iter().find(|row| {
        row.action == action
            && row.target_hive == target_hive
            && row.params_hash == params_hash_value
            && !is_terminal_operation_status(&row.status)
    }))
}

async fn save_operation_with_context(
    context: &ArchitectAdminToolContext,
    record: &ChatOperationRecord,
) -> Result<(), ArchitectError> {
    let _guard = context.chat_lock.lock().await;
    let db = open_architect_db_for_hive(&context.hive_id).await?;
    let operations = ensure_operations_table(&db).await?;
    upsert_operation_record(&operations, record).await
}

async fn count_session_messages(
    table: &lancedb::Table,
    session_id: &str,
) -> Result<u64, ArchitectError> {
    let filter = session_filter(session_id);
    table
        .count_rows(Some(filter.into()))
        .await
        .map(|count| count as u64)
        .map_err(|err| -> ArchitectError { Box::new(err) })
}

async fn count_all_rows(table: &lancedb::Table) -> Result<u64, ArchitectError> {
    table
        .count_rows(None)
        .await
        .map(|count| count as u64)
        .map_err(|err| -> ArchitectError { Box::new(err) })
}

async fn upsert_session_record(
    table: &lancedb::Table,
    record: &ChatSessionRecord,
) -> Result<(), ArchitectError> {
    let batch = session_batch_from_rows(std::slice::from_ref(record))?;
    let mut merge = table.merge_insert(&["session_id"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge
        .execute(single_batch_reader(batch))
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    Ok(())
}

async fn upsert_session_profile_record(
    table: &lancedb::Table,
    record: &ChatSessionProfileRecord,
) -> Result<(), ArchitectError> {
    let batch = session_profile_batch_from_rows(std::slice::from_ref(record))?;
    let mut merge = table.merge_insert(&["session_id"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge
        .execute(single_batch_reader(batch))
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    Ok(())
}

async fn append_message_record(
    table: &lancedb::Table,
    record: &ChatMessageRecord,
) -> Result<(), ArchitectError> {
    let batch = message_batch_from_rows(std::slice::from_ref(record))?;
    table
        .add(batch)
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    Ok(())
}

async fn upsert_operation_record(
    table: &lancedb::Table,
    record: &ChatOperationRecord,
) -> Result<(), ArchitectError> {
    let batch = operation_batch_from_rows(std::slice::from_ref(record))?;
    let mut merge = table.merge_insert(&["operation_id"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge
        .execute(single_batch_reader(batch))
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    Ok(())
}

fn single_batch_reader(batch: RecordBatch) -> Box<dyn arrow_array::RecordBatchReader + Send> {
    let schema = batch.schema();
    Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema,
    ))
}

fn session_batch_from_rows(rows: &[ChatSessionRecord]) -> Result<RecordBatch, ArchitectError> {
    let schema = chat_sessions_schema();
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.session_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.title.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.agent.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|row| row.created_at_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|row| row.last_activity_at_ms)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|row| row.message_count).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.last_message_preview.as_str())
                    .collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

fn message_batch_from_rows(rows: &[ChatMessageRecord]) -> Result<RecordBatch, ArchitectError> {
    let schema = chat_messages_schema();
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.message_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.session_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|row| row.role.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.content.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|row| row.timestamp_ms).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|row| row.mode.as_str()).collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.metadata_json.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

fn session_profile_batch_from_rows(
    rows: &[ChatSessionProfileRecord],
) -> Result<RecordBatch, ArchitectError> {
    let schema = chat_session_profiles_schema();
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.session_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.chat_mode.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.effective_ich_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.effective_ilk.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.impersonation_target.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.thread_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.source_channel_kind.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(arrow_array::BooleanArray::from(
                rows.iter().map(|row| row.debug_enabled).collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

fn operation_batch_from_rows(rows: &[ChatOperationRecord]) -> Result<RecordBatch, ArchitectError> {
    let schema = chat_operations_schema();
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.operation_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.session_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.scope_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.origin.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.action.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.target_hive.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.params_json.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.params_hash.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.preview_command.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.status.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|row| row.created_at_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|row| row.updated_at_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|row| row.dispatched_at_ms)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|row| row.completed_at_ms)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.request_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.trace_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|row| row.error_summary.as_str())
                    .collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

fn parse_session_batches(
    batches: &[RecordBatch],
    profiles: &HashMap<String, ChatSessionProfileRecord>,
) -> Result<Vec<SessionSummary>, ArchitectError> {
    Ok(parse_session_records(batches, profiles)?
        .into_iter()
        .map(|record| session_record_to_summary(&record))
        .collect())
}

fn parse_session_records(
    batches: &[RecordBatch],
    profiles: &HashMap<String, ChatSessionProfileRecord>,
) -> Result<Vec<ChatSessionRecord>, ArchitectError> {
    let mut out = Vec::new();
    for batch in batches {
        let session_ids = string_column(batch, "session_id")?;
        let titles = string_column(batch, "title")?;
        let agents = string_column(batch, "agent")?;
        let created = u64_column(batch, "created_at_ms")?;
        let updated = u64_column(batch, "last_activity_at_ms")?;
        let counts = u64_column(batch, "message_count")?;
        let previews = string_column(batch, "last_message_preview")?;
        for idx in 0..batch.num_rows() {
            let session_id = session_ids.value(idx).to_string();
            let profile = profiles.get(&session_id).cloned().unwrap_or_else(|| {
                session_profile_record(&default_session_profile_with_id(&session_id))
            });
            out.push(ChatSessionRecord {
                session_id,
                title: session_title_or_default(titles.value(idx)),
                agent: agents.value(idx).to_string(),
                created_at_ms: created.value(idx),
                last_activity_at_ms: updated.value(idx),
                message_count: counts.value(idx),
                last_message_preview: previews.value(idx).to_string(),
                chat_mode: profile.chat_mode,
                effective_ich_id: profile.effective_ich_id,
                effective_ilk: profile.effective_ilk,
                impersonation_target: profile.impersonation_target,
                thread_id: profile.thread_id,
                source_channel_kind: profile.source_channel_kind,
                debug_enabled: profile.debug_enabled,
            });
        }
    }
    Ok(out)
}

fn parse_message_records(
    batches: &[RecordBatch],
) -> Result<Vec<ChatMessageRecord>, ArchitectError> {
    let mut out = Vec::new();
    for batch in batches {
        let message_ids = string_column(batch, "message_id")?;
        let session_ids = string_column(batch, "session_id")?;
        let roles = string_column(batch, "role")?;
        let contents = string_column(batch, "content")?;
        let timestamps = u64_column(batch, "timestamp_ms")?;
        let modes = string_column(batch, "mode")?;
        let metadata = string_column(batch, "metadata_json")?;
        let seqs = u64_column(batch, "seq")?;
        for idx in 0..batch.num_rows() {
            out.push(ChatMessageRecord {
                message_id: message_ids.value(idx).to_string(),
                session_id: session_ids.value(idx).to_string(),
                role: roles.value(idx).to_string(),
                content: contents.value(idx).to_string(),
                timestamp_ms: timestamps.value(idx),
                mode: modes.value(idx).to_string(),
                metadata_json: metadata.value(idx).to_string(),
                seq: seqs.value(idx),
            });
        }
    }
    Ok(out)
}

fn parse_operation_records(
    batches: &[RecordBatch],
) -> Result<Vec<ChatOperationRecord>, ArchitectError> {
    let mut out = Vec::new();
    for batch in batches {
        let operation_ids = string_column(batch, "operation_id")?;
        let session_ids = string_column(batch, "session_id")?;
        let scope_ids = string_column(batch, "scope_id")?;
        let origins = string_column(batch, "origin")?;
        let actions = string_column(batch, "action")?;
        let target_hives = string_column(batch, "target_hive")?;
        let params_json = string_column(batch, "params_json")?;
        let params_hash = string_column(batch, "params_hash")?;
        let preview_commands = string_column(batch, "preview_command")?;
        let statuses = string_column(batch, "status")?;
        let created = u64_column(batch, "created_at_ms")?;
        let updated = u64_column(batch, "updated_at_ms")?;
        let dispatched = u64_column(batch, "dispatched_at_ms")?;
        let completed = u64_column(batch, "completed_at_ms")?;
        let request_ids = string_column(batch, "request_id")?;
        let trace_ids = string_column(batch, "trace_id")?;
        let error_summaries = string_column(batch, "error_summary")?;
        for idx in 0..batch.num_rows() {
            out.push(ChatOperationRecord {
                operation_id: operation_ids.value(idx).to_string(),
                session_id: session_ids.value(idx).to_string(),
                scope_id: scope_ids.value(idx).to_string(),
                origin: origins.value(idx).to_string(),
                action: actions.value(idx).to_string(),
                target_hive: target_hives.value(idx).to_string(),
                params_json: params_json.value(idx).to_string(),
                params_hash: params_hash.value(idx).to_string(),
                preview_command: preview_commands.value(idx).to_string(),
                status: statuses.value(idx).to_string(),
                created_at_ms: created.value(idx),
                updated_at_ms: updated.value(idx),
                dispatched_at_ms: dispatched.value(idx),
                completed_at_ms: completed.value(idx),
                request_id: request_ids.value(idx).to_string(),
                trace_id: trace_ids.value(idx).to_string(),
                error_summary: error_summaries.value(idx).to_string(),
            });
        }
    }
    Ok(out)
}

fn string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, ArchitectError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| format!("missing column: {name}").into())
        .and_then(|column| {
            column
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| format!("invalid string column: {name}").into())
        })
}

fn u64_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array, ArchitectError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| format!("missing column: {name}").into())
        .and_then(|column| {
            column
                .as_any()
                .downcast_ref::<UInt64Array>()
                .ok_or_else(|| format!("invalid u64 column: {name}").into())
        })
}

fn bool_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a arrow_array::BooleanArray, ArchitectError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| format!("missing column: {name}").into())
        .and_then(|column| {
            column
                .as_any()
                .downcast_ref::<arrow_array::BooleanArray>()
                .ok_or_else(|| format!("invalid bool column: {name}").into())
        })
}

fn session_record_to_summary(record: &ChatSessionRecord) -> SessionSummary {
    SessionSummary {
        session_id: record.session_id.clone(),
        title: session_title_or_default(&record.title),
        agent: record.agent.clone(),
        created_at_ms: record.created_at_ms,
        last_activity_at_ms: record.last_activity_at_ms,
        message_count: record.message_count,
        last_message_preview: if record.last_message_preview.trim().is_empty() {
            None
        } else {
            Some(record.last_message_preview.clone())
        },
        chat_mode: record.chat_mode.clone(),
        effective_ich_id: none_if_empty(&record.effective_ich_id),
        effective_ilk: none_if_empty(&record.effective_ilk),
        impersonation_target: none_if_empty(&record.impersonation_target),
        thread_id: none_if_empty(&record.thread_id),
        source_channel_kind: none_if_empty(&record.source_channel_kind),
        debug_enabled: record.debug_enabled,
    }
}

fn build_session_profile(req: CreateSessionRequest) -> Result<ChatSessionRecord, ArchitectError> {
    let chat_mode = normalize_chat_mode(req.chat_mode.as_deref())?;
    let effective_ich_id = sanitize_optional_identity(req.effective_ich_id);
    let effective_ilk = sanitize_optional_identity(req.effective_ilk);
    let impersonation_target = sanitize_optional_identity(req.impersonation_target);
    let thread_id = sanitize_optional_value(req.thread_id);
    let source_channel_kind = sanitize_optional_value(req.source_channel_kind);
    if chat_mode == CHAT_MODE_IMPERSONATION
        && effective_ich_id.is_empty()
        && effective_ilk.is_empty()
        && impersonation_target.is_empty()
    {
        return Err(
            "impersonation chat mode requires at least effective_ich_id, effective_ilk or impersonation_target"
                .to_string()
                .into(),
        );
    }
    let debug_enabled = req
        .debug_enabled
        .unwrap_or(chat_mode == CHAT_MODE_IMPERSONATION);
    Ok(ChatSessionRecord {
        session_id: String::new(),
        title: String::new(),
        agent: String::new(),
        created_at_ms: 0,
        last_activity_at_ms: 0,
        message_count: 0,
        last_message_preview: String::new(),
        chat_mode,
        effective_ich_id,
        effective_ilk,
        impersonation_target,
        thread_id,
        source_channel_kind,
        debug_enabled,
    })
}

fn default_session_profile() -> ChatSessionRecord {
    ChatSessionRecord {
        session_id: String::new(),
        title: String::new(),
        agent: String::new(),
        created_at_ms: 0,
        last_activity_at_ms: 0,
        message_count: 0,
        last_message_preview: String::new(),
        chat_mode: CHAT_MODE_OPERATOR.to_string(),
        effective_ich_id: String::new(),
        effective_ilk: String::new(),
        impersonation_target: String::new(),
        thread_id: String::new(),
        source_channel_kind: String::new(),
        debug_enabled: false,
    }
}

fn default_session_profile_with_id(session_id: &str) -> ChatSessionRecord {
    let mut record = default_session_profile();
    record.session_id = session_id.to_string();
    record
}

fn session_profile_from_record(record: &ChatSessionRecord) -> ChatSessionProfileRecord {
    ChatSessionProfileRecord {
        session_id: record.session_id.clone(),
        chat_mode: record.chat_mode.clone(),
        effective_ich_id: record.effective_ich_id.clone(),
        effective_ilk: record.effective_ilk.clone(),
        impersonation_target: record.impersonation_target.clone(),
        thread_id: record.thread_id.clone(),
        source_channel_kind: record.source_channel_kind.clone(),
        debug_enabled: record.debug_enabled,
    }
}

fn session_profile_record(record: &ChatSessionRecord) -> ChatSessionProfileRecord {
    session_profile_from_record(record)
}

fn normalize_chat_mode(value: Option<&str>) -> Result<String, ArchitectError> {
    let mode = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(CHAT_MODE_OPERATOR);
    match mode {
        CHAT_MODE_OPERATOR | CHAT_MODE_IMPERSONATION => Ok(mode.to_string()),
        other => Err(format!(
            "invalid chat_mode '{other}'. expected '{CHAT_MODE_OPERATOR}' or '{CHAT_MODE_IMPERSONATION}'"
        )
        .into()),
    }
}

fn sanitize_optional_identity(value: Option<String>) -> String {
    sanitize_optional_value(value)
}

fn sanitize_optional_value(value: Option<String>) -> String {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| preview_text(value, 120))
        .unwrap_or_default()
}

fn none_if_empty(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn session_scope_id(session: &ChatSessionRecord) -> Option<String> {
    if let Some(effective_ich_id) = none_if_empty(&session.effective_ich_id) {
        return Some(effective_ich_id);
    }
    if let Some(effective_ilk) = none_if_empty(&session.effective_ilk) {
        return Some(effective_ilk);
    }
    if let Some(impersonation_target) = none_if_empty(&session.impersonation_target) {
        return Some(impersonation_target);
    }
    if let Some(thread_id) = none_if_empty(&session.thread_id) {
        return Some(thread_id);
    }
    None
}

async fn load_session_profile_map(
    table: &lancedb::Table,
    session_id: Option<&str>,
) -> Result<HashMap<String, ChatSessionProfileRecord>, ArchitectError> {
    let mut query = table.query();
    if let Some(session_id) = session_id {
        query = query.only_if(session_filter(session_id));
    }
    let batches = query
        .select(Select::columns(&[
            "session_id",
            "chat_mode",
            "effective_ich_id",
            "effective_ilk",
            "impersonation_target",
            "thread_id",
            "source_channel_kind",
            "debug_enabled",
        ]))
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    let rows = parse_session_profile_records(&batches)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.session_id.clone(), row))
        .collect())
}

fn parse_session_profile_records(
    batches: &[RecordBatch],
) -> Result<Vec<ChatSessionProfileRecord>, ArchitectError> {
    let mut out = Vec::new();
    for batch in batches {
        let session_ids = string_column(batch, "session_id")?;
        let chat_modes = string_column(batch, "chat_mode")?;
        let effective_ich_ids = string_column(batch, "effective_ich_id")?;
        let effective_ilks = string_column(batch, "effective_ilk")?;
        let impersonation_targets = string_column(batch, "impersonation_target")?;
        let thread_ids = string_column(batch, "thread_id")?;
        let source_channel_kinds = string_column(batch, "source_channel_kind")?;
        let debug_enabled = bool_column(batch, "debug_enabled")?;
        for idx in 0..batch.num_rows() {
            out.push(ChatSessionProfileRecord {
                session_id: session_ids.value(idx).to_string(),
                chat_mode: chat_modes.value(idx).to_string(),
                effective_ich_id: effective_ich_ids.value(idx).to_string(),
                effective_ilk: effective_ilks.value(idx).to_string(),
                impersonation_target: impersonation_targets.value(idx).to_string(),
                thread_id: thread_ids.value(idx).to_string(),
                source_channel_kind: source_channel_kinds.value(idx).to_string(),
                debug_enabled: debug_enabled.value(idx),
            });
        }
    }
    Ok(out)
}

fn message_record_to_persisted(record: ChatMessageRecord) -> PersistedChatMessage {
    PersistedChatMessage {
        message_id: record.message_id,
        session_id: record.session_id,
        role: record.role,
        content: record.content,
        timestamp_ms: record.timestamp_ms,
        mode: record.mode,
        metadata: serde_json::from_str(&record.metadata_json)
            .unwrap_or_else(|_| json!({ "kind": "text" })),
    }
}

fn persisted_message_label(message: &PersistedChatMessage) -> Option<String> {
    message
        .metadata
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn sanitize_session_title(title: Option<&str>) -> String {
    title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| preview_text(value, 56))
        .unwrap_or_else(|| "Untitled chat".to_string())
}

fn session_title_or_default(title: &str) -> String {
    if title.trim().is_empty() {
        "Untitled chat".to_string()
    } else {
        title.to_string()
    }
}

fn update_title_from_message(current_title: &str, message: &str) -> String {
    if current_title != "Untitled chat" {
        return current_title.to_string();
    }
    if confirmation_requested(message) || cancellation_requested(message) {
        return current_title.to_string();
    }
    let normalized = compact_title_candidate(message);
    if normalized.is_empty() {
        current_title.to_string()
    } else {
        preview_text(&normalized, 56)
    }
}

fn compact_title_candidate(message: &str) -> String {
    let trimmed = message.trim();
    if let Some(raw) = trimmed.strip_prefix("SCMD:") {
        return compact_scmd_title(raw.trim());
    }
    if trimmed.starts_with("ACMD:") {
        return "Legacy ACMD command".to_string();
    }
    if trimmed.starts_with("FCMD:") {
        return "Legacy FCMD command".to_string();
    }
    trimmed.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_scmd_title(raw: &str) -> String {
    match parse_scmd(raw) {
        Ok(parsed) => format!("{} {}", parsed.method, preview_text(&parsed.path, 40)),
        Err(_) => format!("SCMD {}", preview_text(raw, 40)),
    }
}

fn sanitize_user_message_for_persistence(user_message: &str) -> String {
    let trimmed = user_message.trim();
    let Some(raw) = trimmed.strip_prefix("SCMD:") else {
        return user_message.to_string();
    };
    match parse_scmd(raw.trim()) {
        Ok(parsed) => {
            let redacted_body = parsed.body.as_ref().map(redact_json_secret_fields);
            format!(
                "SCMD: {}",
                scmd_raw_from_parts(&parsed.method, &parsed.path, redacted_body.as_ref())
            )
        }
        Err(_) => user_message.to_string(),
    }
}

fn redact_json_secret_fields(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if key_looks_secret(key) {
                        (
                            key.clone(),
                            Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()),
                        )
                    } else {
                        (key.clone(), redact_json_secret_fields(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_secret_fields).collect()),
        other => other.clone(),
    }
}

fn key_looks_secret(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "api_key" | "apikey" | "token" | "access_token" | "client_secret" | "secret" | "password"
    ) || normalized.ends_with("_api_key")
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
}

fn session_filter(session_id: &str) -> String {
    format!("session_id = '{}'", escape_sql_literal(session_id))
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn preview_text(input: &str, max_chars: usize) -> String {
    let compact = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let truncated = compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        .trim()
        .to_string();
    format!("{truncated}...")
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
        || path == "/api/sessions"
        || path == "/api/identity/ich-options"
        || path.starts_with("/api/sessions/")
        || path.ends_with("/api/status")
        || path.ends_with("/api/chat")
        || path.ends_with("/api/sessions")
        || path == "/healthz"
}

fn is_status_path(path: &str) -> bool {
    path == "/healthz" || path == "/api/status" || path.ends_with("/api/status")
}

fn is_chat_path(path: &str) -> bool {
    path == "/api/chat" || path.ends_with("/api/chat")
}

fn is_executor_plan_path(path: &str) -> bool {
    path == "/api/executor/plan" || path.ends_with("/api/executor/plan")
}

fn is_attachments_path(path: &str) -> bool {
    path == "/api/attachments" || path.ends_with("/api/attachments")
}

fn is_identity_ich_options_path(path: &str) -> bool {
    path == "/api/identity/ich-options" || path.ends_with("/api/identity/ich-options")
}

fn is_favicon_path(path: &str) -> bool {
    matches!(path, "/favicon.svg")
}

fn serve_favicon(path: &str) -> Response {
    match path {
        "/favicon.svg" => (
            [(CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
            FAVICON_SVG,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn is_sessions_collection_path(path: &str) -> bool {
    path == "/api/sessions" || path.ends_with("/api/sessions")
}

fn is_session_detail_path(path: &str) -> bool {
    path.starts_with("/api/sessions/") && session_id_from_path(path).is_some()
}

fn session_id_from_path(path: &str) -> Option<&str> {
    path.split("/api/sessions/")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn architect_index_html(state: &ArchitectState) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
  <head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>SY.architect</title>
  <link rel="icon" href="/favicon.svg" type="image/svg+xml">
  <style>
    :root {{
      --bg: #ffffff;
      --panel: #ffffff;
      --panel-alt: #f8f9fb;
      --panel-soft: #f3f6fc;
      --text: #171717;
      --muted: #667085;
      --line: #e6e9f0;
      --accent: #0070F3;
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
      height: 100vh;
      overflow: hidden;
    }}
    .page {{
      width: min(1320px, calc(100vw - 32px));
      margin: 0 auto;
      padding: 22px 0 28px;
      height: 100vh;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
      overflow: hidden;
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
      flex-direction: column;
      gap: 10px;
      align-items: flex-end;
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
      align-items: stretch;
      min-height: 0;
      overflow: hidden;
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
      min-height: 0;
      max-height: 100%;
      overflow: auto;
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
      padding: 4px 9px;
      font-size: 0.72rem;
      color: var(--muted);
      background: #ffffff;
    }}
    .mini-pill.action {{
      cursor: pointer;
      font-weight: 700;
    }}
    .new-chat {{
      width: 100%;
      min-height: 38px;
      padding: 8px 12px;
      font-size: 0.84rem;
      font-weight: 700;
      letter-spacing: 0.01em;
      border: 1px solid transparent;
      background: #f6f8fc;
      color: #243142;
      justify-content: center;
      box-shadow: none;
      transition: border-color 140ms ease, background 140ms ease, color 140ms ease,
        transform 140ms ease;
    }}
    .new-chat-row {{
      display: grid;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
      gap: 10px;
      margin-bottom: 14px;
    }}
    .new-chat.operator {{
      background: #f4f5f7;
      border-color: #d9dee8;
      color: #1f2a37;
    }}
    .new-chat.debug {{
      background: #edf4ff;
      border-color: #c9dafb;
      color: #1f5fbf;
    }}
    .new-chat:hover {{
      transform: translateY(-1px);
      background: #eef2f8;
    }}
    .new-chat.debug:hover {{
      background: #e5efff;
    }}
    .new-chat.operator:hover {{
      background: #eceff4;
    }}
    .history-mode {{
      display: inline-flex;
      align-items: center;
      gap: 6px;
      margin-top: 8px;
      flex-wrap: wrap;
    }}
    .mode-badge {{
      display: inline-flex;
      align-items: center;
      border-radius: 999px;
      padding: 4px 8px;
      font-size: 0.7rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      background: #eef2f8;
      color: #5c6c84;
      max-width: 100%;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }}
    .mode-badge.operator {{
      background: #e8f1ff;
      color: #1f5fbf;
    }}
    .mode-badge.impersonation {{
      background: #fff1cf;
      color: #8a5a00;
    }}
    .mode-badge.context {{
      letter-spacing: 0.03em;
      text-transform: none;
      font-weight: 700;
      max-width: 100%;
    }}
    .history-search {{
      width: 100%;
      margin-bottom: 14px;
      border: 1px solid var(--line);
      border-radius: 16px;
      padding: 11px 13px;
      font: inherit;
      font-size: 0.9rem;
      color: var(--text);
      background: #ffffff;
      outline: none;
    }}
    .history-search:focus {{
      border-color: #b8caef;
      box-shadow: 0 0 0 4px rgba(69, 117, 220, 0.12);
    }}
    .history-stats {{
      margin-bottom: 12px;
      padding: 0 2px;
      color: var(--muted);
      font-size: 0.76rem;
      line-height: 1.35;
    }}
    .history-list {{
      display: grid;
      gap: 10px;
    }}
    .history-card {{
      position: relative;
      padding: 14px 14px 13px;
      border-radius: 18px;
      border: 1px solid var(--line);
      background: var(--panel-alt);
      cursor: pointer;
      overflow: hidden;
      transition: border-color 140ms ease, box-shadow 140ms ease, transform 140ms ease;
    }}
    .history-card:hover {{
      border-color: #d7e4ff;
      box-shadow: 0 10px 24px rgba(31, 42, 55, 0.08);
      transform: translateY(-1px);
    }}
    .history-card.active {{
      background: linear-gradient(180deg, #f5f8ff 0%, #eef3ff 100%);
      border-color: #d7e4ff;
      box-shadow: 0 12px 28px rgba(69, 117, 220, 0.12);
    }}
    .history-card.placeholder {{
      border-style: dashed;
      background: #fbfcfe;
    }}
    .history-card.active::before {{
      content: "";
      position: absolute;
      top: 12px;
      left: 12px;
      width: 8px;
      height: 8px;
      border-radius: 999px;
      background: var(--accent);
      box-shadow: 0 0 0 4px rgba(69, 117, 220, 0.14);
    }}
    .history-card-head {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 10px;
    }}
    .history-name {{
      font-size: 0.95rem;
      font-weight: 700;
      margin-bottom: 3px;
      max-width: 100%;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }}
    .history-meta,
    .meta-grid {{
      color: var(--muted);
      font-size: 0.82rem;
      line-height: 1.45;
    }}
    .history-meta {{
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 6px;
      min-width: 0;
    }}
    .history-meta-sep {{
      opacity: 0.45;
    }}
    .history-meta-updated {{
      min-width: 0;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }}
    .history-preview {{
      margin-top: 8px;
      color: var(--text);
      font-size: 0.82rem;
      line-height: 1.4;
      max-width: 100%;
      display: -webkit-box;
      -webkit-line-clamp: 2;
      -webkit-box-orient: vertical;
      overflow: hidden;
    }}
    .history-delete {{
      border: 1px solid var(--line);
      background: #ffffff;
      color: var(--muted);
      border-radius: 999px;
      width: 28px;
      height: 28px;
      padding: 0;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      flex: none;
      font-size: 0.9rem;
      line-height: 1;
    }}
    .history-delete:hover {{
      color: #b42318;
      border-color: #f2c7c4;
      background: #fff5f4;
    }}
    .meta-grid {{
      margin-top: 16px;
      display: grid;
      gap: 8px;
      padding-top: 16px;
      border-top: 1px solid var(--line);
    }}
    .meta-inline-note {{
      color: var(--muted);
      font-size: 0.82rem;
      line-height: 1.45;
    }}
    .history-open-badge {{
      display: inline-flex;
      align-items: center;
      margin-bottom: 6px;
      border-radius: 999px;
      padding: 3px 8px;
      background: rgba(69, 117, 220, 0.12);
      color: var(--accent);
      font-size: 0.68rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
    }}
    .meta-grid strong {{
      color: var(--text);
      font-weight: 700;
    }}
    .modal-backdrop {{
      position: fixed;
      inset: 0;
      background: rgba(19, 28, 43, 0.36);
      display: none;
      align-items: center;
      justify-content: center;
      padding: 18px;
      z-index: 40;
    }}
    .modal-backdrop.open {{
      display: flex;
    }}
    .modal-card {{
      width: min(560px, 100%);
      border-radius: 24px;
      border: 1px solid var(--line);
      background: #ffffff;
      box-shadow: 0 22px 60px rgba(16, 24, 40, 0.18);
      padding: 22px;
      display: grid;
      gap: 16px;
    }}
    .modal-head {{
      display: flex;
      align-items: flex-start;
      justify-content: space-between;
      gap: 14px;
    }}
    .modal-kicker {{
      color: var(--accent);
      font-size: 0.74rem;
      font-weight: 800;
      letter-spacing: 0.11em;
      text-transform: uppercase;
      margin-bottom: 6px;
    }}
    .modal-title {{
      margin: 0;
      font-size: 1.35rem;
      line-height: 1.05;
      letter-spacing: -0.03em;
    }}
    .modal-copy {{
      margin: 0;
      color: var(--muted);
      font-size: 0.9rem;
      line-height: 1.5;
    }}
    .modal-close {{
      width: 34px;
      height: 34px;
      padding: 0;
      border-radius: 999px;
      border: 1px solid var(--line);
      background: #ffffff;
      color: var(--muted);
      font-size: 1rem;
      display: inline-flex;
      align-items: center;
      justify-content: center;
    }}
    .modal-form {{
      display: grid;
      gap: 12px;
    }}
    .field-grid {{
      display: grid;
      gap: 12px;
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }}
    .field {{
      display: grid;
      gap: 6px;
    }}
    .field.span-2 {{
      grid-column: 1 / -1;
    }}
    .field label {{
      font-size: 0.78rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }}
    .field input,
    .field select {{
      width: 100%;
      border: 1px solid var(--line);
      border-radius: 14px;
      padding: 11px 12px;
      font: inherit;
      color: var(--text);
      background: #ffffff;
      outline: none;
    }}
    .field input:focus,
    .field select:focus {{
      border-color: #b8caef;
      box-shadow: 0 0 0 4px rgba(69, 117, 220, 0.12);
    }}
    .field-help {{
      color: var(--muted);
      font-size: 0.78rem;
      line-height: 1.4;
    }}
    .modal-error {{
      min-height: 1.2em;
      color: #b42318;
      font-size: 0.82rem;
      line-height: 1.4;
    }}
    .confirm-copy {{
      margin: 0;
      color: var(--text);
      font-size: 0.98rem;
      line-height: 1.55;
    }}
    .modal-actions {{
      display: flex;
      justify-content: flex-end;
      gap: 10px;
    }}
    .secondary-button {{
      background: #ffffff;
      color: var(--text);
      border: 1px solid var(--line);
    }}
    .danger-button {{
      background: #b42318;
      color: #ffffff;
    }}
    .shell {{
      overflow: hidden;
      min-height: 0;
      height: 100%;
      max-height: 100%;
      align-self: stretch;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr) auto;
    }}
    .shell-head {{
      padding: 13px 20px 12px;
      border-bottom: 1px solid var(--line);
      background: linear-gradient(180deg, #ffffff 0%, #fafbfd 100%);
    }}
    .shell-title-row {{
      display: flex;
      align-items: center;
      gap: 10px;
    }}
    .shell-title {{
      margin: 0;
      font-size: 1.1rem;
      line-height: 1.1;
      letter-spacing: -0.04em;
    }}
    .shell-title-meta {{
      color: var(--muted);
      font-size: 0.92rem;
      line-height: 1.1;
    }}
    .messages {{
      min-height: 0;
      height: 100%;
      overflow: auto;
      padding: 22px 20px 18px;
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
    .msg.pending {{
      border-style: dashed;
      border-color: #d7e4ff;
      background: linear-gradient(180deg, #fbfdff 0%, #f5f8ff 100%);
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
    .pending-body {{
      display: inline-flex;
      align-items: center;
      gap: 10px;
      color: var(--muted);
      white-space: normal;
    }}
    .pending-text {{
      font-size: 0.92rem;
      font-weight: 600;
      color: #55637c;
    }}
    .thinking-dots {{
      display: inline-flex;
      align-items: center;
      gap: 6px;
    }}
    .thinking-dots span {{
      width: 8px;
      height: 8px;
      border-radius: 999px;
      background: #9cb2de;
      opacity: 0.28;
      animation: thinkingPulse 1.2s infinite ease-in-out;
    }}
    .thinking-dots span:nth-child(2) {{
      animation-delay: 0.16s;
    }}
    .thinking-dots span:nth-child(3) {{
      animation-delay: 0.32s;
    }}
    @keyframes thinkingPulse {{
      0%, 80%, 100% {{
        opacity: 0.25;
        transform: translateY(0);
      }}
      40% {{
        opacity: 1;
        transform: translateY(-2px);
      }}
    }}
    .result-shell {{
      display: grid;
      gap: 10px;
    }}
    .result-head {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 10px;
      flex-wrap: wrap;
    }}
    .result-title {{
      font-size: 0.94rem;
      font-weight: 700;
      color: var(--text);
    }}
    .result-status {{
      border-radius: 999px;
      padding: 5px 9px;
      font-size: 0.72rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      background: var(--panel-soft);
      color: var(--muted);
    }}
    .result-status.ok {{
      background: var(--success-soft);
      color: var(--success);
    }}
    .result-status.warn {{
      background: var(--warning-soft);
      color: var(--warning);
    }}
    .result-meta {{
      color: var(--muted);
      font-size: 0.78rem;
      line-height: 1.45;
    }}
    .result-section {{
      display: grid;
      gap: 6px;
    }}
    .result-section-title {{
      font-size: 0.72rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }}
    .result-section-preview {{
      color: var(--muted);
      font-size: 0.82rem;
      line-height: 1.45;
      word-break: break-word;
    }}
    .result-section details {{
      border-top: 1px dashed rgba(210, 218, 232, 0.9);
      padding-top: 8px;
    }}
    .result-section summary {{
      cursor: pointer;
      color: var(--accent);
      font-size: 0.76rem;
      font-weight: 700;
      list-style: none;
    }}
    .result-section summary::-webkit-details-marker {{
      display: none;
    }}
    .exec-shell {{
      display: grid;
      gap: 10px;
    }}
    .exec-head {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 8px;
      flex-wrap: wrap;
    }}
    .exec-title {{
      font-size: 0.94rem;
      font-weight: 700;
      color: var(--text);
    }}
    .exec-subtitle {{
      font-size: 0.78rem;
      color: var(--muted);
      margin-top: 2px;
    }}
    .exec-steps {{
      display: grid;
      gap: 6px;
    }}
    .exec-step {{
      border: 1px solid var(--line);
      border-radius: 12px;
      overflow: hidden;
    }}
    .exec-step-head {{
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 9px 13px;
      cursor: pointer;
      user-select: none;
    }}
    .exec-step-head:hover {{
      background: rgba(0,0,0,0.02);
    }}
    .exec-step-icon {{
      font-size: 0.78rem;
      width: 16px;
      text-align: center;
      flex-shrink: 0;
    }}
    .exec-step-name {{
      font-size: 0.85rem;
      font-weight: 700;
      color: var(--text);
      flex: 1;
    }}
    .exec-step-id {{
      font-size: 0.75rem;
      color: var(--muted);
      font-family: monospace;
    }}
    .exec-step-badge {{
      font-size: 0.72rem;
      font-weight: 700;
      border-radius: 999px;
      padding: 2px 8px;
      text-transform: uppercase;
      letter-spacing: 0.04em;
    }}
    .exec-step-badge.ok {{
      background: var(--success-soft);
      color: var(--success);
    }}
    .exec-step-badge.warn {{
      background: var(--warning-soft);
      color: var(--warning);
    }}
    .exec-step-badge.muted {{
      background: #f0f2f6;
      color: var(--muted);
    }}
    .exec-step-body {{
      padding: 0 13px 11px 37px;
      display: grid;
      gap: 6px;
    }}
    .exec-step-args {{
      font-size: 0.78rem;
      color: var(--muted);
      font-family: monospace;
      white-space: pre-wrap;
      word-break: break-all;
    }}
    .exec-step-preview {{
      font-size: 0.80rem;
      color: var(--muted);
      line-height: 1.45;
    }}
    .exec-step-error {{
      font-size: 0.82rem;
      color: var(--warning);
      line-height: 1.45;
    }}
    .exec-step details {{
      border-top: 1px dashed rgba(210, 218, 232, 0.9);
      padding-top: 8px;
      margin-top: 4px;
    }}
    .exec-step summary {{
      cursor: pointer;
      color: var(--accent);
      font-size: 0.76rem;
      font-weight: 700;
      list-style: none;
    }}
    .exec-step summary::-webkit-details-marker {{
      display: none;
    }}
    .exec-error-block {{
      border: 1px solid var(--warning-soft);
      border-radius: 12px;
      padding: 10px 13px;
      font-size: 0.82rem;
      color: var(--warning);
      line-height: 1.5;
    }}
    .result-pre {{
      margin: 0;
      padding: 12px 13px;
      border-radius: 14px;
      border: 1px solid var(--line);
      background: #fbfcfe;
      overflow: auto;
      max-height: min(40vh, 420px);
      white-space: pre-wrap;
      word-break: break-word;
      font-size: 0.82rem;
      line-height: 1.45;
      font-family: "SFMono-Regular", "Menlo", "Consolas", monospace;
    }}
    .chat-response {{
      display: grid;
      gap: 12px;
    }}
    .tool-summary {{
      display: grid;
      gap: 10px;
      margin-top: 2px;
      padding-top: 10px;
      border-top: 1px solid rgba(210, 218, 232, 0.9);
    }}
    .tool-summary-toggle {{
      border-top: 1px solid rgba(210, 218, 232, 0.9);
      padding-top: 10px;
    }}
    .tool-summary-toggle summary {{
      cursor: pointer;
      list-style: none;
    }}
    .tool-summary-toggle summary::-webkit-details-marker {{
      display: none;
    }}
    .tool-summary-head {{
      font-size: 0.72rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      color: var(--muted);
    }}
    .tool-summary-meta {{
      margin-top: 4px;
      color: var(--muted);
      font-size: 0.8rem;
      line-height: 1.4;
    }}
    .tool-list {{
      display: grid;
      gap: 8px;
    }}
    .tool-card {{
      display: grid;
      gap: 6px;
      padding: 10px 12px;
      border-radius: 14px;
      border: 1px solid var(--line);
      background: #fbfcfe;
    }}
    .tool-card-head {{
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 10px;
      flex-wrap: wrap;
    }}
    .tool-card-name {{
      font-size: 0.84rem;
      font-weight: 700;
      color: var(--text);
    }}
    .tool-card-badge {{
      border-radius: 999px;
      padding: 4px 8px;
      font-size: 0.68rem;
      font-weight: 800;
      letter-spacing: 0.08em;
      text-transform: uppercase;
      background: var(--accent-soft);
      color: var(--accent);
    }}
    .tool-card-badge.error {{
      background: var(--warning-soft);
      color: var(--warning);
    }}
    .tool-card-summary {{
      color: var(--muted);
      font-size: 0.8rem;
      line-height: 1.45;
    }}
    .tool-card-row {{
      display: grid;
      gap: 4px;
      font-size: 0.78rem;
      line-height: 1.4;
      color: var(--muted);
      word-break: break-word;
    }}
    .tool-card-row strong {{
      color: var(--text);
    }}
    .tool-card-row-preview {{
      color: var(--muted);
      word-break: break-word;
    }}
    .tool-card details {{
      border-top: 1px dashed rgba(210, 218, 232, 0.9);
      padding-top: 8px;
    }}
    .tool-card summary {{
      cursor: pointer;
      color: var(--accent);
      font-size: 0.76rem;
      font-weight: 700;
      list-style: none;
    }}
    .tool-card summary::-webkit-details-marker {{
      display: none;
    }}
    .composer {{
      border-top: 1px solid var(--line);
      padding: 16px 20px 14px;
      background: #ffffff;
    }}
    .composer-box {{
      display: grid;
      gap: 8px;
    }}
    .attachment-draft {{
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      min-height: 0;
    }}
    .attachment-draft:empty {{
      display: none;
    }}
    .attachment-chip {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      max-width: 100%;
      padding: 8px 10px;
      border-radius: 14px;
      border: 1px solid var(--line);
      background: var(--panel-soft);
      color: var(--text);
      font-size: 0.78rem;
      line-height: 1.3;
    }}
    .attachment-chip-name {{
      font-weight: 700;
      word-break: break-word;
    }}
    .attachment-chip-meta {{
      color: var(--muted);
      font-size: 0.74rem;
    }}
    .attachment-chip-remove {{
      border: 0;
      background: transparent;
      color: var(--muted);
      cursor: pointer;
      padding: 0;
      min-width: auto;
      font-size: 1rem;
      line-height: 1;
      box-shadow: none;
    }}
    .attachment-chip-remove:hover {{
      color: var(--text);
    }}
    .message-attachments {{
      display: flex;
      flex-wrap: wrap;
      gap: 8px;
      margin-top: 10px;
    }}
    .message-attachment {{
      display: inline-flex;
      flex-direction: column;
      gap: 2px;
      padding: 8px 10px;
      border-radius: 12px;
      border: 1px solid var(--line);
      background: #fbfcfe;
      min-width: 0;
      max-width: 100%;
    }}
    .message-attachment-name {{
      font-size: 0.76rem;
      font-weight: 700;
      color: var(--text);
      word-break: break-word;
    }}
    .message-attachment-meta {{
      font-size: 0.72rem;
      color: var(--muted);
      word-break: break-word;
    }}
    .composer-input-shell {{
      position: relative;
    }}
    textarea {{
      width: 100%;
      min-height: 86px;
      resize: vertical;
      border: 1px solid var(--line);
      border-radius: 22px;
      padding: 16px 172px 16px 20px;
      font: inherit;
      background: var(--panel);
      color: var(--text);
      outline: none;
      overflow-wrap: break-word;
      word-break: break-word;
    }}
    textarea:focus {{
      border-color: #b8caef;
      box-shadow: 0 0 0 4px rgba(69, 117, 220, 0.12);
    }}
    .composer-actions {{
      position: absolute;
      right: 14px;
      top: 50%;
      bottom: auto;
      transform: translateY(-50%);
      display: flex;
      align-items: center;
      gap: 10px;
    }}
    .hint {{
      color: var(--muted);
      font-size: 0.77rem;
      line-height: 1.35;
      padding: 0 4px;
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
    #send {{
      min-width: 96px;
      padding: 12px 20px;
      box-shadow: 0 10px 24px rgba(23, 23, 23, 0.14);
    }}
    #attach {{
      min-width: auto;
      padding: 10px 14px;
      background: var(--panel-soft);
      color: var(--text);
      border: 1px solid var(--line);
      box-shadow: none;
    }}
    #attach:hover {{
      background: #eaf0fb;
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
        max-height: 260px;
      }}
      .shell {{
        min-height: auto;
        max-height: none;
      }}
    }}
    @media (max-width: 760px) {{
      .page {{
        width: min(100vw - 20px, 1320px);
        padding-top: 14px;
        padding-bottom: 14px;
      }}
      .masthead,
      .shell-title-row {{
        flex-direction: column;
        align-items: stretch;
      }}
      .topbar {{
        align-items: stretch;
      }}
      .status-strip {{
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
      .composer {{
        padding-left: 14px;
        padding-right: 14px;
      }}
      .composer-actions {{
        right: 12px;
        top: 50%;
        bottom: auto;
        transform: translateY(-50%);
      }}
      #send {{
        min-width: 84px;
      }}
      .msg {{
        max-width: 100%;
      }}
      .field-grid {{
        grid-template-columns: 1fr;
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
            <span>archi &middot; system architect interface</span>
          </div>
        </div>
      </div>
      <div class="topbar">
        <div class="status-strip">
          <div class="chip"><span class="chip-label">Hives</span><span id="hives-summary" class="chip-value">--</span></div>
          <div class="chip"><span class="chip-label">Nodes</span><span id="nodes-summary" class="chip-value">--</span></div>
          <div class="chip"><span class="chip-label">Updated</span><span id="updated-at" class="chip-value">--</span></div>
        </div>
      </div>
    </div>
    <div class="workspace">
      <aside class="sidebar">
        <div class="sidebar-head">
          <div class="sidebar-title">Chat History</div>
          <button id="clear-history" class="mini-pill action" type="button">Clear</button>
        </div>
        <div class="new-chat-row">
          <button id="new-chat-operator" class="new-chat operator">Operator</button>
          <button id="new-chat-impersonation" class="new-chat debug">Impersonate</button>
        </div>
        <input id="history-search" class="history-search" type="search" placeholder="Search chats" autocomplete="off" />
        <div id="history-stats" class="history-stats">Loading chats...</div>
        <div id="history-list" class="history-list"></div>
        <div class="meta-grid">
          <div class="meta-inline-note">Stored locally on motherbee. Use impersonation only for debug and simulation flows.</div>
          <div><strong>Node</strong>: {node}</div>
          <div><strong>Modes</strong>: operator, impersonation, SCMD</div>
        </div>
      </aside>
      <div class="shell">
        <div class="shell-head">
          <div class="shell-title-row">
            <h1 class="shell-title">archi</h1>
            <div class="shell-title-meta">&middot; system architect interface</div>
          </div>
        </div>
        <div id="messages" class="messages"></div>
        <div class="composer">
          <div class="composer-box">
            <input id="attachment-input" type="file" multiple hidden />
            <div id="attachment-draft" class="attachment-draft"></div>
            <div class="composer-input-shell">
              <textarea id="input" placeholder="Message or SCMD. Try: SCMD: help"></textarea>
              <div class="composer-actions">
                <button id="attach" type="button">Attach</button>
                <button id="send">Send</button>
              </div>
            </div>
            <div id="composer-hint" class="hint">Enter to send. Shift+Enter for newline. Sessions on the left are local and reload-safe.</div>
          </div>
        </div>
      </div>
    </div>
  </div>
  <div id="impersonation-modal" class="modal-backdrop" aria-hidden="true">
    <div class="modal-card" role="dialog" aria-modal="true" aria-labelledby="impersonation-modal-title">
      <div class="modal-head">
        <div>
          <div class="modal-kicker">Debug Session</div>
          <h2 id="impersonation-modal-title" class="modal-title">Create impersonation chat</h2>
          <p class="modal-copy">Choose an existing ICH from identity SHM and, if needed, the ILK bound to that channel so you can simulate a real ingress path without connecting external IO.</p>
        </div>
        <button id="impersonation-close" class="modal-close" type="button" aria-label="Close impersonation dialog">&times;</button>
      </div>
      <form id="impersonation-form" class="modal-form">
        <div class="field-grid">
          <div class="field span-2">
            <label for="impersonation-title">Title</label>
            <input id="impersonation-title" type="text" placeholder="Impersonation chat" autocomplete="off" />
          </div>
          <div class="field span-2">
            <label for="impersonation-ich">Existing ICH</label>
            <select id="impersonation-ich">
              <option value="">Loading identity channels...</option>
            </select>
          </div>
          <div class="field span-2">
            <label for="impersonation-ilk">Effective ILK</label>
            <select id="impersonation-ilk">
              <option value="">Choose an ICH first</option>
            </select>
          </div>
          <div class="field span-2">
            <label for="impersonation-thread-id">Thread Id</label>
            <input id="impersonation-thread-id" type="text" placeholder="thread:demo-123" autocomplete="off" />
          </div>
        </div>
        <div class="field-help">The modal only offers active channels known by identity SHM. Thread is optional simulation context layered on top of the selected ingress path.</div>
        <div id="impersonation-error" class="modal-error"></div>
        <div class="modal-actions">
          <button id="impersonation-cancel" class="secondary-button" type="button">Cancel</button>
          <button id="impersonation-submit" type="submit">Create chat</button>
        </div>
      </form>
    </div>
  </div>
  <div id="confirm-modal" class="modal-backdrop" aria-hidden="true">
    <div class="modal-card" role="dialog" aria-modal="true" aria-labelledby="confirm-modal-title">
      <div class="modal-head">
        <div>
          <div id="confirm-modal-kicker" class="modal-kicker">Confirm Action</div>
          <h2 id="confirm-modal-title" class="modal-title">Please confirm</h2>
          <p id="confirm-modal-copy" class="confirm-copy">Are you sure?</p>
        </div>
        <button id="confirm-close" class="modal-close" type="button" aria-label="Close confirmation dialog">&times;</button>
      </div>
      <div class="modal-actions">
        <button id="confirm-cancel" class="secondary-button" type="button">Cancel</button>
        <button id="confirm-accept" type="button">Confirm</button>
      </div>
    </div>
  </div>
  <script>
    const rawPath = window.location.pathname.replace(/\/+$/, "");
    const base = rawPath === "" ? "" : rawPath;
    const statusUrl = (base || "") + "/api/status";
    const chatUrl = (base || "") + "/api/chat";
    const attachmentsUrl = (base || "") + "/api/attachments";
    const sessionsUrl = (base || "") + "/api/sessions";
    const identityIchOptionsUrl = (base || "") + "/api/identity/ich-options";
    const currentSessionStorageKey = "sy.architect.currentSession.{hive}";
    const statusRefreshActiveMs = 15000;
    const statusRefreshHiddenMs = 60000;
    const sessionRefreshActiveMs = 2000;
    const sessionRefreshHiddenMs = 8000;
    const messages = document.getElementById("messages");
    const input = document.getElementById("input");
    const attach = document.getElementById("attach");
    const attachmentInput = document.getElementById("attachment-input");
    const attachmentDraft = document.getElementById("attachment-draft");
    const send = document.getElementById("send");
    const newChatOperator = document.getElementById("new-chat-operator");
    const newChatImpersonation = document.getElementById("new-chat-impersonation");
    const clearHistory = document.getElementById("clear-history");
    const historySearch = document.getElementById("history-search");
    const historyStats = document.getElementById("history-stats");
    const historyList = document.getElementById("history-list");
    const composerHint = document.getElementById("composer-hint");
    const impersonationModal = document.getElementById("impersonation-modal");
    const impersonationForm = document.getElementById("impersonation-form");
    const impersonationClose = document.getElementById("impersonation-close");
    const impersonationCancel = document.getElementById("impersonation-cancel");
    const impersonationTitle = document.getElementById("impersonation-title");
    const impersonationIch = document.getElementById("impersonation-ich");
    const impersonationIlk = document.getElementById("impersonation-ilk");
    const impersonationThreadId = document.getElementById("impersonation-thread-id");
    const impersonationError = document.getElementById("impersonation-error");
    const confirmModal = document.getElementById("confirm-modal");
    const confirmModalKicker = document.getElementById("confirm-modal-kicker");
    const confirmModalTitle = document.getElementById("confirm-modal-title");
    const confirmModalCopy = document.getElementById("confirm-modal-copy");
    const confirmClose = document.getElementById("confirm-close");
    const confirmCancel = document.getElementById("confirm-cancel");
    const confirmAccept = document.getElementById("confirm-accept");
    let currentSessionId = null;
    let currentSessionMode = "operator";
    let currentSessionRevision = "";
    let sessionsCache = [];
    let pendingIndicator = null;
    let impersonationOptionsCache = [];
    let statusRefreshTimer = null;
    let statusRefreshInFlight = false;
    let sessionRefreshTimer = null;
    let sessionRefreshInFlight = false;
    let confirmResolver = null;
    let pendingAttachments = [];
    function formatBytes(value) {{
      const size = Number(value || 0);
      if (!Number.isFinite(size) || size <= 0) return "0 B";
      if (size < 1024) return String(size) + " B";
      if (size < 1024 * 1024) return (size / 1024).toFixed(1).replace(/\.0$/, "") + " KB";
      return (size / (1024 * 1024)).toFixed(1).replace(/\.0$/, "") + " MB";
    }}
    function attachmentDisplayName(entry) {{
      if (!entry) return "attachment";
      return entry.filename || entry.name || "attachment";
    }}
    function attachmentDisplayMime(entry) {{
      if (!entry) return "application/octet-stream";
      return entry.mime || entry.type || "application/octet-stream";
    }}
    function clearPendingAttachments() {{
      pendingAttachments = [];
      if (attachmentInput) {{
        attachmentInput.value = "";
      }}
      renderAttachmentDraft();
    }}
    function removePendingAttachment(index) {{
      pendingAttachments = pendingAttachments.filter((_, idx) => idx !== index);
      renderAttachmentDraft();
    }}
    function renderAttachmentDraft() {{
      attachmentDraft.innerHTML = "";
      pendingAttachments.forEach((entry, index) => {{
        const chip = document.createElement("div");
        const name = document.createElement("div");
        const meta = document.createElement("div");
        const remove = document.createElement("button");
        chip.className = "attachment-chip";
        name.className = "attachment-chip-name";
        meta.className = "attachment-chip-meta";
        remove.className = "attachment-chip-remove";
        remove.type = "button";
        remove.setAttribute("aria-label", "Remove attachment");
        remove.innerHTML = "&times;";
        name.textContent = attachmentDisplayName(entry.file);
        meta.textContent = attachmentDisplayMime(entry.file) + " \u00B7 " + formatBytes(entry.file && entry.file.size);
        remove.addEventListener("click", () => removePendingAttachment(index));
        chip.appendChild(name);
        chip.appendChild(meta);
        chip.appendChild(remove);
        attachmentDraft.appendChild(chip);
      }});
    }}
    async function uploadPendingAttachments() {{
      if (!pendingAttachments.length) {{
        return [];
      }}
      const form = new FormData();
      pendingAttachments.forEach((entry) => {{
        if (entry && entry.file) {{
          form.append("file", entry.file, entry.file.name || "attachment");
        }}
      }});
      if (currentSessionId) {{
        form.append("session_id", currentSessionId);
      }}
      const res = await fetch(attachmentsUrl, {{
        method: "POST",
        body: form
      }});
      const data = await res.json().catch(() => ({{ status: "error", error: "attachment upload failed" }}));
      if (!res.ok || !data || data.status !== "ok") {{
        const error = data && (data.error || data.error_detail) ? (data.error || data.error_detail) : "attachment upload failed";
        throw new Error(error);
      }}
      return Array.isArray(data.attachments) ? data.attachments.map((item) => ({{
        attachment_id: item.attachment_id,
        blob_ref: item.blob_ref
      }})) : [];
    }}
    function buildMessageBody(text, attachments) {{
      const hasText = typeof text === "string" && text.length > 0;
      const hasAttachments = Array.isArray(attachments) && attachments.length > 0;
      if (!hasAttachments) {{
        return hasText ? text : "";
      }}
      const shell = document.createElement("div");
      if (hasText) {{
        const textNode = document.createElement("div");
        textNode.textContent = text;
        shell.appendChild(textNode);
      }}
      const list = document.createElement("div");
      list.className = "message-attachments";
      attachments.forEach((entry) => {{
        const card = document.createElement("div");
        const name = document.createElement("div");
        const meta = document.createElement("div");
        card.className = "message-attachment";
        name.className = "message-attachment-name";
        meta.className = "message-attachment-meta";
        name.textContent = attachmentDisplayName(entry);
        meta.textContent = attachmentDisplayMime(entry) + " \u00B7 " + formatBytes(entry && entry.size);
        card.appendChild(name);
        card.appendChild(meta);
        list.appendChild(card);
      }});
      shell.appendChild(list);
      return shell;
    }}
    function describeIchOption(option) {{
      if (!option) return "";
      const primary = option.is_primary ? " \u00B7 primary" : "";
      return option.channel_type + " \u00B7 " + option.address + primary + " \u00B7 " + option.ich_id;
    }}
    function selectedImpersonationOption() {{
      const idx = Number(impersonationIch.value);
      if (!Number.isInteger(idx) || idx < 0 || idx >= impersonationOptionsCache.length) {{
        return null;
      }}
      return impersonationOptionsCache[idx] || null;
    }}
    function syncImpersonationIlkOptions() {{
      const option = selectedImpersonationOption();
      impersonationIlk.innerHTML = "";
      if (!option) {{
        impersonationIlk.innerHTML = '<option value="">Choose an ICH first</option>';
        impersonationIlk.disabled = true;
        return;
      }}
      if (!Array.isArray(option.ilks) || option.ilks.length === 0) {{
        impersonationIlk.innerHTML = '<option value="">No ILK bound to this ICH</option>';
        impersonationIlk.disabled = true;
        return;
      }}
      option.ilks.forEach((ilk) => {{
        const item = document.createElement("option");
        item.value = ilk.ilk_id;
        item.textContent = (ilk.display_name || ilk.ilk_id) + " \u00B7 " + ilk.registration_status;
        impersonationIlk.appendChild(item);
      }});
      impersonationIlk.disabled = option.ilks.length <= 1;
      impersonationIlk.value = option.ilks[0].ilk_id;
    }}
    function renderImpersonationIchOptions(options) {{
      impersonationOptionsCache = Array.isArray(options) ? options : [];
      impersonationIch.innerHTML = "";
      if (!impersonationOptionsCache.length) {{
        impersonationIch.innerHTML = '<option value="">No active ICH entries found</option>';
        impersonationIch.disabled = true;
        syncImpersonationIlkOptions();
        return;
      }}
      impersonationIch.disabled = false;
      impersonationOptionsCache.forEach((option, index) => {{
        const item = document.createElement("option");
        item.value = String(index);
        item.textContent = describeIchOption(option);
        impersonationIch.appendChild(item);
      }});
      impersonationIch.value = "0";
      syncImpersonationIlkOptions();
    }}
    async function fetchImpersonationOptions() {{
      const res = await fetch(identityIchOptionsUrl);
      if (!res.ok) {{
        const error = await res.json().catch(() => ({{ error: "identity options request failed" }}));
        throw new Error(error && error.error ? error.error : "identity options request failed");
      }}
      const data = await res.json();
      renderImpersonationIchOptions(Array.isArray(data.options) ? data.options : []);
    }}
    function closeImpersonationModal() {{
      impersonationModal.classList.remove("open");
      impersonationModal.setAttribute("aria-hidden", "true");
      impersonationForm.reset();
      impersonationTitle.value = "Impersonation chat";
      impersonationError.textContent = "";
      impersonationIch.disabled = false;
      impersonationIlk.disabled = true;
      impersonationIch.innerHTML = '<option value="">Loading identity channels...</option>';
      impersonationIlk.innerHTML = '<option value="">Choose an ICH first</option>';
    }}
    async function openImpersonationModal() {{
      impersonationForm.reset();
      impersonationTitle.value = "Impersonation chat";
      impersonationError.textContent = "";
      impersonationIch.disabled = true;
      impersonationIlk.disabled = true;
      impersonationIch.innerHTML = '<option value="">Loading identity channels...</option>';
      impersonationIlk.innerHTML = '<option value="">Choose an ICH first</option>';
      impersonationModal.classList.add("open");
      impersonationModal.setAttribute("aria-hidden", "false");
      try {{
        await fetchImpersonationOptions();
      }} catch (err) {{
        impersonationError.textContent = "Failed to load identity options: " + err;
      }}
      window.setTimeout(() => impersonationIch.focus(), 0);
    }}
    function closeConfirmModal(confirmed = false) {{
      confirmModal.classList.remove("open");
      confirmModal.setAttribute("aria-hidden", "true");
      confirmAccept.classList.remove("danger-button");
      const resolver = confirmResolver;
      confirmResolver = null;
      if (resolver) {{
        resolver(confirmed);
      }}
    }}
    function openConfirmModal(options = {{}}) {{
      if (confirmResolver) {{
        confirmResolver(false);
        confirmResolver = null;
      }}
      const title = options.title || "Please confirm";
      const copy = options.message || "Are you sure?";
      const kicker = options.kicker || "Confirm Action";
      const confirmLabel = options.confirmLabel || "Confirm";
      const cancelLabel = options.cancelLabel || "Cancel";
      const tone = options.tone || "default";
      confirmModalTitle.textContent = title;
      confirmModalCopy.textContent = copy;
      confirmModalKicker.textContent = kicker;
      confirmAccept.textContent = confirmLabel;
      confirmCancel.textContent = cancelLabel;
      confirmAccept.classList.toggle("danger-button", tone === "danger");
      confirmModal.classList.add("open");
      confirmModal.setAttribute("aria-hidden", "false");
      return new Promise((resolve) => {{
        confirmResolver = resolve;
        window.setTimeout(() => confirmAccept.focus(), 0);
      }});
    }}
    function collectImpersonationPayload() {{
      const option = selectedImpersonationOption();
      const title = impersonationTitle.value.trim();
      const threadId = impersonationThreadId.value.trim();
      const effectiveIlk = impersonationIlk.value.trim();
      if (!option) {{
        throw new Error("Choose an existing ICH before creating an impersonation chat.");
      }}
      if (!effectiveIlk) {{
        throw new Error("Choose a valid ILK for the selected ICH.");
      }}
      return {{
        title: title || "Impersonation chat",
        chat_mode: "impersonation",
        effective_ich_id: option.ich_id,
        effective_ilk: effectiveIlk || null,
        impersonation_target: null,
        thread_id: threadId || null,
        source_channel_kind: option.channel_type || null,
        debug_enabled: true
      }};
    }}
    function appendMessage(kind, labelText, bodyContent) {{
      const div = document.createElement("div");
      const label = document.createElement("div");
      const body = document.createElement("div");
      div.className = "msg " + kind;
      label.className = "msg-label";
      label.textContent = labelText;
      body.className = "msg-body";
      if (typeof bodyContent === "string") {{
        body.textContent = bodyContent;
      }} else if (bodyContent) {{
        body.appendChild(bodyContent);
      }}
      div.appendChild(label);
      div.appendChild(body);
      messages.appendChild(div);
      messages.scrollTop = messages.scrollHeight;
      return div;
    }}
    function formatChip(elementId, text, className) {{
      const element = document.getElementById(elementId);
      if (!element) return;
      element.textContent = text;
      element.classList.remove("ok", "warn");
      if (className) {{
        element.classList.add(className);
      }}
    }}

    function statusClass(raw) {{
      const value = String(raw || "").toLowerCase();
      if (["alive", "running", "healthy", "ok", "active", "connected"].includes(value)) return "ok";
      if (["stale", "degraded", "failed", "error", "missing", "offline", "deleted", "not_found"].includes(value)) return "warn";
      return "";
    }}

    function formatTimestamp(value) {{
      if (!value) return "--";
      const date = new Date(Number(value));
      if (Number.isNaN(date.getTime())) return "--";
      return date.toLocaleTimeString([], {{ hour: "2-digit", minute: "2-digit", second: "2-digit" }});
    }}

    function addMessage(kind, text, labelOverride) {{
      const labels = {{ user: "Operator", architect: "archi", system: "System" }};
      appendMessage(kind, labelOverride || labels[kind] || "Message", text);
    }}
    function showPendingIndicator(label = "archi", text = "Thinking") {{
      hidePendingIndicator();
      const body = document.createElement("div");
      const message = document.createElement("div");
      const dots = document.createElement("div");
      body.className = "pending-body";
      message.className = "pending-text";
      message.textContent = text;
      dots.className = "thinking-dots";
      dots.innerHTML = "<span></span><span></span><span></span>";
      body.appendChild(message);
      body.appendChild(dots);
      pendingIndicator = appendMessage("architect", label, body);
      pendingIndicator.classList.add("pending");
    }}
    function hidePendingIndicator() {{
      if (pendingIndicator && pendingIndicator.parentNode) {{
        pendingIndicator.parentNode.removeChild(pendingIndicator);
      }}
      pendingIndicator = null;
    }}
    function createPre(value) {{
      const pre = document.createElement("pre");
      pre.className = "result-pre";
      pre.textContent = typeof value === "string" ? value : JSON.stringify(value, null, 2);
      return pre;
    }}
    function shouldExpandValue(value, maxLen = 180) {{
      if (value === null || value === undefined) return false;
      if (typeof value !== "string") return true;
      const text = value.trim();
      return text.length > maxLen || text.includes("\n");
    }}
    function createExpandableValue(summaryLabel, value, maxLen = 180) {{
      const shell = document.createElement("div");
      const preview = document.createElement("div");
      shell.className = "result-section";
      preview.className = "result-section-preview";
      preview.textContent = compactValue(value, maxLen);
      shell.appendChild(preview);
      if (shouldExpandValue(value, maxLen)) {{
        const details = document.createElement("details");
        const summary = document.createElement("summary");
        summary.textContent = summaryLabel;
        details.appendChild(summary);
        details.appendChild(createPre(value));
        shell.appendChild(details);
      }}
      return shell;
    }}
    function createResultSection(title, value) {{
      const section = document.createElement("div");
      const heading = document.createElement("div");
      section.className = "result-section";
      heading.className = "result-section-title";
      heading.textContent = title;
      section.appendChild(heading);
      const block = createExpandableValue("Show full " + title, value, 240);
      Array.from(block.childNodes).forEach((child) => section.appendChild(child));
      return section;
    }}
    function compactValue(value, maxLen = 180) {{
      let text = "";
      if (typeof value === "string") {{
        text = value;
      }} else {{
        try {{
          text = JSON.stringify(value);
        }} catch (_err) {{
          text = String(value);
        }}
      }}
      text = String(text || "").replace(/\s+/g, " ").trim();
      if (!text) return "none";
      if (text.length <= maxLen) return text;
      return text.slice(0, Math.max(0, maxLen - 3)).trimEnd() + "...";
    }}
    function toolSortWeight(tool, index) {{
      const isError = !!(tool && tool.is_error);
      const name = tool && tool.name ? String(tool.name) : "";
      const isWrite = name === "fluxbee_system_write";
      return {{
        errorRank: isError ? 1 : 0,
        writeRank: isWrite ? 0 : 1,
        indexRank: -index,
      }};
    }}
    function createToolSummarySection(toolResults) {{
      if (!Array.isArray(toolResults) || !toolResults.length) return null;
      const shell = document.createElement("details");
      const head = document.createElement("div");
      const meta = document.createElement("div");
      const list = document.createElement("div");
      const errorCount = toolResults.filter((tool) => tool && tool.is_error).length;
      const okCount = toolResults.length - errorCount;
      const orderedTools = toolResults
        .map((tool, index) => ({{
          tool,
          index,
          sort: toolSortWeight(tool, index),
        }}))
        .sort((a, b) => {{
          if (a.sort.errorRank !== b.sort.errorRank) {{
            return a.sort.errorRank - b.sort.errorRank;
          }}
          if (a.sort.writeRank !== b.sort.writeRank) {{
            return a.sort.writeRank - b.sort.writeRank;
          }}
          return a.sort.indexRank - b.sort.indexRank;
        }})
        .map((entry) => entry.tool);
      shell.className = "tool-summary-toggle";
      head.className = "tool-summary-head";
      meta.className = "tool-summary-meta";
      head.textContent = toolResults.length === 1 ? "Tool used" : "Tools used";
      if (errorCount && okCount) {{
        meta.textContent = okCount + " ok, " + errorCount + " error";
      }} else if (errorCount) {{
        meta.textContent = errorCount === 1 ? "1 tool error" : errorCount + " tool errors";
      }} else {{
        meta.textContent = toolResults.length === 1 ? "1 successful tool" : toolResults.length + " successful tools";
      }}
      list.className = "tool-list";
      const summaryToggle = document.createElement("summary");
      const body = document.createElement("div");
      body.className = "tool-summary";
      summaryToggle.appendChild(head);
      summaryToggle.appendChild(meta);
      shell.appendChild(summaryToggle);
      shell.appendChild(body);
      body.appendChild(list);
      orderedTools.forEach((tool) => {{
        const card = document.createElement("div");
        const cardHead = document.createElement("div");
        const name = document.createElement("div");
        const badge = document.createElement("div");
        const summary = document.createElement("div");
        const inputRow = document.createElement("div");
        const outputRow = document.createElement("div");
        const details = document.createElement("details");
        const detailsSummary = document.createElement("summary");
        card.className = "tool-card";
        cardHead.className = "tool-card-head";
        name.className = "tool-card-name";
        badge.className = "tool-card-badge";
        summary.className = "tool-card-summary";
        inputRow.className = "tool-card-row";
        outputRow.className = "tool-card-row";
        name.textContent = tool && tool.name ? tool.name : "tool";
        badge.textContent = tool && tool.is_error ? "error" : "ok";
        if (tool && tool.is_error) {{
          badge.classList.add("error");
        }}
        summary.textContent = tool && tool.summary ? tool.summary : compactValue(tool && tool.output !== undefined ? tool.output : null, 220);
        inputRow.innerHTML = "<strong>Input</strong>";
        outputRow.innerHTML = "<strong>Output</strong>";
        cardHead.appendChild(name);
        cardHead.appendChild(badge);
        card.appendChild(cardHead);
        card.appendChild(summary);
        const inputBlock = createExpandableValue("Show full input", tool && tool.arguments !== undefined ? tool.arguments : null, 180);
        const outputBlock = createExpandableValue("Show full output", tool && tool.output !== undefined ? tool.output : null, 180);
        Array.from(inputBlock.childNodes).forEach((child) => inputRow.appendChild(child));
        Array.from(outputBlock.childNodes).forEach((child) => outputRow.appendChild(child));
        card.appendChild(inputRow);
        card.appendChild(outputRow);
        detailsSummary.textContent = "Show full tool result";
        details.appendChild(detailsSummary);
        details.appendChild(createPre(tool || null));
        card.appendChild(details);
        list.appendChild(card);
      }});
      return shell;
    }}
    function renderExecutorResult(data) {{
      const output = data && data.output ? data.output : {{}};
      const payload = output.payload || {{}};
      const summary = payload.summary || {{}};
      const events = Array.isArray(payload.events) ? payload.events : [];
      const executionId = payload.execution_id || output.execution_id || "";
      const overallStatus = summary.status || (data.status === "ok" ? "done" : "error");
      const isError = data.status !== "ok" || overallStatus === "failed" || overallStatus === "stopped";

      const shell = document.createElement("div");
      shell.className = "exec-shell";

      // header
      const head = document.createElement("div");
      head.className = "exec-head";
      const titleBlock = document.createElement("div");
      const titleEl = document.createElement("div");
      titleEl.className = "exec-title";
      titleEl.textContent = "executor plan";
      const subtitleEl = document.createElement("div");
      subtitleEl.className = "exec-subtitle";
      const completedSteps = summary.completed_steps != null ? summary.completed_steps : null;
      const totalSteps = summary.total_steps != null ? summary.total_steps : null;
      const stepsText = (completedSteps != null && totalSteps != null)
        ? completedSteps + " / " + totalSteps + " steps"
        : (executionId ? executionId.slice(0, 8) : "");
      const finalMessage = summary.final_message || (isError ? output.error || "" : "");
      const failedStep = summary.failed_step_id
        ? " · failed at " + summary.failed_step_id + (summary.failed_step_action ? " (" + summary.failed_step_action + ")" : "")
        : "";
      subtitleEl.textContent = stepsText + (finalMessage ? " · " + finalMessage : "") + failedStep;
      titleBlock.appendChild(titleEl);
      titleBlock.appendChild(subtitleEl);
      const badge = document.createElement("div");
      badge.className = "result-status " + (isError ? "warn" : "ok");
      badge.textContent = overallStatus;
      head.appendChild(titleBlock);
      head.appendChild(badge);
      shell.appendChild(head);

      // if validation/pre-execution error
      if (data.status !== "ok" && output.error) {{
        const errBlock = document.createElement("div");
        errBlock.className = "exec-error-block";
        const phase = output.phase ? "[" + output.phase + "] " : "";
        errBlock.textContent = phase + output.error;
        shell.appendChild(errBlock);
        appendMessage("architect", "archi", shell);
        return;
      }}

      // group events by step_id — keep only the last terminal event per step
      const stepMap = new Map();
      const stepOrder = [];
      for (const ev of events) {{
        const sid = ev.step_id || "_";
        if (!stepMap.has(sid)) {{
          stepMap.set(sid, []);
          stepOrder.push(sid);
        }}
        stepMap.get(sid).push(ev);
      }}

      if (stepOrder.length > 0) {{
        const stepsEl = document.createElement("div");
        stepsEl.className = "exec-steps";

        for (const sid of stepOrder) {{
          const evs = stepMap.get(sid);
          // pick terminal event (done/failed/stopped), fall back to last
          const terminal = evs.slice().reverse().find((e) =>
            ["done", "failed", "stopped"].includes(e.status)
          ) || evs[evs.length - 1];
          // running event has the final args
          const runningEv = evs.find((e) => e.status === "running");
          const hasHelp = evs.some((e) => e.status === "help_lookup");

          const stepEl = document.createElement("div");
          stepEl.className = "exec-step";

          // step head (always visible, click to toggle body)
          const stepHead = document.createElement("div");
          stepHead.className = "exec-step-head";
          const icon = document.createElement("div");
          icon.className = "exec-step-icon";
          const termStatus = terminal ? terminal.status : "unknown";
          icon.textContent = termStatus === "done" ? "✓" : termStatus === "failed" ? "✗" : "·";
          const nameEl = document.createElement("div");
          nameEl.className = "exec-step-name";
          nameEl.textContent = (terminal && terminal.step_action) || sid;
          const idEl = document.createElement("div");
          idEl.className = "exec-step-id";
          idEl.textContent = sid;
          const stepBadge = document.createElement("div");
          stepBadge.className = "exec-step-badge " + (termStatus === "done" ? "ok" : termStatus === "failed" || termStatus === "stopped" ? "warn" : "muted");
          stepBadge.textContent = termStatus;
          stepHead.appendChild(icon);
          stepHead.appendChild(nameEl);
          stepHead.appendChild(idEl);
          if (hasHelp) {{
            const helpChip = document.createElement("div");
            helpChip.className = "exec-step-badge muted";
            helpChip.textContent = "help";
            stepHead.appendChild(helpChip);
          }}
          stepHead.appendChild(stepBadge);

          // step body (collapsed by default unless error)
          const stepBody = document.createElement("div");
          stepBody.className = "exec-step-body";
          let bodyVisible = termStatus === "failed" || termStatus === "stopped";

          // args
          const argsVal = (runningEv && runningEv.tool_args_preview) || (terminal && terminal.tool_args_preview);
          if (argsVal && typeof argsVal === "object" && Object.keys(argsVal).length > 0) {{
            const argsEl = document.createElement("div");
            argsEl.className = "exec-step-args";
            argsEl.textContent = Object.entries(argsVal)
              .filter(([, v]) => v != null)
              .map(([k, v]) => k + ": " + (typeof v === "object" ? JSON.stringify(v) : v))
              .join("\n");
            stepBody.appendChild(argsEl);
          }}

          // error message
          if (terminal && terminal.error_message) {{
            const errEl = document.createElement("div");
            errEl.className = "exec-step-error";
            errEl.textContent = terminal.error_message;
            stepBody.appendChild(errEl);
          }}

          // result preview (expandable)
          const resultVal = terminal && terminal.result_preview;
          if (resultVal) {{
            const details = document.createElement("details");
            const sumEl = document.createElement("summary");
            sumEl.textContent = "Show result";
            const pre = document.createElement("pre");
            pre.className = "result-pre";
            pre.textContent = typeof resultVal === "string" ? resultVal : JSON.stringify(resultVal, null, 2);
            details.appendChild(sumEl);
            details.appendChild(pre);
            stepBody.appendChild(details);
          }}

          stepEl.appendChild(stepHead);
          if (stepBody.children.length > 0) {{
            if (!bodyVisible) {{
              stepBody.style.display = "none";
            }}
            stepHead.addEventListener("click", () => {{
              stepBody.style.display = stepBody.style.display === "none" ? "" : "none";
            }});
            stepEl.appendChild(stepBody);
          }}
          stepsEl.appendChild(stepEl);
        }}
        shell.appendChild(stepsEl);
      }}

      appendMessage("architect", "archi", shell);
    }}
    function renderCommandResult(kind, mode, data) {{
      const shell = document.createElement("div");
      const head = document.createElement("div");
      const title = document.createElement("div");
      const badge = document.createElement("div");
      const meta = document.createElement("div");
      shell.className = "result-shell";
      head.className = "result-head";
      title.className = "result-title";
      badge.className = "result-status";
      meta.className = "result-meta";

      const isSystemCommand = mode === "scmd" || mode === "acmd";
      const modeLabel = isSystemCommand ? "system command" : "response";
      const resultStatus = String(data.status || "unknown");
      title.textContent = modeLabel;
      badge.textContent = resultStatus;
      const badgeClass = statusClass(resultStatus);
      if (badgeClass) {{
        badge.classList.add(badgeClass);
      }}
      head.appendChild(title);
      head.appendChild(badge);
      shell.appendChild(head);

      if (isSystemCommand) {{
        const action = data.output && data.output.action ? data.output.action : "unknown";
        const traceId = data.output && data.output.trace_id ? data.output.trace_id : null;
        meta.textContent = "action: " + action + (traceId ? " | trace: " + traceId : "");
        shell.appendChild(meta);
        if (data.output && data.output.payload !== undefined) {{
          shell.appendChild(createResultSection("payload", data.output.payload));
        }}
        const commandError = data.output && (data.output.error_detail || data.output.error);
        if (commandError) {{
          shell.appendChild(createResultSection("error", commandError));
        }}
      }} else {{
        shell.appendChild(createResultSection("result", data.output));
      }}

      appendMessage(kind, kind === "architect" ? "archi" : "System", shell);
    }}
    function renderResponsePayload(kind, data) {{
      if (data && data.mode === "executor") {{
        renderExecutorResult(data);
        return;
      }}
      const output = data && data.output ? data.output : null;
      if (data && data.mode === "chat" && output && typeof output.message === "string" && output.message.trim()) {{
        const toolSummary = createToolSummarySection(output.tool_results);
        if (!toolSummary) {{
          addMessage(kind, output.message);
          return;
        }}
        const body = document.createElement("div");
        const message = document.createElement("div");
        body.className = "chat-response";
        message.textContent = output.message;
        body.appendChild(message);
        body.appendChild(toolSummary);
        appendMessage(kind, kind === "architect" ? "archi" : "System", body);
        return;
      }}
      renderCommandResult(kind, data.mode, data);
    }}
    function renderStoredMessage(message) {{
      const metadata = message && message.metadata ? message.metadata : {{}};
      if (metadata.kind === "response" && metadata.response) {{
        const role = message.role === "architect" ? "architect" : message.role === "system" ? "system" : "architect";
        renderResponsePayload(role, metadata.response);
        return;
      }}
      const role = message.role === "architect" ? "architect" : message.role === "system" ? "system" : "user";
      const label = metadata.kind === "router_message" && metadata.label ? String(metadata.label) : null;
      const attachments = Array.isArray(metadata.attachments) ? metadata.attachments : [];
      addMessage(role, buildMessageBody(message.content || "", attachments), label);
    }}
    function isDestructiveMessage(message) {{
      const trimmed = String(message || "").trim();
      if (!trimmed.startsWith("SCMD:")) return false;
      const command = trimmed.slice(5).trim().toUpperCase();
      return command.startsWith("CURL -X DELETE ");
    }}
    function seedWelcomeMessages() {{
      addMessage("system", "Fluxbee architect interface ready. System operations are available through SCMD.");
      addMessage("architect", "I am archi. Chat is live, and SCMD remains available for direct system operations.");
      addMessage("system", "Examples: SCMD: help | SCMD: curl -X GET /hives/{hive}/nodes");
    }}
    function resetChatViewport(preserveComposer = false) {{
      hidePendingIndicator();
      messages.innerHTML = "";
      if (!preserveComposer) {{
        input.value = "";
        clearPendingAttachments();
      }}
    }}
    function formatSessionMeta(session) {{
      if (!session) return "waiting for first message";
      if (!session.message_count) return "waiting for first message";
      const count = Number(session.message_count || 0);
      return count + " messages \u00B7 updated " + formatTimestamp(session.last_activity_at_ms);
    }}
    function formatSessionCount(session) {{
      if (!session || !session.message_count) return "waiting for first message";
      const count = Number(session.message_count || 0);
      return count === 1 ? "1 message" : count + " messages";
    }}
    function formatSessionUpdated(session) {{
      if (!session || !session.last_activity_at_ms) return "not updated yet";
      return "updated " + formatTimestamp(session.last_activity_at_ms);
    }}
    function compactHistoryToken(value) {{
      if (!value) return "";
      const text = String(value);
      if (text.length <= 20) return text;
      return text.slice(0, 8) + "..." + text.slice(-6);
    }}
    function appendHistoryBadge(container, label, value) {{
      if (!value) return;
      const badge = document.createElement("span");
      badge.className = "mode-badge context";
      badge.title = value;
      badge.textContent = label + " " + compactHistoryToken(value);
      container.appendChild(badge);
    }}
    function updateHistoryStats(visibleCount, totalCount, query) {{
      if (!historyStats) return;
      if (!totalCount) {{
        historyStats.textContent = "Local session history is empty.";
        return;
      }}
      if (query) {{
        historyStats.textContent = "Showing " + visibleCount + " of " + totalCount + " chats for \"" + query + "\".";
        return;
      }}
      historyStats.textContent = totalCount === 1 ? "1 local chat available." : totalCount + " local chats available.";
    }}
    function sessionModeLabel(session) {{
      if (!session || !session.chat_mode) return "operator";
      return session.chat_mode === "impersonation" ? "impersonation" : "operator";
    }}
    function historyMatchesSearch(session, query) {{
      if (!query) return true;
      const haystack = [
        session && session.title ? session.title : "",
        session && session.last_message_preview ? session.last_message_preview : "",
        session && session.agent ? session.agent : "",
        session && session.chat_mode ? session.chat_mode : "",
        session && session.effective_ich_id ? session.effective_ich_id : "",
        session && session.effective_ilk ? session.effective_ilk : "",
        session && session.impersonation_target ? session.impersonation_target : "",
        session && session.source_channel_kind ? session.source_channel_kind : "",
        session && session.thread_id ? session.thread_id : ""
      ].join(" ").toLowerCase();
      return haystack.includes(query);
    }}
    function sortHistorySessions(a, b) {{
      if ((a && a.session_id) === currentSessionId && (b && b.session_id) !== currentSessionId) return -1;
      if ((b && b.session_id) === currentSessionId && (a && a.session_id) !== currentSessionId) return 1;
      const aUpdated = Number((a && a.last_activity_at_ms) || 0);
      const bUpdated = Number((b && b.last_activity_at_ms) || 0);
      return bUpdated - aUpdated;
    }}
    function renderHistory() {{
      historyList.innerHTML = "";
      if (!sessionsCache.length) {{
        updateHistoryStats(0, 0, "");
        const empty = document.createElement("div");
        empty.className = "history-card placeholder";
        empty.innerHTML = '<div class="history-name">No chats yet</div><div class="history-meta">Create the first session to start working with archi.</div>';
        historyList.appendChild(empty);
        return;
      }}
      const query = (historySearch && historySearch.value ? historySearch.value : "").trim().toLowerCase();
      const visibleSessions = sessionsCache
        .filter((session) => historyMatchesSearch(session, query))
        .sort(sortHistorySessions);
      updateHistoryStats(visibleSessions.length, sessionsCache.length, query);
      if (!visibleSessions.length) {{
        const empty = document.createElement("div");
        empty.className = "history-card placeholder";
        empty.innerHTML = '<div class="history-name">No matches</div><div class="history-meta">Try a different title or keyword from a recent message.</div>';
        historyList.appendChild(empty);
        return;
      }}
      visibleSessions.forEach((session) => {{
        const card = document.createElement("div");
        const head = document.createElement("div");
        const name = document.createElement("div");
        const meta = document.createElement("div");
        const deleteButton = document.createElement("button");
        card.className = "history-card" + (session.session_id === currentSessionId ? " active" : "");
        head.className = "history-card-head";
        name.className = "history-name";
        meta.className = "history-meta";
        deleteButton.className = "history-delete";
        deleteButton.type = "button";
        deleteButton.innerHTML = "&times;";
        deleteButton.title = "Delete chat";
        name.textContent = session.title || "Untitled chat";
        if (session.session_id === currentSessionId) {{
          const openBadge = document.createElement("div");
          openBadge.className = "history-open-badge";
          openBadge.textContent = "Open";
          card.appendChild(openBadge);
        }}
        const countSpan = document.createElement("span");
        const sepSpan = document.createElement("span");
        const updatedSpan = document.createElement("span");
        countSpan.textContent = formatSessionCount(session);
        sepSpan.className = "history-meta-sep";
        sepSpan.textContent = "\u2022";
        updatedSpan.className = "history-meta-updated";
        updatedSpan.textContent = formatSessionUpdated(session);
        meta.appendChild(countSpan);
        meta.appendChild(sepSpan);
        meta.appendChild(updatedSpan);
        deleteButton.addEventListener("click", (event) => {{
          event.stopPropagation();
          deleteSession(session.session_id).catch((err) => {{
            addMessage("system", "Delete chat failed: " + err);
          }});
        }});
        head.appendChild(name);
        head.appendChild(deleteButton);
        card.appendChild(head);
        card.appendChild(meta);
        const modeRow = document.createElement("div");
        modeRow.className = "history-mode";
        const modeBadge = document.createElement("span");
        modeBadge.className = "mode-badge " + sessionModeLabel(session);
        modeBadge.textContent = sessionModeLabel(session);
        modeRow.appendChild(modeBadge);
        appendHistoryBadge(modeRow, "ICH", session.effective_ich_id);
        appendHistoryBadge(modeRow, "ILK", session.effective_ilk);
        appendHistoryBadge(modeRow, "Thread", session.thread_id);
        card.appendChild(modeRow);
        if (session.last_message_preview) {{
          const preview = document.createElement("div");
          preview.className = "history-preview";
          preview.textContent = session.last_message_preview;
          card.appendChild(preview);
        }}
        card.addEventListener("click", () => {{
          if (session.session_id !== currentSessionId) {{
            loadSession(session.session_id);
          }}
        }});
        historyList.appendChild(card);
      }});
    }}
    async function refreshSessionList(preferredSessionId) {{
      const res = await fetch(sessionsUrl);
      const data = await res.json();
      sessionsCache = Array.isArray(data.sessions) ? data.sessions : [];
      if (!currentSessionId && sessionsCache.length) {{
        currentSessionId = preferredSessionId || localStorage.getItem(currentSessionStorageKey) || sessionsCache[0].session_id;
      }}
      if (preferredSessionId) {{
        currentSessionId = preferredSessionId;
      }}
      renderHistory();
    }}
    async function deleteSession(sessionId) {{
      const confirmed = await openConfirmModal({{
        title: "Delete chat",
        message: "Delete this chat history?",
        kicker: "Chat History",
        confirmLabel: "Delete",
        tone: "danger"
      }});
      if (!confirmed) return;
      const res = await fetch(sessionsUrl + "/" + encodeURIComponent(sessionId), {{
        method: "DELETE"
      }});
      if (!res.ok) {{
        throw new Error("session delete failed");
      }}
      await refreshSessionList(currentSessionId === sessionId ? null : currentSessionId);
      if (!sessionsCache.length) {{
        await createSession({{}}, false);
        return;
      }}
      if (currentSessionId === sessionId) {{
        await loadSession(sessionsCache[0].session_id, null, false);
      }}
    }}
    async function clearAllHistory() {{
      const confirmed = await openConfirmModal({{
        title: "Delete all chats",
        message: "Delete all chat history?",
        kicker: "Chat History",
        confirmLabel: "Delete all",
        tone: "danger"
      }});
      if (!confirmed) return;
      const res = await fetch(sessionsUrl, {{
        method: "DELETE"
      }});
      if (!res.ok) {{
        throw new Error("history clear failed");
      }}
      currentSessionId = null;
      localStorage.removeItem(currentSessionStorageKey);
      await refreshSessionList(null);
      await createSession({{}}, false);
    }}
    async function createSession(payload = {{}}, showWelcome = false) {{
      const res = await fetch(sessionsUrl, {{
        method: "POST",
        headers: {{ "Content-Type": "application/json" }},
        body: JSON.stringify(payload)
      }});
      if (!res.ok) {{
        const error = await res.json().catch(() => ({{ error: "session create failed" }}));
        throw new Error(error && error.error ? error.error : "session create failed");
      }}
      const detail = await res.json();
      await refreshSessionList(detail.session && detail.session.session_id ? detail.session.session_id : null);
      await loadSession(detail.session.session_id, detail, showWelcome);
    }}
    function updateComposerHint(session) {{
      if (!session) {{
        composerHint.textContent = "Enter to send. Shift+Enter for newline. Sessions on the left are local and reload-safe.";
        return;
      }}
      const base = session.message_count
        ? "Enter to send. Shift+Enter for newline. This chat has " + session.message_count + " persisted messages."
        : "Enter to send. Shift+Enter for newline. This chat is empty and ready.";
      if (sessionModeLabel(session) === "impersonation") {{
        const channel = session.source_channel_kind ? " via " + session.source_channel_kind : "";
        const ich = session.effective_ich_id ? " (" + session.effective_ich_id + ")" : "";
        composerHint.textContent = base + " Running in impersonation/debug mode" + channel + ich + ".";
      }} else {{
        composerHint.textContent = base + " Running in operator mode.";
      }}
    }}
    function sessionRevision(detail) {{
      const session = detail && detail.session ? detail.session : null;
      const count = session && session.message_count ? Number(session.message_count) : 0;
      const updated = session && session.last_activity_at_ms ? Number(session.last_activity_at_ms) : 0;
      return String(count) + ":" + String(updated);
    }}
    function renderSession(detail, showWelcome = false, preserveComposer = false) {{
      resetChatViewport(preserveComposer);
      const session = detail && detail.session ? detail.session : null;
      const sessionId = session && session.session_id ? session.session_id : null;
      currentSessionId = sessionId;
      currentSessionMode = sessionModeLabel(session);
      currentSessionRevision = sessionRevision(detail);
      if (sessionId) {{
        localStorage.setItem(currentSessionStorageKey, sessionId);
      }}
      renderHistory();
      updateComposerHint(session);
      if (!detail || !Array.isArray(detail.messages) || detail.messages.length === 0) {{
        if (showWelcome) {{
          seedWelcomeMessages();
        }}
        return;
      }}
      detail.messages.forEach((message) => renderStoredMessage(message));
    }}
    async function loadSession(sessionId, existingDetail, showWelcome = false) {{
      currentSessionId = sessionId;
      renderHistory();
      localStorage.setItem(currentSessionStorageKey, sessionId);
      if (existingDetail) {{
        renderSession(existingDetail, showWelcome);
        return;
      }}
      const res = await fetch(sessionsUrl + "/" + encodeURIComponent(sessionId), {{ cache: "no-store" }});
      if (!res.ok) {{
        if (res.status === 404) {{
          localStorage.removeItem(currentSessionStorageKey);
        }}
        throw new Error("session load failed");
      }}
      const detail = await res.json();
      renderSession(detail, showWelcome);
    }}
    async function refreshCurrentSession(options = {{}}) {{
      const force = !!(options && options.force);
      if (!currentSessionId || sessionRefreshInFlight) {{
        return;
      }}
      sessionRefreshInFlight = true;
      try {{
        const res = await fetch(sessionsUrl + "/" + encodeURIComponent(currentSessionId), {{ cache: "no-store" }});
        if (!res.ok) {{
          return;
        }}
        const detail = await res.json();
        const nextRevision = sessionRevision(detail);
        if (force || nextRevision !== currentSessionRevision) {{
          renderSession(detail, false, true);
          await refreshSessionList(currentSessionId);
        }}
      }} catch (_err) {{
      }} finally {{
        sessionRefreshInFlight = false;
      }}
    }}
    async function refreshStatus(options = {{}}) {{
      const force = !!(options && options.force);
      if (statusRefreshInFlight && !force) {{
        return;
      }}
      statusRefreshInFlight = true;
      try {{
        const res = await fetch(statusUrl, {{ cache: "no-store" }});
        const data = await res.json();
        const alive = Number(data.hives_alive || 0);
        const totalHives = Number(data.total_hives || 0);
        const stale = Number(data.hives_stale || 0);
        const totalNodes = Number(data.total_nodes || 0);
        formatChip("hives-summary", totalHives ? (String(alive) + "/" + String(totalHives) + " alive") : "--", stale > 0 ? "warn" : "ok");
        formatChip("nodes-summary", totalNodes ? String(totalNodes) : "--", totalNodes > 0 ? "ok" : "");
        formatChip("updated-at", formatTimestamp(data.inventory_updated_at), data.admin_available ? "ok" : "warn");
      }} catch (_err) {{
        formatChip("updated-at", "offline", "warn");
      }} finally {{
        statusRefreshInFlight = false;
      }}
    }}
    function stopStatusRefreshLoop() {{
      if (statusRefreshTimer) {{
        window.clearTimeout(statusRefreshTimer);
        statusRefreshTimer = null;
      }}
    }}
    function stopSessionRefreshLoop() {{
      if (sessionRefreshTimer) {{
        window.clearTimeout(sessionRefreshTimer);
        sessionRefreshTimer = null;
      }}
    }}
    function scheduleStatusRefresh(delayMs) {{
      stopStatusRefreshLoop();
      statusRefreshTimer = window.setTimeout(async () => {{
        await refreshStatus();
        if (!document.hidden) {{
          scheduleStatusRefresh(statusRefreshActiveMs);
        }} else {{
          scheduleStatusRefresh(statusRefreshHiddenMs);
        }}
      }}, delayMs);
    }}
    function restartStatusRefreshLoop(immediate = false) {{
      const delay = immediate ? 0 : (document.hidden ? statusRefreshHiddenMs : statusRefreshActiveMs);
      scheduleStatusRefresh(delay);
    }}
    function scheduleSessionRefresh(delayMs) {{
      stopSessionRefreshLoop();
      sessionRefreshTimer = window.setTimeout(async () => {{
        await refreshCurrentSession();
        scheduleSessionRefresh(document.hidden ? sessionRefreshHiddenMs : sessionRefreshActiveMs);
      }}, delayMs);
    }}
    function restartSessionRefreshLoop(immediate = false) {{
      const delay = immediate ? 0 : (document.hidden ? sessionRefreshHiddenMs : sessionRefreshActiveMs);
      scheduleSessionRefresh(delay);
    }}
    async function submit() {{
      const message = input.value.trim();
      if (!message && !pendingAttachments.length) return;
      if (!currentSessionId) {{
        await createSession();
      }}
      if (pendingAttachments.length) {{
        const trimmed = String(message || "").trim();
        if (!trimmed) {{
          addMessage("system", "Attachments require a message.");
          return;
        }}
        if (currentSessionMode !== "operator") {{
          addMessage("system", "Attachments are only supported in operator chat mode.");
          return;
        }}
        if (trimmed.startsWith("SCMD:") || trimmed === "CONFIRM" || trimmed === "CANCEL") {{
          addMessage("system", "Attachments are not supported for SCMD, CONFIRM, or CANCEL.");
          return;
        }}
      }}
      if (isDestructiveMessage(message)) {{
        const confirmed = await openConfirmModal({{
          title: "Send destructive command",
          message: "This command looks destructive. Do you want to send it?",
          kicker: "Safety Check",
          confirmLabel: "Send command",
          tone: "danger"
        }});
        if (!confirmed) return;
      }}
      const draftFiles = pendingAttachments.map((entry) => entry.file).filter(Boolean);
      addMessage("user", buildMessageBody(message, draftFiles));
      const pendingLabel = message.trim().startsWith("SCMD:") || message.trim() === "CONFIRM"
        ? "System"
        : "archi";
      const pendingText = pendingLabel === "System" ? "Executing action" : "Thinking";
      showPendingIndicator(pendingLabel, pendingText);
      input.value = "";
      renderAttachmentDraft();
      send.disabled = true;
      attach.disabled = true;
      try {{
        const attachments = await uploadPendingAttachments();
        const res = await fetch(chatUrl, {{
          method: "POST",
          headers: {{ "Content-Type": "application/json" }},
          body: JSON.stringify({{ session_id: currentSessionId, message, attachments }})
        }});
        const data = await res.json();
        if (data.session_id) {{
          currentSessionId = data.session_id;
          localStorage.setItem(currentSessionStorageKey, currentSessionId);
        }}
        hidePendingIndicator();
        if (!(data && data.output && data.output.suppress_echo === true)) {{
          renderResponsePayload(data.status === "ok" ? "architect" : "system", data);
        }}
        clearPendingAttachments();
        await refreshSessionList(currentSessionId);
      }} catch (err) {{
        hidePendingIndicator();
        addMessage("system", "Request failed: " + err);
      }} finally {{
        hidePendingIndicator();
        send.disabled = false;
        attach.disabled = false;
        refreshStatus({{ force: true }});
      }}
    }}
    attach.addEventListener("click", () => {{
      attachmentInput.click();
    }});
    attachmentInput.addEventListener("change", (event) => {{
      const files = Array.from(event.target && event.target.files ? event.target.files : []);
      if (!files.length) return;
      pendingAttachments = pendingAttachments.concat(files.map((file) => ({{ file }})));
      attachmentInput.value = "";
      renderAttachmentDraft();
    }});
    send.addEventListener("click", submit);
    clearHistory.addEventListener("click", () => {{
      clearAllHistory().catch((err) => {{
        addMessage("system", "Clear history failed: " + err);
      }});
    }});
    historySearch.addEventListener("input", () => {{
      renderHistory();
    }});
    newChatOperator.addEventListener("click", () => {{
      createSession({{}}, false).catch((err) => {{
        addMessage("system", "Session creation failed: " + err);
      }});
    }});
    newChatImpersonation.addEventListener("click", () => {{
      openImpersonationModal().catch((err) => {{
        addMessage("system", "Failed to open impersonation modal: " + err);
      }});
    }});
    impersonationIch.addEventListener("change", () => {{
      syncImpersonationIlkOptions();
      impersonationError.textContent = "";
    }});
    impersonationClose.addEventListener("click", closeImpersonationModal);
    impersonationCancel.addEventListener("click", closeImpersonationModal);
    impersonationModal.addEventListener("click", (event) => {{
      if (event.target === impersonationModal) {{
        closeImpersonationModal();
      }}
    }});
    confirmClose.addEventListener("click", () => closeConfirmModal(false));
    confirmCancel.addEventListener("click", () => closeConfirmModal(false));
    confirmAccept.addEventListener("click", () => closeConfirmModal(true));
    confirmModal.addEventListener("click", (event) => {{
      if (event.target === confirmModal) {{
        closeConfirmModal(false);
      }}
    }});
    impersonationForm.addEventListener("submit", (event) => {{
      event.preventDefault();
      impersonationError.textContent = "";
      let payload = null;
      try {{
        payload = collectImpersonationPayload();
      }} catch (err) {{
        impersonationError.textContent = String(err);
        return;
      }}
      createSession(payload, false).then(() => {{
        closeImpersonationModal();
      }}).catch((err) => {{
        impersonationError.textContent = "Impersonation session creation failed: " + err;
      }});
    }});
    input.addEventListener("keydown", (event) => {{
      if (event.key === "Enter" && !event.shiftKey) {{
        event.preventDefault();
        submit();
      }}
    }});
    window.addEventListener("keydown", (event) => {{
      if (event.key === "Escape") {{
        if (confirmModal.classList.contains("open")) {{
          closeConfirmModal(false);
          return;
        }}
        if (impersonationModal.classList.contains("open")) {{
          closeImpersonationModal();
        }}
      }}
    }});
    document.addEventListener("visibilitychange", () => {{
      if (document.hidden) {{
        restartStatusRefreshLoop(false);
        restartSessionRefreshLoop(false);
      }} else {{
        refreshStatus({{ force: true }});
        refreshCurrentSession({{ force: true }});
        restartStatusRefreshLoop(false);
        restartSessionRefreshLoop(false);
      }}
    }});
    async function bootstrap() {{
      resetChatViewport();
      await refreshSessionList(localStorage.getItem(currentSessionStorageKey));
      if (!sessionsCache.length) {{
        await createSession({{}}, true);
      }} else {{
        const stored = currentSessionId || localStorage.getItem(currentSessionStorageKey);
        const preferred = sessionsCache.some((session) => session.session_id === stored)
          ? stored
          : sessionsCache[0].session_id;
        if (stored && stored !== preferred) {{
          localStorage.setItem(currentSessionStorageKey, preferred);
        }}
        try {{
          await loadSession(preferred, null, false);
        }} catch (_err) {{
          localStorage.removeItem(currentSessionStorageKey);
          const fallback = sessionsCache[0] && sessionsCache[0].session_id ? sessionsCache[0].session_id : null;
          if (fallback) {{
            await loadSession(fallback, null, false);
          }} else {{
            await createSession({{}}, true);
          }}
        }}
      }}
      await refreshStatus({{ force: true }});
      restartStatusRefreshLoop(false);
      restartSessionRefreshLoop(false);
    }}
    bootstrap().catch((err) => {{
      addMessage("system", "Bootstrap failed: " + err);
    }});
  </script>
        </body>
</html>"##,
        node = state.node_name,
        hive = state.hive_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_blob_ref(filename: &str, mime: &str, size: u64) -> BlobRef {
        BlobRef {
            ref_type: "blob_ref".to_string(),
            blob_name: format!("blob_{}_0123456789abcdef", filename.replace('.', "_")),
            size,
            mime: mime.to_string(),
            filename_original: filename.to_string(),
            spool_day: "2026-04-17".to_string(),
        }
    }

    fn parse(raw: &str) -> ParsedScmd {
        parse_scmd(raw).expect("scmd should parse")
    }

    #[test]
    fn translate_wf_rules_list_workflows_scmd() {
        let translated =
            translate_scmd("motherbee", parse("curl -X GET /hives/motherbee/wf-rules"))
                .expect("translation should succeed");

        assert_eq!(translated.action, "wf_rules_list_workflows");
        assert_eq!(translated.target_hive, "motherbee");
        assert_eq!(translated.params, json!({}));
    }

    #[test]
    fn translate_wf_rules_get_workflow_scmd() {
        let translated = translate_scmd(
            "motherbee",
            parse("curl -X GET /hives/motherbee/wf-rules?workflow_name=invoice"),
        )
        .expect("translation should succeed");

        assert_eq!(translated.action, "wf_rules_get_workflow");
        assert_eq!(translated.target_hive, "motherbee");
        assert_eq!(translated.params, json!({ "workflow_name": "invoice" }));
    }

    #[test]
    fn translate_wf_rules_get_status_scmd() {
        let translated = translate_scmd(
            "motherbee",
            parse("curl -X GET /hives/motherbee/wf-rules/status?workflow_name=invoice"),
        )
        .expect("translation should succeed");

        assert_eq!(translated.action, "wf_rules_get_status");
        assert_eq!(translated.target_hive, "motherbee");
        assert_eq!(translated.params, json!({ "workflow_name": "invoice" }));
    }

    #[test]
    fn translate_wf_rules_compile_apply_scmd() {
        let translated = translate_scmd(
            "motherbee",
            parse(
                r#"curl -X POST /hives/motherbee/wf-rules -d '{"workflow_name":"invoice","definition":{"wf_schema_version":"1"}}'"#,
            ),
        )
        .expect("translation should succeed");

        assert_eq!(translated.action, "wf_rules_compile_apply");
        assert_eq!(translated.target_hive, "motherbee");
        assert_eq!(
            translated.params,
            json!({
                "workflow_name": "invoice",
                "definition": { "wf_schema_version": "1" }
            })
        );
    }

    #[test]
    fn render_user_message_with_attachment_summary_includes_attachment_lines() {
        let summary = render_user_message_with_attachment_summary(
            "revisa esto",
            &[ChatAttachmentRef {
                attachment_id: "att_1".to_string(),
                blob_ref: sample_blob_ref("invoice.pdf", "application/pdf", 182233),
            }],
        );

        assert!(summary.contains("revisa esto"));
        assert!(summary.contains("Attachment: invoice.pdf (application/pdf, 182233 bytes)"));
    }

    #[test]
    fn immediate_interaction_from_message_includes_persisted_attachment_summary() {
        let message = PersistedChatMessage {
            message_id: "msg_1".to_string(),
            session_id: "sess_1".to_string(),
            role: "user".to_string(),
            content: "revisa esto".to_string(),
            timestamp_ms: 0,
            mode: "text".to_string(),
            metadata: json!({
                "kind": "text_with_attachments",
                "attachments": [{
                    "attachment_id": "att_1",
                    "filename": "invoice.pdf",
                    "mime": "application/pdf",
                    "size": 182233,
                    "blob_ref": sample_blob_ref("invoice.pdf", "application/pdf", 182233),
                }]
            }),
        };

        let interaction =
            immediate_interaction_from_message(&message).expect("interaction should exist");
        assert!(interaction.content.contains("revisa esto"));
        assert!(interaction
            .content
            .contains("Attachment: invoice.pdf (application/pdf, 182233 bytes)"));
    }

    // ARCHI-ATTACH-11: mime validation
    #[test]
    fn normalize_attachment_mime_uses_provided_mime_over_extension() {
        let result = normalize_attachment_mime(Some("text/plain"), "file.pdf");
        assert_eq!(result.as_deref(), Some("text/plain"));
    }

    #[test]
    fn normalize_attachment_mime_infers_from_extension_when_none_provided() {
        assert_eq!(
            normalize_attachment_mime(None, "spec.md").as_deref(),
            Some("text/markdown")
        );
        assert_eq!(
            normalize_attachment_mime(None, "data.csv").as_deref(),
            Some("text/csv")
        );
        assert_eq!(
            normalize_attachment_mime(None, "report.pdf").as_deref(),
            Some("application/pdf")
        );
        assert_eq!(
            normalize_attachment_mime(None, "photo.png").as_deref(),
            Some("image/png")
        );
        assert_eq!(
            normalize_attachment_mime(None, "doc.docx").as_deref(),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
    }

    #[test]
    fn normalize_attachment_mime_returns_none_for_unknown_extension() {
        assert!(normalize_attachment_mime(None, "archive.zip").is_none());
        assert!(normalize_attachment_mime(None, "script.sh").is_none());
        assert!(normalize_attachment_mime(None, "noext").is_none());
    }

    #[test]
    fn architect_attachment_mime_supported_accepts_allowed_types() {
        for mime in &[
            "text/plain",
            "text/markdown",
            "text/csv",
            "application/json",
            "application/pdf",
            "image/png",
            "image/jpeg",
            "image/webp",
            "image/gif",
        ] {
            assert!(
                architect_attachment_mime_supported(mime),
                "should accept {mime}"
            );
        }
    }

    #[test]
    fn architect_attachment_mime_supported_rejects_disallowed_types() {
        for mime in &[
            "application/zip",
            "application/octet-stream",
            "text/html",
            "video/mp4",
            "audio/mpeg",
        ] {
            assert!(
                !architect_attachment_mime_supported(mime),
                "should reject {mime}"
            );
        }
    }

    // ARCHI-ATTACH-11: chat request parsing with attachments
    #[test]
    fn chat_request_parses_with_attachments() {
        let blob = sample_blob_ref("spec.md", "text/markdown", 4096);
        let raw = json!({
            "session_id": "sess_abc",
            "message": "revisa el spec",
            "attachments": [{
                "attachment_id": "att_001",
                "blob_ref": blob
            }]
        });
        let req: ChatRequest = serde_json::from_value(raw).expect("should parse");
        assert_eq!(req.session_id.as_deref(), Some("sess_abc"));
        assert_eq!(req.message, "revisa el spec");
        assert_eq!(req.attachments.len(), 1);
        assert_eq!(req.attachments[0].attachment_id, "att_001");
        assert_eq!(req.attachments[0].blob_ref.filename_original, "spec.md");
    }

    #[test]
    fn chat_request_parses_without_attachments() {
        let raw = json!({ "message": "hola" });
        let req: ChatRequest = serde_json::from_value(raw).expect("should parse");
        assert!(req.attachments.is_empty());
    }

    // ARCHI-ATTACH-11: persistence metadata shape
    #[test]
    fn persisted_message_attachments_extracts_from_metadata() {
        let blob = sample_blob_ref("spec.md", "text/markdown", 4096);
        let message = PersistedChatMessage {
            message_id: "msg_1".to_string(),
            session_id: "sess_1".to_string(),
            role: "user".to_string(),
            content: "texto".to_string(),
            timestamp_ms: 0,
            mode: "text".to_string(),
            metadata: json!({
                "kind": "text_with_attachments",
                "attachments": [{
                    "attachment_id": "att_1",
                    "filename": blob.filename_original,
                    "mime": blob.mime,
                    "size": blob.size,
                    "blob_ref": blob,
                }]
            }),
        };
        let attachments = persisted_message_attachments(&message);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "spec.md");
        assert_eq!(attachments[0].mime, "text/markdown");
        assert_eq!(attachments[0].size, 4096);
    }

    #[test]
    fn persisted_message_attachments_returns_empty_for_plain_text_message() {
        let message = PersistedChatMessage {
            message_id: "msg_1".to_string(),
            session_id: "sess_1".to_string(),
            role: "user".to_string(),
            content: "texto".to_string(),
            timestamp_ms: 0,
            mode: "text".to_string(),
            metadata: json!({ "kind": "text" }),
        };
        assert!(persisted_message_attachments(&message).is_empty());
    }

    // ARCHI-ATTACH-11: render_persisted_user_message
    #[test]
    fn render_persisted_user_message_with_attachments_includes_summary_lines() {
        let blob = sample_blob_ref("report.pdf", "application/pdf", 182233);
        let message = PersistedChatMessage {
            message_id: "msg_1".to_string(),
            session_id: "sess_1".to_string(),
            role: "user".to_string(),
            content: "revisa el reporte".to_string(),
            timestamp_ms: 0,
            mode: "text".to_string(),
            metadata: json!({
                "kind": "text_with_attachments",
                "attachments": [{
                    "attachment_id": "att_1",
                    "filename": blob.filename_original,
                    "mime": blob.mime,
                    "size": blob.size,
                    "blob_ref": blob,
                }]
            }),
        };
        let rendered = render_persisted_user_message(&message);
        assert!(rendered.contains("revisa el reporte"));
        assert!(rendered.contains("Attachment: report.pdf (application/pdf, 182233 bytes)"));
    }

    #[test]
    fn render_persisted_user_message_without_attachments_returns_content() {
        let message = PersistedChatMessage {
            message_id: "msg_1".to_string(),
            session_id: "sess_1".to_string(),
            role: "user".to_string(),
            content: "solo texto".to_string(),
            timestamp_ms: 0,
            mode: "text".to_string(),
            metadata: json!({ "kind": "text" }),
        };
        assert_eq!(render_persisted_user_message(&message), "solo texto");
    }

    // ARCHI-ATTACH-11: persistence metadata does not contain raw bytes
    #[test]
    fn user_metadata_for_attachment_contains_only_safe_fields() {
        let blob = sample_blob_ref("spec.md", "text/markdown", 4096);
        let attachment = ChatAttachmentRef {
            attachment_id: "att_1".to_string(),
            blob_ref: blob.clone(),
        };
        let metadata = json!({
            "kind": "text_with_attachments",
            "attachments": [json!({
                "attachment_id": attachment.attachment_id,
                "filename": attachment.blob_ref.filename_original,
                "mime": attachment.blob_ref.mime,
                "size": attachment.blob_ref.size,
                "blob_ref": attachment.blob_ref,
            })]
        });
        let serialized = metadata.to_string();
        // Must contain safe metadata
        assert!(serialized.contains("spec.md"));
        assert!(serialized.contains("text/markdown"));
        // Must NOT contain raw binary indicators
        assert!(!serialized.contains("data:"));
        assert!(!serialized.contains("base64,"));
    }
}
