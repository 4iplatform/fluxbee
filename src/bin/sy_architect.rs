#[path = "sy_architect/artifact_loop.rs"]
mod artifact_loop;
#[path = "sy_architect/failure_classifier.rs"]
mod failure_classifier;
#[path = "sy_architect/pipeline_types.rs"]
mod pipeline_types;
#[path = "sy_architect/reconciler.rs"]
mod reconciler;
use failure_classifier::{classify_failure_deterministic, route_failure, FailureContext};
use pipeline_types::{
    compiler_class_admin_steps, compiler_class_risk, cookbook_paths,
    desired_state_unknown_sections, is_valid_ownership_label, manifest_path, ActualStateSnapshot,
    ArtifactBundle, AuditSeverity, BuildTaskPacket, ChangeType, CookbookEntryV2, CookbookLayer,
    DeltaReport, DeltaReportStatus, DesiredHive, DesiredNode, DesiredOpaDeployment, DesiredRoute,
    DesiredRuntime, DesiredVpn, DesiredWfDeployment, FailureClass, PipelineRunRecord,
    PipelineRunStatus, PipelineStage, RepairPacket, RiskClass, SolutionManifestV2,
    MAX_DESIGN_ITERATIONS, MIN_DESIGN_SCORE_IMPROVEMENT, UNSUPPORTED_DESIRED_STATE_SECTIONS,
};

use std::collections::{hash_map::DefaultHasher, BTreeMap, HashMap};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
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
    ConversationSummary, FunctionCallingConfig, FunctionCallingRunner, FunctionLoopItem,
    FunctionRunInput, FunctionTool, FunctionToolDefinition, FunctionToolProvider,
    FunctionToolRegistry, ImmediateConversationMemory, ImmediateInteraction,
    ImmediateInteractionKind, ImmediateOperation, ImmediateRole, ModelInputOptions, ModelSettings,
    OpenAiResponsesClient,
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
use json_router::runtime_manifest::{
    load_runtime_manifest_from_paths, RuntimeManifest, RuntimeManifestEntry,
};
use json_router::runtime_package::{DIST_RUNTIME_MANIFEST_PATH, DIST_RUNTIME_ROOT_DIR};
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
const PIPELINE_RUNS_TABLE: &str = "pipeline_runs";
const MANIFEST_REFS_TABLE: &str = "manifest_refs";
const CHAT_SESSION_PROFILES_TABLE: &str = "session_profiles";
const CHAT_MODE_OPERATOR: &str = "operator";
const CHAT_MODE_IMPERSONATION: &str = "impersonation";
const ARCHITECT_LOCAL_SECRET_KEY_OPENAI: &str = "openai_api_key";
const ARCHITECT_MAX_ATTACHMENTS: usize = 8;
const ARCHITECT_MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const ARCHITECT_MAX_SOFTWARE_UPLOAD_BYTES: usize = 128 * 1024 * 1024;
const ARCHITECT_MAX_MULTIPART_UPLOAD_BYTES: usize =
    ARCHITECT_MAX_SOFTWARE_UPLOAD_BYTES + (4 * 1024 * 1024);
const STATUS_REFRESH_INTERVAL_SECS: u64 = 60;
fn build_archi_prompt(handbook: Option<&str>) -> String {
    let mut prompt = r#"You are Archi, the Fluxbee system coordinator.

## Role

You are a coordinator, not an executor. You read system state and route all mutations through controlled planning tools. You never call admin actions directly — every state change eventually goes through `fluxbee_plan_compiler`.

## Reading system state

- For all nodes across the system: use `/inventory` or `/inventory/summary`. Use `/hives/{hive}/nodes` only for a single explicit hive.
- For software versions: use `/versions` or `/hives/{hive}/versions`. Do not infer versions from `/hives/{hive}/nodes`.
- For runtime/package info: use `/hives/{hive}/runtimes` or `/hives/{hive}/runtimes/{runtime}`, not `/hives/{hive}/nodes`.
- For deployments and drift: use `/hives/{hive}/deployments` or `/hives/{hive}/drift-alerts`. If these return `entries: []`, report zero entries — do not infer data from broader endpoints.
- Stored node config: `GET /hives/{hive}/nodes/{node_name}/config` (persisted snapshot). Live node contract: `POST /hives/{hive}/nodes/{node_name}/control/config-get`. Prefer GET first; use control/config-get only when the live contract is needed.
- For admin action schemas and examples, use `fluxbee_system_get` on `/admin/actions/{action}` when answering questions. Planning tools do their own schema lookup before mutation.

## Making changes

Never stage writes. Never call admin mutation endpoints directly.

- `fluxbee_plan_compiler` — primary path for clear mutations, from one step to many steps. It translates the task to an executor_plan and executes only after the operator confirms the prepared plan.
- `fluxbee_start_pipeline` — for broad design intents that need manifest + reconcile before a plan can be produced: new topology, multi-resource architecture, or unclear desired-state work.

If the operator's mutation intent is clear, call `fluxbee_plan_compiler` directly. Do not ask "should I continue?" first. The operator confirms once, after the plan is ready. Do not claim a mutation ran before confirmation.

## Pipeline confirmation rules

- After `fluxbee_start_pipeline` returns, call NO more tools. Present its returned `message` verbatim. If it produced an executor plan, the next CONFIRM approves execution. If it blocked before a plan, explain the blocker and next option.
- After `fluxbee_plan_compiler` returns a plan, call NO more tools. Present the `human_summary` and end with "Reply CONFIRM to execute or CANCEL to discard." Only the literal word CONFIRM triggers execution. Never call `fluxbee_plan_compiler` more than once per task.

## Resolving a blocked pipeline

If `fluxbee_start_pipeline` returns `status: "blocked_run_pending"`, the session has a previous pipeline stuck in `Blocked` state. Do NOT just retry `fluxbee_start_pipeline` — it will keep returning the same status. Instead, present the three options to the operator and call `fluxbee_pipeline_action` with their choice:

- `discard` — drop the blocked run, then call `fluxbee_start_pipeline` again for the new task.
- `restart_from_design` — close the blocked run and re-run the same task from Design (returns a fresh Confirm1 payload).
- `retry` — return the blocked run to its last CONFIRM checkpoint so the operator can re-engage execution with a CONFIRM message.

If the operator's intent is clearly a different task than the blocked one, default to `discard` then start the new pipeline. Ask only when the choice is genuinely ambiguous.

## Asking questions — maximum one per task

Do not ask multiple clarifying questions. If the task is reasonably clear, choose the correct planning tool without asking first. If one critical piece is truly missing and cannot be inferred (e.g., which `tenant_id` for a new multi-tenant node), ask exactly one specific question. Never ask several things at once.

## General

- Be direct and operational. Prefer short concrete answers.
- Do not claim actions ran unless they actually did.
- When unsure of field names or schemas while answering, inspect `/admin/actions/{action}` — never guess payloads."#.to_string();

    if let Some(handbook_text) = handbook.filter(|t| !t.trim().is_empty()) {
        prompt.push_str("\n\n---\n\n## Reference handbook\n\n");
        prompt.push_str(handbook_text);
    }

    prompt
}
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
    ai_runtime: Arc<Mutex<Option<ArchitectAiRuntime>>>,
    session_id: Option<String>,
    admin_actions_cache: Arc<Mutex<Option<AdminActionsCache>>>,
    plan_compile_pending: Arc<Mutex<HashMap<String, PlanCompilePending>>>,
    active_pipeline_runs: Arc<Mutex<HashMap<String, PipelineRunRecord>>>,
}

struct AdminActionsCache {
    actions: Vec<Value>,
    fetched_at_ms: u64,
}

const ADMIN_ACTIONS_CACHE_TTL_MS: u64 = 300_000; // 5 minutes
const TASK_AGENT_TOKEN_BUDGET: u32 = 50_000;

#[derive(Debug, Clone)]
struct TaskTokenBudget {
    used: u32,
    budget: u32,
}

impl TaskTokenBudget {
    fn new(initial_used: u32) -> Self {
        Self {
            used: initial_used,
            budget: TASK_AGENT_TOKEN_BUDGET,
        }
    }

    fn ensure_room(&self, stage: &str) -> Result<(), ArchitectError> {
        if self.used >= self.budget {
            return Err(token_budget_exceeded_error(stage, self.used, self.budget));
        }
        Ok(())
    }

    fn add(&mut self, stage: &str, tokens: u32) -> Result<(), ArchitectError> {
        self.used = self.used.saturating_add(tokens);
        if self.used > self.budget {
            return Err(token_budget_exceeded_error(stage, self.used, self.budget));
        }
        Ok(())
    }
}

fn token_budget_exceeded_error(stage: &str, tokens_used: u32, budget: u32) -> ArchitectError {
    json!({
        "error": "BUDGET_EXCEEDED",
        "stage": stage,
        "tokens_used": tokens_used,
        "budget": budget,
    })
    .to_string()
    .into()
}

const PLAN_COMPILE_COOKBOOK_MAX_ENTRIES: usize = 50;
const HANDBOOK_CANDIDATE_PATHS: &[&str] = &[
    // Production path — installed by install.sh to /etc/fluxbee/
    "/etc/fluxbee/handbook_fluxbee.md",
    // Dev paths — relative to repo root (used when CWD = repo)
    "docs/onworking COA/archi/handbook_fluxbee.md",
    "docs/onworking COA/handbook_fluxbee.md",
];

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
    admin_actions_cache: Arc<Mutex<Option<AdminActionsCache>>>,
    plan_compile_pending: Arc<Mutex<HashMap<String, PlanCompilePending>>>,
    active_pipeline_runs: Arc<Mutex<HashMap<String, PipelineRunRecord>>>,
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
struct PackagePublishRequest {
    blob_name: String,
    #[serde(default)]
    set_current: Option<bool>,
    #[serde(default)]
    preserve_node_config: bool,
    #[serde(default)]
    sync_to: Vec<String>,
    #[serde(default)]
    update_to: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SoftwareUploadResponse {
    status: String,
    upload_kind: String,
    attachment: UploadedAttachment,
}

#[derive(Debug, Deserialize)]
struct SoftwarePublishRequest {
    blob_name: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    set_current: Option<bool>,
    #[serde(default)]
    preserve_node_config: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SoftwarePublishResolution {
    upload_kind: String,
    runtime_name: Option<String>,
    operation: String,
    package_type: Option<String>,
    current_version: Option<String>,
    next_version: Option<String>,
    executable_path: Option<String>,
    summary: String,
}

#[derive(Debug, Clone)]
struct ResolvedSoftwarePublish {
    resolution: SoftwarePublishResolution,
    uploaded_blob_path: PathBuf,
    generated_exec_filename: String,
    source_package_dir: Option<PathBuf>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_recovery: Option<PipelineRecoveryInfo>,
}

#[derive(Debug, Serialize)]
struct SessionMetaResponse {
    session_id: String,
    message_count: u64,
    last_activity_at_ms: u64,
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_recovery: Option<PipelineRecoveryInfo>,
}

#[derive(Debug, Serialize)]
struct SessionDeleteResponse {
    status: String,
    deleted_sessions: u64,
    deleted_messages: u64,
}

#[derive(Debug, Deserialize)]
struct PipelineRecoveryActionRequest {
    action: String,
    pipeline_run_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct PipelineRecoveryActionResponse {
    status: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_recovery: Option<PipelineRecoveryInfo>,
}

#[derive(Debug, Serialize, Clone)]
struct PipelineRecoveryInfo {
    pipeline_run_id: String,
    solution_id: Option<String>,
    status: String,
    current_stage: String,
    interrupted_at_ms: Option<u64>,
    can_resume: bool,
    can_discard: bool,
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

#[derive(Debug, Clone)]
struct PlanCompileTraceStep {
    id: String,
    action: String,
    args_preview: String,
}

#[derive(Debug, Clone)]
struct PlanCompileTrace {
    task: String,
    hive: String,
    steps: Vec<PlanCompileTraceStep>,
    step_count: usize,
    validation: String,
    first_validation_error: Option<String>,
    tokens_used: u32,
    token_budget: u32,
}

struct PlanCompilePending {
    plan: Value,
    cookbook_entry: Option<CookbookEntryV2>,
    trace: Option<PlanCompileTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct VerificationVerdict {
    eligible_for_cookbook: bool,
    reason: String,
    verified_success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DesignAuditStatus {
    Pass,
    Revise,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DesignFinding {
    code: String,
    section: String,
    message: String,
    severity: AuditSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DesignAuditVerdict {
    verdict_id: String,
    manifest_version: String,
    status: DesignAuditStatus,
    score: u8,
    blocking_issues: Vec<String>,
    findings: Vec<DesignFinding>,
    summary: String,
    produced_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Confirm1Summary {
    solution_name: String,
    hive_count: usize,
    node_count: usize,
    route_count: usize,
    main_runtimes: Vec<String>,
    audit_status: DesignAuditStatus,
    audit_score: u8,
    blocking_issues: Vec<String>,
    advisory_highlights: Vec<String>,
    warnings: Vec<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DesignerTrace {
    task: String,
    solution_id: Option<String>,
    query_hive_calls: u32,
    manifest_version: String,
    section_count: usize,
    validation_result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DesignLoopStopReason {
    Passed,
    MaxIterations,
    NoScoreImprovement,
    RepeatedBlocker,
    AuditRejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct DesignLoopTraceEvent {
    iteration: u32,
    stage: String,
    score: Option<u8>,
    status: Option<DesignAuditStatus>,
    blocking_issue_count: usize,
    stopped_reason: Option<DesignLoopStopReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesignLoopOutput {
    manifest: SolutionManifestV2,
    solution_id: String,
    manifest_path: String,
    designer_human_summary: String,
    designer_traces: Vec<DesignerTrace>,
    audit_verdict: DesignAuditVerdict,
    confirm1_summary: Confirm1Summary,
    iterations_used: u32,
    stopped_reason: DesignLoopStopReason,
    trace: Vec<DesignLoopTraceEvent>,
    tokens_used: u32,
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
        registry.register(Arc::new(StartPipelineTool::new(self.context.clone())))?;
        registry.register(Arc::new(PipelineActionTool::new(self.context.clone())))?;
        registry.register(Arc::new(PlanCompilerTool::new(self.context.clone())))
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

struct GetManifestCurrentTool {
    context: ArchitectAdminToolContext,
}

impl GetManifestCurrentTool {
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
                "Read live Fluxbee system state through SY.admin for hive {hive}. Read-only. \
                Supports GET and safe POST checks (OPA policy check, node CONFIG_GET). \
                Key paths: /inventory (all nodes), /versions, /hives/{{hive}}/runtimes, \
                /hives/{{hive}}/nodes, /hives/{{hive}}/nodes/{{name}}/config (persisted), \
                /hives/{{hive}}/nodes/{{name}}/control/config-get (live contract), \
                /hives/{{hive}}/wf-rules, /hives/{{hive}}/drift-alerts, \
                /admin/actions/{{action}} (schema + example_scmd). \
                Never execute paths with literal {{placeholders}}. \
                For admin action schemas, prefer example_scmd over guessing.",
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
impl FunctionTool for GetManifestCurrentTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "get_manifest_current".to_string(),
            description: "Read the latest saved solution_manifest for a solution_id. If solution_id is omitted, resolves it from the current session's active pipeline solution. Read-only.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "solution_id": {
                        "type": "string",
                        "description": "Optional explicit solution_id. When omitted, use the current session's active solution."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let explicit_solution_id = arguments
            .get("solution_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let solution_id = match explicit_solution_id {
            Some(solution_id) => solution_id,
            None => {
                let session_id = require_session_id(&self.context, "get_manifest_current")?;
                latest_solution_id_for_session(&self.context, session_id)
                    .await
                    .map_err(|err| {
                        fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                            "get_manifest_current failed to resolve current session solution: {err}"
                        ))
                    })?
                    .ok_or_else(|| {
                        fluxbee_ai_sdk::AiSdkError::Protocol(
                            "get_manifest_current could not resolve an active solution for this session".to_string(),
                        )
                    })?
            }
        };

        let manifest = load_manifest_from_state_dir(&self.context.state_dir, &solution_id, None)
            .await
            .map_err(|err| {
                fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                    "get_manifest_current storage read failed: {err}"
                ))
            })?;

        Ok(match manifest {
            Some(manifest) => json!({
                "found": true,
                "solution_id": solution_id,
                "solution_manifest": manifest,
            }),
            None => json!({
                "found": false,
                "solution_id": solution_id,
            }),
        })
    }
}

// ── Admin actions cache (PROG-T1/T2) ─────────────────────────────────────────

async fn get_or_refresh_admin_actions(
    context: &ArchitectAdminToolContext,
) -> Result<Vec<Value>, ArchitectError> {
    let now_ms = now_epoch_ms();
    {
        let guard = context.admin_actions_cache.lock().await;
        if let Some(cache) = guard.as_ref() {
            if now_ms.saturating_sub(cache.fetched_at_ms) < ADMIN_ACTIONS_CACHE_TTL_MS {
                return Ok(cache.actions.clone());
            }
        }
    }

    let output = execute_admin_action_with_context(
        context,
        &format!("SY.admin@{}", context.hive_id),
        "list_admin_actions",
        None,
        json!({}),
        "plan_compiler.cache_refresh",
    )
    .await;

    match output {
        Ok(val) => {
            let actions: Vec<Value> = val
                .get("payload")
                .and_then(|p| p.get("actions"))
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            let mut guard = context.admin_actions_cache.lock().await;
            *guard = Some(AdminActionsCache {
                actions: actions.clone(),
                fetched_at_ms: now_ms,
            });
            Ok(actions)
        }
        Err(err) => {
            // return stale cache if available rather than failing hard
            let guard = context.admin_actions_cache.lock().await;
            if let Some(cache) = guard.as_ref() {
                tracing::warn!(error = %err, "admin actions refresh failed, using stale cache");
                return Ok(cache.actions.clone());
            }
            Err(err)
        }
    }
}

// ── Programmer cookbook (PROG-T5/T6) ─────────────────────────────────────────

fn plan_compile_cookbook_path(state_dir: &Path) -> PathBuf {
    state_dir.join(cookbook_paths::PLAN_COMPILE)
}

fn artifact_cookbook_path(state_dir: &Path) -> PathBuf {
    state_dir.join(cookbook_paths::ARTIFACT)
}

fn sanitize_pattern_key(seed: &str) -> String {
    let mut key = seed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    while key.contains("__") {
        key = key.replace("__", "_");
    }
    let key = key.trim_matches('_');
    if key.is_empty() {
        "plan_compile_pattern".to_string()
    } else {
        key.to_string()
    }
}

fn normalize_plan_compile_cookbook_entry(mut entry: CookbookEntryV2) -> CookbookEntryV2 {
    entry.layer = CookbookLayer::PlanCompile;
    entry.pattern_key = sanitize_pattern_key(&entry.pattern_key);
    if entry.pattern_key.is_empty() {
        entry.pattern_key = "plan_compile_pattern".to_string();
    }
    if entry.seed_kind.trim().is_empty() {
        entry.seed_kind = "observed".to_string();
    }
    entry
}

fn parse_plan_compile_cookbook_entry(value: &Value) -> Option<CookbookEntryV2> {
    serde_json::from_value::<CookbookEntryV2>(value.clone())
        .ok()
        .map(normalize_plan_compile_cookbook_entry)
}

fn normalize_artifact_cookbook_entry(mut entry: CookbookEntryV2) -> CookbookEntryV2 {
    entry.layer = CookbookLayer::Artifact;
    entry.pattern_key = sanitize_pattern_key(&entry.pattern_key);
    if entry.pattern_key.is_empty() {
        entry.pattern_key = "artifact_pattern".to_string();
    }
    if entry.seed_kind.trim().is_empty() {
        entry.seed_kind = "observed".to_string();
    }
    entry
}

// Ensures the cookbook directory exists and the cookbook file is a valid JSON
// array. If the file is missing, corrupt, or unreadable it is silently reset to
// an empty array so that a fresh write can succeed.
fn ensure_plan_compile_cookbook_dir(state_dir: &Path) {
    let path = plan_compile_cookbook_path(state_dir);
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state_dir.to_path_buf());
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %err, path = %dir.display(), "plan_compile cookbook: could not create directory");
        return;
    }
    if path.exists() {
        // Validate: if the file is unreadable or not a JSON array, reset it.
        let ok = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|v| v.is_array())
            .unwrap_or(false);
        if !ok {
            tracing::warn!(path = %path.display(), "plan_compile cookbook corrupted — resetting to empty");
            let _ = std::fs::write(&path, b"[]");
        }
    } else {
        // First run: create an empty cookbook so subsequent reads always succeed.
        if let Err(err) = std::fs::write(&path, b"[]") {
            tracing::warn!(error = %err, path = %path.display(), "plan_compile cookbook: could not create initial file");
        }
    }
}

fn ensure_artifact_cookbook_dir(state_dir: &Path) {
    let path = artifact_cookbook_path(state_dir);
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state_dir.to_path_buf());
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %err, path = %dir.display(), "artifact cookbook: could not create directory");
        return;
    }
    if path.exists() {
        let ok = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|v| v.is_array())
            .unwrap_or(false);
        if !ok {
            tracing::warn!(path = %path.display(), "artifact cookbook corrupted — resetting to empty");
            let _ = std::fs::write(&path, b"[]");
        }
    } else if let Err(err) = std::fs::write(&path, b"[]") {
        tracing::warn!(error = %err, path = %path.display(), "artifact cookbook: could not create initial file");
    }
}

fn design_audit_path(state_dir: &Path, run_id: &str, iteration: u32) -> PathBuf {
    state_dir
        .join("pipeline")
        .join(run_id)
        .join(format!("design_audit_{iteration}.json"))
}

fn save_design_audit_verdict(
    state_dir: &Path,
    run_id: &str,
    iteration: u32,
    verdict: &DesignAuditVerdict,
) -> Result<PathBuf, ArchitectError> {
    let path = design_audit_path(state_dir, run_id, iteration);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(verdict)?;
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(test)]
fn load_design_audit_verdict(
    state_dir: &Path,
    run_id: &str,
    iteration: u32,
) -> Result<Option<DesignAuditVerdict>, ArchitectError> {
    let path = design_audit_path(state_dir, run_id, iteration);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

fn read_plan_compile_cookbook(state_dir: &Path) -> Vec<CookbookEntryV2> {
    let path = plan_compile_cookbook_path(state_dir);
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(error = %err, path = %path.display(), "plan_compile cookbook unreadable — starting empty");
            return vec![];
        }
    };
    serde_json::from_slice::<Vec<CookbookEntryV2>>(&raw)
        .map(|entries| {
            entries
                .into_iter()
                .map(normalize_plan_compile_cookbook_entry)
                .collect()
        })
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "plan_compile cookbook parse error — starting empty");
            vec![]
        })
}

fn write_plan_compile_cookbook(state_dir: &Path, entries: &[CookbookEntryV2]) {
    let path = plan_compile_cookbook_path(state_dir);
    let canonical_entries = entries
        .iter()
        .cloned()
        .map(normalize_plan_compile_cookbook_entry)
        .collect::<Vec<_>>();
    let json_bytes = match serde_json::to_vec_pretty(&canonical_entries) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(error = %err, "plan_compile cookbook serialize failed");
            return;
        }
    };
    // Write to a temp file first, then rename — avoids leaving a corrupt file
    // on partial write (e.g. process killed mid-write).
    let tmp_path = path.with_extension("tmp");
    if let Err(err) = std::fs::write(&tmp_path, &json_bytes) {
        tracing::warn!(error = %err, path = %tmp_path.display(), "plan_compile cookbook write failed");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp_path, &path) {
        tracing::warn!(error = %err, "plan_compile cookbook rename failed — removing tmp");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

fn read_artifact_cookbook(state_dir: &Path) -> Vec<CookbookEntryV2> {
    let path = artifact_cookbook_path(state_dir);
    let raw = match std::fs::read(&path) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(error = %err, path = %path.display(), "artifact cookbook unreadable — starting empty");
            return vec![];
        }
    };
    serde_json::from_slice::<Vec<CookbookEntryV2>>(&raw)
        .map(|entries| {
            entries
                .into_iter()
                .map(normalize_artifact_cookbook_entry)
                .collect()
        })
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "artifact cookbook parse error — starting empty");
            vec![]
        })
}

fn write_artifact_cookbook(state_dir: &Path, entries: &[CookbookEntryV2]) {
    let path = artifact_cookbook_path(state_dir);
    let canonical_entries = entries
        .iter()
        .cloned()
        .map(normalize_artifact_cookbook_entry)
        .collect::<Vec<_>>();
    let json_bytes = match serde_json::to_vec_pretty(&canonical_entries) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(error = %err, "artifact cookbook serialize failed");
            return;
        }
    };
    let tmp_path = path.with_extension("tmp");
    if let Err(err) = std::fs::write(&tmp_path, &json_bytes) {
        tracing::warn!(error = %err, path = %tmp_path.display(), "artifact cookbook write failed");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp_path, &path) {
        tracing::warn!(error = %err, "artifact cookbook rename failed — removing tmp");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

fn append_plan_compile_cookbook_entry(state_dir: &Path, entry: CookbookEntryV2) {
    let mut entries = read_plan_compile_cookbook(state_dir);
    entries.push(normalize_plan_compile_cookbook_entry(entry));
    if entries.len() > PLAN_COMPILE_COOKBOOK_MAX_ENTRIES {
        let drop = entries.len() - PLAN_COMPILE_COOKBOOK_MAX_ENTRIES;
        entries.drain(0..drop);
    }
    write_plan_compile_cookbook(state_dir, &entries);
}

fn append_artifact_cookbook_entry(state_dir: &Path, entry: CookbookEntryV2) {
    let mut entries = read_artifact_cookbook(state_dir);
    entries.push(normalize_artifact_cookbook_entry(entry));
    if entries.len() > PLAN_COMPILE_COOKBOOK_MAX_ENTRIES {
        let drop = entries.len() - PLAN_COMPILE_COOKBOOK_MAX_ENTRIES;
        entries.drain(0..drop);
    }
    write_artifact_cookbook(state_dir, &entries);
}

fn write_design_cookbook(state_dir: &Path, entries: &[CookbookEntryV2]) {
    let path = state_dir.join(cookbook_paths::DESIGN);
    let json_bytes = match serde_json::to_vec_pretty(entries) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(error = %err, "design cookbook serialize failed");
            return;
        }
    };
    let tmp_path = path.with_extension("tmp");
    if let Err(err) = std::fs::write(&tmp_path, &json_bytes) {
        tracing::warn!(error = %err, path = %tmp_path.display(), "design cookbook write failed");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp_path, &path) {
        tracing::warn!(error = %err, "design cookbook rename failed — removing tmp");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

fn append_design_cookbook_entry(state_dir: &Path, entry: CookbookEntryV2) {
    let mut entries = read_design_cookbook(state_dir);
    entries.push(entry);
    if entries.len() > PLAN_COMPILE_COOKBOOK_MAX_ENTRIES {
        let drop = entries.len() - PLAN_COMPILE_COOKBOOK_MAX_ENTRIES;
        entries.drain(0..drop);
    }
    write_design_cookbook(state_dir, &entries);
}

fn read_design_cookbook(state_dir: &Path) -> Vec<CookbookEntryV2> {
    let path = state_dir.join(cookbook_paths::DESIGN);
    let raw = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return vec![],
    };
    serde_json::from_slice::<Vec<CookbookEntryV2>>(&raw).unwrap_or_default()
}

fn read_handbook_text() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    HANDBOOK_CANDIDATE_PATHS
        .iter()
        .map(|candidate| cwd.join(candidate))
        .find_map(|path| std::fs::read_to_string(path).ok())
}

// ── TF: Seed cookbooks from repo defaults on startup ─────────────────────────
// Each cookbook is seeded once from seeds/cookbook/{name} if the runtime file
// is missing or empty. Once the pipeline appends observed patterns, the seeds
// are never overwritten — they are bootstrapping data, not configuration.
fn seed_cookbooks_from_defaults(state_dir: &Path) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(_) => return,
    };

    let pairs: &[(&str, &str)] = &[
        (
            cookbook_paths::DESIGN,
            "seeds/cookbook/design_cookbook_v1.json",
        ),
        (
            cookbook_paths::ARTIFACT,
            "seeds/cookbook/artifact_cookbook_v1.json",
        ),
        (
            cookbook_paths::PLAN_COMPILE,
            "seeds/cookbook/plan_compile_cookbook_v1.json",
        ),
        (
            cookbook_paths::REPAIR,
            "seeds/cookbook/repair_cookbook_v1.json",
        ),
    ];

    for (runtime_rel, seed_rel) in pairs {
        let runtime_path = state_dir.join(runtime_rel);
        let is_empty = !runtime_path.exists()
            || std::fs::metadata(&runtime_path)
                .map(|m| m.len() <= 2)
                .unwrap_or(true);
        if !is_empty {
            continue;
        }
        let seed_path = cwd.join(seed_rel);
        let raw = match std::fs::read(&seed_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Some(parent) = runtime_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&runtime_path, &raw) {
            tracing::warn!(
                seed = %seed_path.display(),
                target = %runtime_path.display(),
                error = %err,
                "cookbook seed write failed"
            );
        } else {
            tracing::info!(
                target = %runtime_path.display(),
                "cookbook seeded from defaults"
            );
        }
    }
}

// ── TE-2: Residual AI failure classifier ─────────────────────────────────────
// Called only when classify_failure_deterministic returns None.
// Uses a direct chat completion (no function calling) to classify unknown errors.
// Always returns a FailureClass — falls back to UnknownResidual if AI is unavailable.
async fn classify_failure_with_ai(
    state: &ArchitectState,
    error_text: &str,
    ctx: &failure_classifier::FailureContext,
) -> FailureClass {
    let runtime = match state.ai_runtime.lock().await.clone() {
        Some(r) => r,
        None => return FailureClass::UnknownResidual,
    };

    let stage_label = format!("{:?}", ctx.stage);
    let code_label = ctx.error_code.as_deref().unwrap_or("none");
    let prompt = format!(
        "You are a failure classifier for the Fluxbee pipeline. \
        Classify the following error into exactly one FailureClass variant. \
        Respond with ONLY the variant name — no explanation, no markdown.\n\n\
        Valid variants: DesignIncomplete, DesignConflict, SnapshotPartialBlocking, \
        SnapshotSectionUnsupported, DeltaUnsupported, ArtifactTaskUnderspecified, \
        ArtifactContractInvalid, ArtifactLayoutInvalid, PlanInvalid, PlanContractInvalid, \
        ExecutionEnvironmentMissing, ExecutionActionFailed, ExecutionTimeout, UnknownResidual\n\n\
        Stage: {stage_label}\n\
        Error code: {code_label}\n\
        Error: {error_text}"
    );

    let model = runtime.client.clone().function_model(
        runtime.model.clone(),
        None,
        runtime.model_settings.clone(),
    );
    let tools = FunctionToolRegistry::new();
    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let result = runner
        .run_with_input(
            &model,
            &tools,
            FunctionRunInput {
                current_user_message: prompt,
                current_user_parts: None,
                immediate_memory: None,
            },
        )
        .await;

    let raw = match result {
        Ok(r) => r.final_assistant_text.unwrap_or_default(),
        Err(_) => return FailureClass::UnknownResidual,
    };

    let label = raw.trim();
    match label {
        "DesignIncomplete" => FailureClass::DesignIncomplete,
        "DesignConflict" => FailureClass::DesignConflict,
        "SnapshotPartialBlocking" => FailureClass::SnapshotPartialBlocking,
        "SnapshotSectionUnsupported" => FailureClass::SnapshotSectionUnsupported,
        "DeltaUnsupported" => FailureClass::DeltaUnsupported,
        "ArtifactTaskUnderspecified" => FailureClass::ArtifactTaskUnderspecified,
        "ArtifactContractInvalid" => FailureClass::ArtifactContractInvalid,
        "ArtifactLayoutInvalid" => FailureClass::ArtifactLayoutInvalid,
        "PlanInvalid" => FailureClass::PlanInvalid,
        "PlanContractInvalid" => FailureClass::PlanContractInvalid,
        "ExecutionEnvironmentMissing" => FailureClass::ExecutionEnvironmentMissing,
        "ExecutionActionFailed" => FailureClass::ExecutionActionFailed,
        "ExecutionTimeout" => FailureClass::ExecutionTimeout,
        _ => FailureClass::UnknownResidual,
    }
}

fn sanitize_solution_id(seed: &str) -> String {
    let mut value = seed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while value.contains("--") {
        value = value.replace("--", "-");
    }
    value = value.trim_matches('-').to_string();
    if value.is_empty() {
        format!("solution-{}", now_epoch_ms())
    } else {
        value
    }
}

fn resolve_manifest_solution_id(manifest: &SolutionManifestV2, explicit: Option<&str>) -> String {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_solution_id)
        .or_else(|| {
            manifest
                .solution
                .get("solution_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(sanitize_solution_id)
        })
        .or_else(|| {
            manifest
                .solution
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(sanitize_solution_id)
        })
        .unwrap_or_else(|| sanitize_solution_id(""))
}

// ── Programmer system prompt builder (PROG-T8/T10) ───────────────────────────

const PLAN_COMPILER_SYSTEM_PROMPT_BASE: &str = r#"You are the plan_compiler agent inside SY.architect.

Your only job is to translate a deployment task or delta_report into a valid executor_plan JSON.
You do NOT interpret user intent — Archi already did that and gave you a clear task.
You do NOT ask questions. You do NOT produce explanations. You call submit_executor_plan exactly once.

## executor_plan format

```json
{
  "plan_version": "0.1",
  "kind": "executor_plan",
  "metadata": {
    "name": "<short_snake_case_name>",
    "target_hive": "<hive_id>"
  },
  "execution": {
    "strict": true,
    "stop_on_error": true,
    "allow_help_lookup": true,
    "steps": [
      {
        "id": "s1",
        "action": "<action_name>",
        "args": { ... }
      }
    ]
  }
}
```

## Rules

- Use only actions from the AVAILABLE ACTIONS section below. Never invent action names.
- Every arg field must come from the action's documented schema. Never invent arg fields.
- Step ids must be unique. Use s1, s2, s3, ... in order.
- Ordering: publish_runtime_package steps before run_node steps; run_node steps before add_route steps.
- If the context says a runtime is already published, skip its publish_runtime_package step.
- If the context says a node is already running, skip its run_node step.
- target_hive in metadata is the primary hive for this deployment.
- For multi-hive operations, individual step args carry the specific hive.

## Delta-report mode

When the input includes `delta_report`, you are in the canonical pipeline path.

- Translate only the operations in `delta_report.operations`.
- Use the compiler_class translation table provided in the input context.
- Do not add extra mutating steps not justified by a delta operation.
- Do not skip required steps for a delta operation unless the operation is NOOP.
- If a delta operation has a blocked compiler_class with no translation, do not invent a workaround.
- Treat free-form `task` as deprecated context only when `delta_report` is present.
- If `approved_artifacts` entries include `publish_source`, use that exact source for any `publish_runtime_package` step justified by the delta. Do not reconstruct inline files or blob paths manually when a canonical publish_source is already provided.

## Fluxbee node naming convention

Node names in Fluxbee always include a type prefix followed by a dot, then the instance name, then `@hive`. The prefixes are: `AI.` for AI/language model nodes, `WF.` for workflow nodes, `IO.` for I/O integration nodes, `SY.` for system nodes. Examples: `AI.coa@motherbee`, `WF.router@worker-220`, `IO.slack.main@motherbee`. Never create a `node_name` without its type prefix — a name like `coa@motherbee` is invalid.

## Querying live hive state before generating the plan

Use `query_hive` to read live state from the hive BEFORE generating steps. This is how you find out what runtimes exist, which nodes are running, what routes are configured, etc. Never include read actions (list_runtimes, inventory, etc.) as steps in the executor plan — executor plan args are static, so a step's output cannot feed into another step's args at runtime.

Examples:
- `query_hive(action="list_runtimes", hive="motherbee")` → see which runtimes are available and materialized
- `query_hive(action="inventory")` → see which nodes are already running
- `query_hive(action="get_runtime", hive="motherbee", params={"runtime": "ai.common"})` → verify a specific runtime

## Choosing a runtime

When the task requires creating a new node and the runtime is not specified, call `query_hive` with `list_runtimes` to see what is available. Choose the runtime whose name and type match the node being created (e.g. for an AI node, look for `ai.*` runtimes). Do not reuse the runtime name of an existing node (e.g. `ai.chat`) as the base for a different new node unless explicitly requested.

## Looking up action schemas

Before generating a step for any action, call `get_admin_action_help` with the action name. The response contains the exact required and optional fields, their types, and an example. Use it — do not guess arg names or assume fields based on action name alone.

## human_summary rules

- Write one paragraph, plain language, no JSON, no commands, no technical field names.
- Describe what will happen from the operator's perspective.
- Example: "I'll publish the WF routing runtime on motherbee, then start three nodes on worker-220, and configure the routes so messages from AI.support flow through WF.router to IO.slack and IO.email."

## Workflow definition schema (wf_rules_compile_apply)

`workflow_name` must match `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$` — lowercase, digits, hyphens, dot-segments. Underscores are rejected. Convert snake_case to kebab-case (e.g. `response_dispatch` → `response-dispatch`).

Required top-level fields: `wf_schema_version: "1"`, `workflow_type` (matches workflow_name), `description`, `input_schema` (JSON Schema), `initial_state`, `terminal_states: [...]`, `states: [...]`.

Each state: `name`, `description`, `entry_actions: []`, `exit_actions: []`, `transitions: []`.

Action types: `send_message` (fields: `type`, `target` as node instance name, `meta: {"msg":"...", "type":"..."}`, `payload`), `schedule_timer` (fields: `type`, `timer_key`, `fire_in`, `missed_policy`), `cancel_timer` (fields: `type`, `timer_key`), `reschedule_timer` (fields: `type`, `timer_key`, `fire_in`), `set_variable` (fields: `type`, `name`, `value` as CEL expression).

`send_message` critical: `msg` and `type` go INSIDE `meta` — never as top-level fields. No `type_meta` field. Unknown top-level fields cause compile errors.

Payload `$ref` syntax: `{"$ref": "input.field"}`, `{"$ref": "state.field"}`, `{"$ref": "event.field"}` for runtime values. Plain JSON values are literals.

Each transition: `event_match: {"msg":"..."}`, optional `guard` (CEL), `target_state`, `actions: []`.

CEL guard variables: `input` (creation event payload), `state` (workflow variables), `event` (`event.payload` for user data, `event.user_payload` for timer metadata).

`tenant_id` is required in the `wf_rules_compile_apply` body when `auto_spawn: true` and the `WF.<name>@<hive>` node does not yet exist.

## cookbook_entry rules

- Include only if the pattern is genuinely reusable for future similar tasks.
- Use the canonical CookbookEntryV2 shape.
- layer must be exactly "plan_compile".
- pattern_key must be lowercase with underscores and must describe the reusable pattern.
- trigger: one sentence describing when the pattern applies.
- successful_shape: summarize the action order and structural shape, for example {"steps_pattern":["publish_runtime_package","run_node","add_route"]}.
- constraints: ordering requirements or assumptions that must hold.
- do_not_repeat: common mistakes to avoid for this pattern.
- seed_kind should be "observed" for entries produced from real runs.
- Omit cookbook_entry for one-off tasks or tasks with very specific non-reusable parameters.
"#;

fn build_plan_compiler_prompt(actions: &[Value], cookbook: &[CookbookEntryV2]) -> String {
    let mut prompt = PLAN_COMPILER_SYSTEM_PROMPT_BASE.to_string();

    prompt.push_str("\n## AVAILABLE ACTIONS\n\n");
    prompt.push_str(
        "This is a compact catalog. Before emitting any executor step, call `get_admin_action_help` for that action and use its request_contract/example_scmd as the exact schema.\n\n",
    );
    for action in actions {
        let name = action.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let description = action
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let read_only = action
            .get("read_only")
            .and_then(Value::as_bool)
            .map(|value| if value { "read_only" } else { "mutating" })
            .unwrap_or("unknown");
        prompt.push_str(&format!("### {name}\n{description}\nKind: {read_only}\n\n"));
    }

    if !cookbook.is_empty() {
        prompt.push_str("## COOKBOOK (successful patterns for reference)\n\n");
        for (i, entry) in cookbook.iter().enumerate().take(10) {
            let successful_shape = serde_json::to_string_pretty(&entry.successful_shape)
                .unwrap_or_else(|_| "{}".to_string());
            prompt.push_str(&format!(
                "### Pattern {}\nKey: {}\nTrigger: {}\nSuccessful shape: {}\nConstraints: {}\nDo not repeat: {}\n\n",
                i + 1,
                entry.pattern_key,
                entry.trigger,
                successful_shape,
                if entry.constraints.is_empty() {
                    "(none)".to_string()
                } else {
                    entry.constraints.join(" | ")
                },
                if entry.do_not_repeat.is_empty() {
                    "(none)".to_string()
                } else {
                    entry.do_not_repeat.join(" | ")
                }
            ));
        }
    }

    prompt
}

// ── Designer / DesignAuditor tools (TH-1 / TH-4) ─────────────────────

const DESIGNER_SYSTEM_PROMPT_BASE: &str = r#"You are the designer agent inside SY.architect.

Your job is to produce a valid `solution_manifest` in the Fluxbee rearchitecture format.
You think only in desired state and advisory guidance.
You do NOT produce executor plans, SCMD commands, admin step sequences, or operational procedures.
You do NOT ask questions. You gather context with read-only tools if needed and then call submit_solution_manifest exactly once.

## Manifest rules

- The top-level manifest shape must be:
  - manifest_version: "2.0"
  - solution: { name, description }
  - desired_state
  - advisory
- desired_state may contain only:
  - topology
  - runtimes
  - nodes
  - routing
  - wf_deployments
  - opa_deployments
  - ownership
- Never include policy or identity in desired_state.
- Nodes must use Fluxbee names like AI.name@hive, WF.name@hive, IO.name@hive, SY.name@hive.
- Produce a complete manifest for the requested solution scope, not a patch.

## Ownership rules

Every resource you declare must include an `ownership` field:
- `"ownership": "solution"` — for resources this solution owns. The reconciler will create, update, and delete these to match desired_state.
- `"ownership": "external"` — for pre-existing resources that must not be touched. The reconciler will not delete or modify these.
- Default when omitted: `"external"` (conservative — never use the omission for resources you intend to manage).

## Runtime package_source rules

Every runtime in desired_state.runtimes must declare `package_source`:
- `"package_source": "inline_package"` — the real_programmer agent will generate the package files (package.json, config, prompts). Use this for NEW runtimes being created for this solution.
- `"package_source": "pre_published"` — the runtime already exists in the hive and is materialized. No artifact generation needed. Use ONLY when the runtime is already confirmed to exist.
- `"package_source": "bundle_upload"` — the programmer generates files but the host will zip and upload the bundle. Use when the package is too large for inline generation.

If in doubt whether a runtime exists, use `query_hive(list_runtimes)` to verify. Default to `inline_package` for new runtimes.

## Extending an existing solution

If the input includes a `solution_id`, call `get_manifest_current` FIRST to load the existing manifest. Then extend or modify it — do not create from scratch. All existing sections that are not changing must be preserved verbatim.

## Tool rules

- Use query_hive only for read-only live context.
- Use get_manifest_current when a solution_id is provided in the input.
- Never use mutating tools.
- After reasoning, call submit_solution_manifest exactly once.

## submit_solution_manifest rules

- solution_manifest must be valid JSON and structurally complete.
- human_summary must be one short paragraph for the operator.
- solution_id may be omitted only if the manifest.solution block makes the solution identity clear.
"#;

const DESIGN_AUDITOR_SYSTEM_PROMPT_BASE: &str = r#"You are the design_auditor agent inside SY.architect.

Your job is to review a solution_manifest and produce a structured design_audit_verdict.
You do NOT propose executor steps or admin actions.
You check completeness, internal consistency, ownership clarity, topology feasibility, and unresolved advisory risk.
You call submit_design_audit_verdict exactly once.

## Status semantics

- `pass` — the manifest is complete and consistent. Proceed to execution.
- `revise` — there are fixable issues. The designer will receive your findings and produce a new iteration. Use for: missing sections, wrong field values, name convention violations, consistency gaps, missing ownership markers.
- `reject` — the design is fundamentally invalid and iteration cannot fix it. Use ONLY for: logically impossible requests, unsupported desired_state sections (policy, identity), or requests that require capabilities Fluxbee does not have.

Prefer `revise` over `reject` whenever the designer could plausibly fix the issue.

## Score semantics

Score reflects actual manifest quality from 0 to 10:
- 0–3: serious structural or logical errors
- 4–6: workable skeleton but missing important details
- 7–8: good, minor gaps
- 9–10: complete and well-specified

The loop retries only if score improves by at least 1 point per iteration. Give an honest score — do not inflate it to pass a flawed manifest, and do not deflate it to force extra iterations.

## Verdict rules

- blocking_issues must be short machine-friendly keys: NODES_MISSING, TOPOLOGY_INCOMPLETE, RUNTIME_UNDEFINED, OWNERSHIP_UNCLEAR, INVALID_NODE_NAME, MISSING_PACKAGE_SOURCE, ROUTING_INCOMPLETE, etc.
- findings must reference the specific section being reviewed (e.g., "desired_state.nodes", "desired_state.runtimes[0]")
- summary must be one plain-language paragraph for the operator

## Review focus

- desired_state completeness: topology, runtimes, nodes, routing, workflows, opa as applicable
- every runtime referenced by a node must exist in desired_state.runtimes OR be confirmed present on the hive
- every node must have a valid Fluxbee name (AI.*, WF.*, IO.*, SY.* prefix + @hive)
- every new runtime must have package_source declared (inline_package, pre_published, or bundle_upload)
- ownership must be explicitly set on all resources the solution manages
- advisory notes that signal unresolved deployment risk
"#;

const REAL_PROGRAMMER_SYSTEM_PROMPT_BASE: &str = r#"You are the real_programmer agent inside SY.architect.

Your job is to generate one concrete artifact bundle for exactly one build_task_packet.
You do NOT design topology, compute diffs, choose admin actions, or publish anything.
You must stay inside the artifact task.

## Input

build_task_packet fields you must read:
- `artifact_kind`: what to build (currently only "runtime_package" is supported)
- `target_kind`: "inline_package" or "bundle_upload"
- `known_context.available_runtimes`: list of runtime names available on the hive — use these for runtime_base
- `requirements`: task-specific requirements (e.g. required_files for config_bundle)
- `constraints`: hard constraints that must be respected
- `runtime_name`: suggested package name (use it in package.json)

## Repair packet

If a repair_packet is present, this is a retry after the previous attempt failed the auditor. You MUST:
- Apply every item in `required_corrections` — these are mandatory fixes
- Avoid everything in `do_not_repeat` — these caused the previous failure
- Preserve everything in `must_preserve` — these parts were correct
- Treat a repair_packet as higher priority than any default assumption

## Current supported artifact contract

- artifact_kind = `runtime_package`
- target_kind = `inline_package` | `bundle_upload`

For runtime_package, the artifact.files map must include at minimum:
- `package.json` — JSON string with fields: name (lowercase), version (semver), type, runtime_base
- `config/default-config.json` — JSON string with default node configuration

Optional additional files: `prompts/system.txt` (system prompt for AI nodes), other config files.

`runtime_base` must be one of the values in `build_task_packet.known_context.available_runtimes`.
If `target_kind` is `bundle_upload`, submit the same file map — the host will zip it.

## submit_artifact_bundle fields

- `artifact.files`: the file map (keys = relative paths, values = file content strings)
- `summary`: one sentence describing what was generated
- `assumptions`: list any decisions made where the task was ambiguous (e.g. "assumed tenant_id from context")
- `verification_hints`: things the operator or auditor should check after deployment

## Rules

- Never output admin steps, executor plans, or publish_request payloads.
- Never emit markdown fences.
- Call submit_artifact_bundle exactly once.
"#;

fn build_designer_prompt(cookbook: &[CookbookEntryV2], handbook: Option<&str>) -> String {
    let mut prompt = DESIGNER_SYSTEM_PROMPT_BASE.to_string();
    if let Some(handbook_text) = handbook.filter(|text| !text.trim().is_empty()) {
        prompt.push_str("\n## HANDBOOK\n\n");
        prompt.push_str(handbook_text);
    }
    if !cookbook.is_empty() {
        prompt.push_str("\n## DESIGN COOKBOOK EXAMPLES\n\n");
        for (idx, entry) in cookbook.iter().enumerate().take(8) {
            let entry_json =
                serde_json::to_string_pretty(entry).unwrap_or_else(|_| "{}".to_string());
            prompt.push_str(&format!("Example {}:\n{}\n\n", idx + 1, entry_json));
        }
    }
    prompt
}

fn build_design_auditor_prompt(handbook: Option<&str>) -> String {
    let mut prompt = DESIGN_AUDITOR_SYSTEM_PROMPT_BASE.to_string();
    if let Some(handbook_text) = handbook.filter(|text| !text.trim().is_empty()) {
        prompt.push_str("\n## HANDBOOK\n\n");
        prompt.push_str(handbook_text);
    }
    prompt
}

fn build_real_programmer_prompt(cookbook: &[CookbookEntryV2], handbook: Option<&str>) -> String {
    let mut prompt = REAL_PROGRAMMER_SYSTEM_PROMPT_BASE.to_string();
    if let Some(handbook_text) = handbook.filter(|text| !text.trim().is_empty()) {
        prompt.push_str("\n## HANDBOOK\n\n");
        prompt.push_str(handbook_text);
    }
    if !cookbook.is_empty() {
        prompt.push_str("\n## ARTIFACT COOKBOOK EXAMPLES\n\n");
        for (idx, entry) in cookbook.iter().enumerate().take(8) {
            let entry_json =
                serde_json::to_string_pretty(entry).unwrap_or_else(|_| "{}".to_string());
            prompt.push_str(&format!("Example {}:\n{}\n\n", idx + 1, entry_json));
        }
    }
    prompt
}

fn designer_manifest_section_count(manifest: &SolutionManifestV2) -> usize {
    let desired = &manifest.desired_state;
    let sections = [
        desired.topology.as_ref().map(|_| ()),
        desired.runtimes.as_ref().map(|_| ()),
        desired.nodes.as_ref().map(|_| ()),
        desired.routing.as_ref().map(|_| ()),
        desired.wf_deployments.as_ref().map(|_| ()),
        desired.opa_deployments.as_ref().map(|_| ()),
        desired.ownership.as_ref().map(|_| ()),
    ];
    sections.into_iter().flatten().count()
}

struct StartPipelineTool {
    context: ArchitectAdminToolContext,
}

impl StartPipelineTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

struct DesignerOutput {
    manifest: SolutionManifestV2,
    solution_id: String,
    human_summary: String,
    trace: DesignerTrace,
    tokens_used: u32,
}

#[async_trait]
impl FunctionTool for StartPipelineTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_start_pipeline".to_string(),
            description: "Start the manifest-driven pipeline for broad design intents that cannot be translated directly into an executor plan yet: new topology, multi-resource architecture, or desired-state work that needs Design → Reconcile → PlanCompile. Runs internal designer/auditor agents without a pre-confirmation. For clear targeted mutations, use fluxbee_plan_compiler instead.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["task"],
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Deployment or architecture intent to turn into a reviewed solution manifest."
                    },
                    "solution_id": {
                        "type": "string",
                        "description": "Optional existing solution id to extend."
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional extra operator constraints for the design loop."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let session_id = require_session_id(&self.context, "fluxbee_start_pipeline")?;
        let task = arguments
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_start_pipeline requires 'task'".to_string(),
                )
            })?;
        let solution_id = arguments
            .get("solution_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let operator_context = arguments
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();

        if let Some(existing) =
            latest_nonterminal_pipeline_run_for_session(&self.context, session_id)
                .await
                .map_err(|err| {
                    fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                        "pipeline preflight failed while checking active runs: {err}"
                    ))
                })?
        {
            // Blocked runs need explicit operator action — surface options instead of just erroring.
            if existing.status == PipelineRunStatus::Blocked {
                let state_obj = pipeline_state_from_run(&existing);
                let blocked_reason = state_obj
                    .get("blocked_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let blocked_failure_class = state_obj
                    .get("blocked_failure_class")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Ok(json!({
                    "status": "blocked_run_pending",
                    "pipeline_run_id": existing.pipeline_run_id,
                    "blocked_failure_class": blocked_failure_class,
                    "blocked_reason": blocked_reason,
                    "operator_options": [
                        "discard",
                        "restart_from_design",
                        "retry",
                    ],
                    "message": format!(
                        "There is a blocked pipeline run '{}' in this session that must be resolved first. Call fluxbee_pipeline_action with one of: 'discard' (drop it and start fresh), 'restart_from_design' (close this one and re-design from scratch), or 'retry' (return to the last confirmation checkpoint and re-engage).",
                        existing.pipeline_run_id
                    ),
                }));
            }
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "session already has an active pipeline run '{}' at stage '{}'; resolve it before starting a new pipeline",
                existing.pipeline_run_id, existing.current_stage
            )));
        }

        execute_pipeline_start_with_context(
            &self.context,
            session_id,
            task,
            solution_id.as_deref(),
            &operator_context,
        )
        .await
        .map_err(|err| fluxbee_ai_sdk::AiSdkError::Protocol(err.to_string()))
    }
}

struct PipelineActionTool {
    context: ArchitectAdminToolContext,
}

impl PipelineActionTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl FunctionTool for PipelineActionTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_pipeline_action".to_string(),
            description: "Resolve a blocked pipeline run in the current session. Use after fluxbee_start_pipeline reports a blocked_run_pending, or whenever the operator wants to act on a stuck pipeline. Three actions: 'discard' closes the blocked run and frees the session; 'restart_from_design' closes it and starts a new pipeline with the same task from scratch; 'retry' returns the run to the last confirmation checkpoint (Confirm1 or Confirm2) so the operator can re-engage it with a CONFIRM message.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["action"],
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["discard", "restart_from_design", "retry"],
                        "description": "Operator-chosen resolution for the blocked pipeline."
                    },
                    "pipeline_run_id": {
                        "type": "string",
                        "description": "Optional explicit blocked pipeline_run_id. When omitted, the latest blocked run in this session is used."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let session_id = require_session_id(&self.context, "fluxbee_pipeline_action")?;
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_pipeline_action requires 'action'".to_string(),
                )
            })?;
        let run_id = arguments
            .get("pipeline_run_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());

        apply_pipeline_action_with_context(&self.context, session_id, run_id, action)
            .await
            .map_err(|err| fluxbee_ai_sdk::AiSdkError::Protocol(err.to_string()))
    }
}

async fn execute_pipeline_start_with_context(
    context: &ArchitectAdminToolContext,
    session_id: &str,
    task: &str,
    solution_id: Option<&str>,
    operator_context: &str,
) -> Result<Value, ArchitectError> {
    if let Some(existing) = latest_nonterminal_pipeline_run_for_session(context, session_id).await?
    {
        return Err(format!(
            "session already has an active pipeline run '{}' at stage '{}'; resolve it before starting a new pipeline",
            existing.pipeline_run_id, existing.current_stage
        )
        .into());
    }

    let pipeline_run =
        start_pipeline_run_with_context(context, session_id, solution_id, task).await?;

    let loop_output = match run_design_loop(
        context,
        &pipeline_run,
        task,
        solution_id,
        operator_context,
        0,
    )
    .await
    {
        Ok(output) => output,
        Err(err) => {
            if let Ok(record) = pipeline_run_with_state_update(
                &pipeline_run,
                PipelineStage::Blocked,
                Some(json!({
                    "blocked_reason": format!("design loop failed: {err}"),
                    "blocked_failure_class": FailureClass::DesignIncomplete,
                })),
                None,
            ) {
                if let Err(save_err) = save_pipeline_run_with_context(context, &record).await {
                    tracing::warn!(error = %save_err, pipeline_run_id = %pipeline_run.pipeline_run_id, "failed to persist blocked design-loop run");
                }
            }
            return Err(format!("design loop failed: {err}").into());
        }
    };

    let stage = if loop_output.audit_verdict.status == DesignAuditStatus::Pass {
        PipelineStage::Confirm1
    } else {
        PipelineStage::Blocked
    };
    let message = render_design_blocked_message(&loop_output);
    let state_update = json!({
        "task": task,
        "solution_id": loop_output.solution_id,
        "manifest_ref": format!("manifest://{}/current", loop_output.solution_id),
        "manifest_path": loop_output.manifest_path,
        "designer_human_summary": loop_output.designer_human_summary,
        "designer_traces": loop_output.designer_traces,
        "design_audit_verdict": loop_output.audit_verdict,
        "confirm1_summary": loop_output.confirm1_summary,
        "design_iterations_used": loop_output.iterations_used,
        "design_stop_reason": loop_output.stopped_reason,
        "design_loop_trace": loop_output.trace,
        "design_tokens_used": loop_output.tokens_used,
    });
    let updated = pipeline_run_with_state_update(
        &pipeline_run,
        stage.clone(),
        Some(state_update),
        Some(loop_output.iterations_used),
    )?;
    save_pipeline_run_with_context(context, &updated).await?;

    if stage == PipelineStage::Confirm1 {
        return continue_pipeline_after_design_with_context(context, updated).await;
    }

    Ok(json!({
        "status": "blocked",
        "pipeline_run_id": pipeline_run.pipeline_run_id,
        "stage": stage,
        "solution_id": loop_output.solution_id,
        "iterations_used": loop_output.iterations_used,
        "stopped_reason": loop_output.stopped_reason,
        "designer_traces": loop_output.designer_traces,
        "design_loop_trace": loop_output.trace,
        "confirm1_summary": loop_output.confirm1_summary,
        "design_audit_verdict": loop_output.audit_verdict,
        "message": message,
    }))
}

struct DesignerSubmitManifestTool;

#[async_trait]
impl FunctionTool for DesignerSubmitManifestTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "submit_solution_manifest".to_string(),
            description: "Submit the completed solution manifest. Call this exactly once."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["solution_manifest", "human_summary"],
                "properties": {
                    "solution_id": {
                        "type": "string",
                        "description": "Optional normalized solution id. If omitted, the host derives it from the manifest."
                    },
                    "solution_manifest": {
                        "type": "object",
                        "description": "Complete SolutionManifestV2 JSON.",
                        "required": ["manifest_version", "solution", "desired_state", "advisory"],
                        "properties": {
                            "manifest_version": { "type": "string" },
                            "solution": { "type": "object" },
                            "desired_state": { "type": "object" },
                            "advisory": {}
                        }
                    },
                    "human_summary": {
                        "type": "string",
                        "description": "One short paragraph describing the proposed solution for the operator."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        Ok(arguments)
    }
}

struct DesignAuditorSubmitTool;

#[async_trait]
impl FunctionTool for DesignAuditorSubmitTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "submit_design_audit_verdict".to_string(),
            description: "Submit the completed design audit verdict. Call this exactly once."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["status", "score", "blocking_issues", "findings", "summary"],
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pass", "revise", "reject"]
                    },
                    "score": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 10
                    },
                    "blocking_issues": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "findings": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["code", "section", "message", "severity"],
                            "properties": {
                                "code": { "type": "string" },
                                "section": { "type": "string" },
                                "message": { "type": "string" },
                                "severity": {
                                    "type": "string",
                                    "enum": ["error", "warning"]
                                }
                            }
                        }
                    },
                    "summary": {
                        "type": "string"
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        Ok(arguments)
    }
}

struct RealProgrammerSubmitArtifactTool;

#[async_trait]
impl FunctionTool for RealProgrammerSubmitArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "submit_artifact_bundle".to_string(),
            description: "Submit the generated artifact bundle candidate. Call this exactly once."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["summary", "artifact"],
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Short operator-facing summary of what was generated."
                    },
                    "artifact": {
                        "type": "object",
                        "description": "Artifact content. For runtime_package: must include 'files' map where keys are relative paths and values are file content strings.",
                        "properties": {
                            "files": {
                                "type": "object",
                                "additionalProperties": { "type": "string" },
                                "description": "File map for runtime_package artifacts. Keys = relative paths, values = file content strings."
                            }
                        }
                    },
                    "assumptions": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "verification_hints": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        Ok(arguments)
    }
}

async fn run_designer_with_context(
    context: &ArchitectAdminToolContext,
    task: &str,
    solution_id: Option<&str>,
    user_context: &str,
) -> Result<DesignerOutput, ArchitectError> {
    let runtime = context
        .ai_runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> ArchitectError {
            "AI provider not configured for the designer agent."
                .to_string()
                .into()
        })?;
    let cookbook = read_design_cookbook(&context.state_dir);
    let handbook = read_handbook_text();
    let system_prompt = build_designer_prompt(&cookbook, handbook.as_deref());
    let model = runtime.client.clone().function_model(
        runtime.model.clone(),
        Some(system_prompt),
        runtime.model_settings.clone(),
    );

    let mut tools = FunctionToolRegistry::new();
    tools
        .register(Arc::new(DesignerSubmitManifestTool))
        .map_err(|err| -> ArchitectError {
            format!("designer submit tool register failed: {err}").into()
        })?;
    tools
        .register(Arc::new(PlanCompilerLiveQueryTool {
            context: context.clone(),
        }))
        .map_err(|err| -> ArchitectError {
            format!("designer query_hive tool register failed: {err}").into()
        })?;
    tools
        .register(Arc::new(GetManifestCurrentTool::new(context.clone())))
        .map_err(|err| -> ArchitectError {
            format!("designer get_manifest_current tool register failed: {err}").into()
        })?;

    let input_text = serde_json::to_string_pretty(&json!({
        "task": task,
        "solution_id": solution_id,
        "context": user_context,
        "current_hive": context.hive_id,
    }))
    .unwrap_or_else(|_| task.to_string());

    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let result = runner
        .run_with_input(
            &model,
            &tools,
            FunctionRunInput {
                current_user_message: input_text,
                current_user_parts: None,
                immediate_memory: None,
            },
        )
        .await
        .map_err(|err| -> ArchitectError { format!("designer AI call failed: {err}").into() })?;

    let submitted = result.items.iter().find_map(|item| {
        if let FunctionLoopItem::ToolResult { result: tr } = item {
            if tr.name == "submit_solution_manifest" && !tr.is_error {
                return Some(tr.output.clone());
            }
        }
        None
    });

    let submitted = submitted.ok_or_else(|| -> ArchitectError {
        "designer did not call submit_solution_manifest"
            .to_string()
            .into()
    })?;
    let query_hive_calls = result
        .items
        .iter()
        .filter(|item| matches!(item, FunctionLoopItem::ToolResult { result } if result.name == "query_hive" && !result.is_error))
        .count() as u32;
    let manifest: SolutionManifestV2 =
        serde_json::from_value(submitted.get("solution_manifest").cloned().ok_or_else(
            || -> ArchitectError {
                "submit_solution_manifest missing solution_manifest"
                    .to_string()
                    .into()
            },
        )?)
        .map_err(|err| -> ArchitectError {
            format!("designer returned invalid solution_manifest: {err}").into()
        })?;
    validate_manifest_v2(&manifest).map_err(|err| -> ArchitectError {
        format!("designer returned invalid manifest: {err}").into()
    })?;

    let resolved_solution_id = resolve_manifest_solution_id(
        &manifest,
        submitted
            .get("solution_id")
            .and_then(Value::as_str)
            .or(solution_id),
    );
    let human_summary = submitted
        .get("human_summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Solution manifest ready.")
        .to_string();
    let trace = DesignerTrace {
        task: task.to_string(),
        solution_id: solution_id.map(str::to_string),
        query_hive_calls,
        manifest_version: manifest.manifest_version.clone(),
        section_count: designer_manifest_section_count(&manifest),
        validation_result: "ok".to_string(),
    };

    Ok(DesignerOutput {
        manifest,
        solution_id: resolved_solution_id,
        human_summary,
        trace,
        tokens_used: result.tokens_used,
    })
}

async fn run_design_auditor_with_context(
    context: &ArchitectAdminToolContext,
    manifest: &SolutionManifestV2,
    previous_context: &str,
) -> Result<(DesignAuditVerdict, u32), ArchitectError> {
    let runtime = context
        .ai_runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> ArchitectError {
            "AI provider not configured for the design auditor agent."
                .to_string()
                .into()
        })?;
    let handbook = read_handbook_text();
    let system_prompt = build_design_auditor_prompt(handbook.as_deref());
    let model = runtime.client.clone().function_model(
        runtime.model.clone(),
        Some(system_prompt),
        runtime.model_settings.clone(),
    );

    let mut tools = FunctionToolRegistry::new();
    tools
        .register(Arc::new(DesignAuditorSubmitTool))
        .map_err(|err| -> ArchitectError {
            format!("design auditor submit tool register failed: {err}").into()
        })?;
    tools
        .register(Arc::new(GetManifestCurrentTool::new(context.clone())))
        .map_err(|err| -> ArchitectError {
            format!("design auditor get_manifest_current tool register failed: {err}").into()
        })?;

    let input_text = serde_json::to_string_pretty(&json!({
        "solution_manifest": manifest,
        "previous_audit_context": previous_context,
    }))
    .unwrap_or_else(|_| "audit solution manifest".to_string());

    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let result = runner
        .run_with_input(
            &model,
            &tools,
            FunctionRunInput {
                current_user_message: input_text,
                current_user_parts: None,
                immediate_memory: None,
            },
        )
        .await
        .map_err(|err| -> ArchitectError {
            format!("design auditor AI call failed: {err}").into()
        })?;

    let submitted = result.items.iter().find_map(|item| {
        if let FunctionLoopItem::ToolResult { result: tr } = item {
            if tr.name == "submit_design_audit_verdict" && !tr.is_error {
                return Some(tr.output.clone());
            }
        }
        None
    });

    let submitted = submitted.ok_or_else(|| -> ArchitectError {
        "design auditor did not call submit_design_audit_verdict"
            .to_string()
            .into()
    })?;
    let score =
        submitted
            .get("score")
            .and_then(Value::as_u64)
            .ok_or_else(|| -> ArchitectError {
                "design audit verdict missing numeric score"
                    .to_string()
                    .into()
            })?;
    if score > 10 {
        return Err("design audit score must be between 0 and 10".into());
    }
    let status = match submitted.get("status").and_then(Value::as_str) {
        Some("pass") => DesignAuditStatus::Pass,
        Some("revise") => DesignAuditStatus::Revise,
        Some("reject") => DesignAuditStatus::Reject,
        Some(other) => {
            return Err(format!("unsupported design audit status '{other}'").into());
        }
        None => return Err("design audit verdict missing status".to_string().into()),
    };
    let findings = submitted
        .get("findings")
        .cloned()
        .map(serde_json::from_value::<Vec<DesignFinding>>)
        .transpose()
        .map_err(|err| -> ArchitectError {
            format!("design audit findings invalid: {err}").into()
        })?
        .unwrap_or_default();

    Ok((
        DesignAuditVerdict {
            verdict_id: format!("design-audit-{}", Uuid::new_v4().simple()),
            manifest_version: manifest.manifest_version.clone(),
            status,
            score: score as u8,
            blocking_issues: submitted
                .get("blocking_issues")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            findings,
            summary: submitted
                .get("summary")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("Design audit completed.")
                .to_string(),
            produced_at_ms: now_epoch_ms(),
        },
        result.tokens_used,
    ))
}

async fn run_real_programmer_with_context(
    context: &ArchitectAdminToolContext,
    packet: &BuildTaskPacket,
    repair_packet: Option<&RepairPacket>,
) -> Result<(ArtifactBundle, u32), ArchitectError> {
    let runtime = context
        .ai_runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> ArchitectError {
            "AI provider not configured for the real programmer agent."
                .to_string()
                .into()
        })?;
    let cookbook = read_artifact_cookbook(&context.state_dir);
    let handbook = read_handbook_text();
    let system_prompt = build_real_programmer_prompt(&cookbook, handbook.as_deref());
    let model = runtime.client.clone().function_model(
        runtime.model.clone(),
        Some(system_prompt),
        runtime.model_settings.clone(),
    );

    let mut tools = FunctionToolRegistry::new();
    tools
        .register(Arc::new(RealProgrammerSubmitArtifactTool))
        .map_err(|err| -> ArchitectError {
            format!("real programmer submit tool register failed: {err}").into()
        })?;

    let input_text = serde_json::to_string_pretty(&json!({
        "request": build_real_programmer_request(packet, repair_packet),
        "current_hive": context.hive_id,
    }))
    .unwrap_or_else(|_| "generate artifact bundle".to_string());

    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let result = runner
        .run_with_input(
            &model,
            &tools,
            FunctionRunInput {
                current_user_message: input_text,
                current_user_parts: None,
                immediate_memory: None,
            },
        )
        .await
        .map_err(|err| -> ArchitectError {
            format!("real programmer AI call failed: {err}").into()
        })?;

    let submitted = result.items.iter().find_map(|item| {
        if let FunctionLoopItem::ToolResult { result: tr } = item {
            if tr.name == "submit_artifact_bundle" && !tr.is_error {
                return Some(tr.output.clone());
            }
        }
        None
    });

    let submitted = submitted.ok_or_else(|| -> ArchitectError {
        "real programmer did not call submit_artifact_bundle"
            .to_string()
            .into()
    })?;
    let bundle = artifact_bundle_from_real_programmer_submission(packet, &submitted)
        .map_err(|err| -> ArchitectError { err.into() })?;
    Ok((bundle, result.tokens_used))
}

fn design_feedback_from_verdict(verdict: &DesignAuditVerdict) -> String {
    let mut parts = Vec::new();
    if !verdict.blocking_issues.is_empty() {
        parts.push(format!(
            "blocking issues: {}",
            verdict.blocking_issues.join(", ")
        ));
    }
    let finding_lines = verdict
        .findings
        .iter()
        .take(8)
        .map(|finding| format!("{} [{}] {}", finding.code, finding.section, finding.message))
        .collect::<Vec<_>>();
    if !finding_lines.is_empty() {
        parts.push(format!("findings: {}", finding_lines.join(" | ")));
    }
    if !verdict.summary.trim().is_empty() {
        parts.push(format!("auditor summary: {}", verdict.summary.trim()));
    }
    parts.join("\n")
}

fn blocking_issue_signature(verdict: &DesignAuditVerdict) -> Option<String> {
    if verdict.blocking_issues.is_empty() {
        return None;
    }
    let mut keys = verdict
        .blocking_issues
        .iter()
        .map(|item| item.trim().to_ascii_uppercase())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return None;
    }
    keys.sort();
    keys.dedup();
    Some(keys.join("|"))
}

fn render_design_blocked_message(output: &DesignLoopOutput) -> String {
    let mut lines = vec![
        format!(
            "Pipeline blocked after design loop for {}.",
            output.confirm1_summary.solution_name
        ),
        format!(
            "Stop reason: {:?}. Audit status: {:?} ({}/10).",
            output.stopped_reason, output.audit_verdict.status, output.audit_verdict.score
        ),
    ];
    if !output.audit_verdict.blocking_issues.is_empty() {
        lines.push(format!(
            "Blocking issues: {}",
            output.audit_verdict.blocking_issues.join(", ")
        ));
    }
    if !output.audit_verdict.summary.trim().is_empty() {
        lines.push(output.audit_verdict.summary.trim().to_string());
    }
    lines.join("\n")
}

async fn run_design_loop(
    context: &ArchitectAdminToolContext,
    pipeline_run: &PipelineRunRecord,
    task: &str,
    solution_id: Option<&str>,
    operator_context: &str,
    initial_tokens_used: u32,
) -> Result<DesignLoopOutput, ArchitectError> {
    let mut current_solution_id = solution_id.map(str::to_string);
    let mut feedback = String::new();
    let mut previous_score: Option<u8> = None;
    let mut previous_blocker_signature: Option<String> = None;
    let mut trace = Vec::new();
    let mut designer_traces = Vec::new();
    let mut final_output: Option<DesignLoopOutput> = None;
    let mut token_budget = TaskTokenBudget::new(initial_tokens_used);
    let mut tokens_accumulated: u32 = 0;

    for iteration in 1..=MAX_DESIGN_ITERATIONS {
        let iteration_u32 = iteration as u32;
        let mut designer_context = operator_context.trim().to_string();
        if !feedback.trim().is_empty() {
            if !designer_context.is_empty() {
                designer_context.push_str("\n\n");
            }
            designer_context.push_str("Design revision feedback:\n");
            designer_context.push_str(&feedback);
        }

        token_budget.ensure_room("design.designer")?;

        let designer_output = run_designer_with_context(
            context,
            task,
            current_solution_id.as_deref(),
            &designer_context,
        )
        .await?;
        tokens_accumulated = tokens_accumulated.saturating_add(designer_output.tokens_used);
        token_budget.add("design.designer", designer_output.tokens_used)?;
        current_solution_id = Some(designer_output.solution_id.clone());
        let manifest_path = save_manifest_from_context(
            context,
            &designer_output.solution_id,
            &designer_output.manifest,
        )
        .await?;
        designer_traces.push(designer_output.trace.clone());

        token_budget.ensure_room("design.auditor")?;

        let (verdict, auditor_tokens) =
            run_design_auditor_with_context(context, &designer_output.manifest, &feedback).await?;
        tokens_accumulated = tokens_accumulated.saturating_add(auditor_tokens);
        token_budget.add("design.auditor", auditor_tokens)?;
        let verdict_path = save_design_audit_verdict(
            &context.state_dir,
            &pipeline_run.pipeline_run_id,
            iteration_u32,
            &verdict,
        )?;
        let confirm1_summary = build_confirm1_summary(&designer_output.manifest, &verdict);
        let blocker_signature = blocking_issue_signature(&verdict);
        let stopped_reason = if verdict.status == DesignAuditStatus::Pass {
            Some(DesignLoopStopReason::Passed)
        } else if verdict.status == DesignAuditStatus::Reject {
            Some(DesignLoopStopReason::AuditRejected)
        } else if previous_score
            .map(|score| verdict.score < score.saturating_add(MIN_DESIGN_SCORE_IMPROVEMENT))
            .unwrap_or(false)
        {
            Some(DesignLoopStopReason::NoScoreImprovement)
        } else if blocker_signature.is_some() && blocker_signature == previous_blocker_signature {
            Some(DesignLoopStopReason::RepeatedBlocker)
        } else if iteration == MAX_DESIGN_ITERATIONS {
            Some(DesignLoopStopReason::MaxIterations)
        } else {
            None
        };

        trace.push(DesignLoopTraceEvent {
            iteration: iteration_u32,
            stage: "design_audit".to_string(),
            score: Some(verdict.score),
            status: Some(verdict.status.clone()),
            blocking_issue_count: verdict.blocking_issues.len(),
            stopped_reason: stopped_reason.clone(),
        });

        let output = DesignLoopOutput {
            manifest: designer_output.manifest.clone(),
            solution_id: designer_output.solution_id.clone(),
            manifest_path: manifest_path.to_string_lossy().to_string(),
            designer_human_summary: designer_output.human_summary.clone(),
            designer_traces: designer_traces.clone(),
            audit_verdict: verdict.clone(),
            confirm1_summary,
            iterations_used: iteration_u32,
            stopped_reason: stopped_reason
                .clone()
                .unwrap_or(DesignLoopStopReason::Passed),
            trace: trace.clone(),
            tokens_used: tokens_accumulated,
        };

        save_pipeline_run_with_context(
            context,
            &pipeline_run_with_state_update(
                pipeline_run,
                PipelineStage::DesignAudit,
                Some(json!({
                    "solution_id": output.solution_id,
                    "manifest_ref": format!("manifest://{}/current", output.solution_id),
                    "manifest_path": output.manifest_path,
                    "designer_human_summary": output.designer_human_summary,
                    "design_audit_verdict": output.audit_verdict,
                    "confirm1_summary": output.confirm1_summary,
                    "design_audit_path": verdict_path.to_string_lossy(),
                    "design_feedback": feedback,
                    "design_iterations_used": iteration_u32,
                    "design_loop_trace": output.trace,
                })),
                Some(iteration_u32),
            )?,
        )
        .await?;

        final_output = Some(output.clone());
        if let Some(reason) = stopped_reason {
            return Ok(DesignLoopOutput {
                stopped_reason: reason,
                ..output
            });
        }

        previous_score = Some(verdict.score);
        previous_blocker_signature = blocker_signature;
        feedback = design_feedback_from_verdict(&verdict);
    }

    final_output.ok_or_else(|| "design loop finished without producing output".into())
}

// ── PlanCompilerTool (PROG-T11/T12/T13) ───────────────────────────────

struct PlanCompilerTool {
    context: ArchitectAdminToolContext,
}

impl PlanCompilerTool {
    fn new(context: ArchitectAdminToolContext) -> Self {
        Self { context }
    }
}

#[async_trait]
impl FunctionTool for PlanCompilerTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "fluxbee_plan_compiler".to_string(),
            description: format!(
                "The only executor-plan mutation path available to Archi. Call this to translate a clear deployment \
                or configuration task into an executor_plan that will be executed after operator CONFIRM. \
                Use for clear mutations from one step to many steps: node changes, config updates, restarts, route changes, \
                workflow deployments, runtime publishing, and other admin-backed changes. Use \
                fluxbee_start_pipeline only when the desired state needs manifest/reconcile design work before a plan can exist. Pass either a free-form task description or the \
                canonical delta_report, plus the target hive and any known context. The plan_compiler \
                produces a validated executor_plan and a human-readable summary. After receiving the \
                result, present the human_summary and wait for CONFIRM before execution. \
                Do NOT show the raw JSON plan unless the operator explicitly asks. Hive: {}.",
                self.context.hive_id
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Free-form description of what needs to be deployed or configured. Provide either this field (with hive) OR delta_report — not both."
                    },
                    "hive": {
                        "type": "string",
                        "description": "Primary target hive for the deployment. Required when using free-form task path; optional when delta_report is present."
                    },
                    "delta_report": {
                        "type": "object",
                        "description": "Canonical pipeline input. Deterministic delta_report to translate into executor steps. Provide either this OR task+hive."
                    },
                    "approved_artifacts": {
                        "type": "array",
                        "description": "Optional approved artifact refs/bundles available to the plan compiler when compiling from delta_report.",
                        "items": {
                            "type": "object"
                        }
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional. Any known state: which runtimes are already published, which nodes are already running, constraints the plan_compiler should respect."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let task = arguments
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let delta_report = arguments
            .get("delta_report")
            .cloned()
            .map(|value| {
                serde_json::from_value::<DeltaReport>(value).map_err(|err| {
                    fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                        "fluxbee_plan_compiler invalid delta_report: {err}"
                    ))
                })
            })
            .transpose()?;
        if task.trim().is_empty() && delta_report.is_none() {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(
                "fluxbee_plan_compiler requires either 'task' or 'delta_report'".to_string(),
            ));
        }
        let hive = arguments
            .get("hive")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.context.hive_id)
            .to_string();
        let approved_artifacts = arguments.get("approved_artifacts").cloned();
        let user_context = arguments
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let session_id = self
            .context
            .session_id
            .as_deref()
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "fluxbee_plan_compiler requires a session context".to_string(),
                )
            })?
            .to_string();

        let execution = match run_plan_compiler_transaction(
            &self.context,
            &task,
            &hive,
            &user_context,
            delta_report.as_ref(),
            approved_artifacts.as_ref(),
            0,
        )
        .await
        {
            Ok(o) => o,
            Err(err) => {
                return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                    "plan_compiler agent failed: {err}"
                )))
            }
        };
        let output = execution.output;
        let trace = execution.trace;
        let step_count = trace.step_count;
        let trace_value = plan_compile_trace_to_value(&trace);
        let plan_steps: Vec<Value> = trace_value
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // Capture before trace is moved into the pending plan.
        let tool_result_validation = trace.validation.clone();
        let tool_result_first_error = trace.first_validation_error.clone();
        let tool_result_tokens_used = trace.tokens_used;
        let tool_result_token_budget = trace.token_budget;

        let pending = PlanCompilePending {
            plan: output.plan.clone(),
            cookbook_entry: output.cookbook_entry.clone(),
            trace: Some(trace),
        };
        self.context
            .plan_compile_pending
            .lock()
            .await
            .insert(session_id, pending);

        Ok(json!({
            "status": "plan_ready",
            "human_summary": output.human_summary,
            "step_count": step_count,
            "plan_steps": plan_steps,
            "validation": tool_result_validation,
            "first_validation_error": tool_result_first_error,
            "plan_compile_trace": trace_value,
            "token_usage": {
                "plan_compile": tool_result_tokens_used,
                "total": tool_result_tokens_used,
                "budget": tool_result_token_budget,
            },
        }))
    }
}

struct PlanCompilerOutput {
    plan: Value,
    human_summary: String,
    cookbook_entry: Option<CookbookEntryV2>,
    tokens_used: u32,
}

struct PlanCompilerExecution {
    output: PlanCompilerOutput,
    trace: PlanCompileTrace,
}

fn build_plan_compile_trace(
    task: &str,
    hive: &str,
    plan: &Value,
    first_validation_error: Option<String>,
    tokens_used: u32,
) -> PlanCompileTrace {
    let validation_label = if first_validation_error.is_some() {
        "ok_after_retry"
    } else {
        "ok"
    };
    let plan_steps: Vec<Value> = plan
        .get("execution")
        .and_then(|e| e.get("steps"))
        .and_then(|s| s.as_array())
        .map(|steps| {
            steps
                .iter()
                .map(|step| {
                    let id = step
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let action = step
                        .get("action")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    let args_preview = step
                        .get("args")
                        .map(|args| {
                            serde_json::to_string(args)
                                .unwrap_or_default()
                                .chars()
                                .take(200)
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    json!({
                        "id": id,
                        "action": action,
                        "args_preview": args_preview,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let step_count = plan_steps.len();
    PlanCompileTrace {
        task: task.to_string(),
        hive: hive.to_string(),
        steps: plan_steps
            .iter()
            .map(|s| PlanCompileTraceStep {
                id: s
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                action: s
                    .get("action")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                args_preview: s
                    .get("args_preview")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
            .collect(),
        step_count,
        validation: validation_label.to_string(),
        first_validation_error,
        tokens_used,
        token_budget: TASK_AGENT_TOKEN_BUDGET,
    }
}

fn plan_compile_trace_to_value(trace: &PlanCompileTrace) -> Value {
    json!({
        "task": trace.task,
        "hive": trace.hive,
        "steps": trace.steps.iter().map(|step| json!({
            "id": step.id,
            "action": step.action,
            "args_preview": step.args_preview,
        })).collect::<Vec<_>>(),
        "step_count": trace.step_count,
        "validation": trace.validation,
        "first_validation_error": trace.first_validation_error,
        "tokens_used": trace.tokens_used,
        "token_budget": trace.token_budget,
    })
}

async fn run_plan_compiler_transaction(
    context: &ArchitectAdminToolContext,
    task: &str,
    hive: &str,
    user_context: &str,
    delta_report: Option<&DeltaReport>,
    approved_artifacts: Option<&Value>,
    initial_tokens_used: u32,
) -> Result<PlanCompilerExecution, ArchitectError> {
    let mut token_budget = TaskTokenBudget::new(initial_tokens_used);
    token_budget.ensure_room("plan_compiler.first_attempt")?;
    let first_output = run_plan_compiler_with_context(
        context,
        task,
        hive,
        user_context,
        delta_report,
        approved_artifacts,
    )
    .await?;
    token_budget.add("plan_compiler.first_attempt", first_output.tokens_used)?;

    let validation_error =
        plan_compiler_prevalidate(context, &first_output.plan, delta_report).await;
    let first_validation_error = validation_error.clone();

    let output = if let Some(err) = validation_error {
        token_budget.ensure_room("plan_compiler.retry")?;
        tracing::warn!(error = %err, "plan_compiler plan failed pre-validation — retrying with feedback");
        let feedback_context = format!(
            "{}\n\n[FEEDBACK] Your previous plan was rejected by the validator with this error: {}\nCall get_admin_action_help for the failing action, then call submit_executor_plan with the corrected plan.",
            user_context, err
        );
        let retried = run_plan_compiler_with_context(
            context,
            task,
            hive,
            &feedback_context,
            delta_report,
            approved_artifacts,
        )
        .await?;
        token_budget.add("plan_compiler.retry", retried.tokens_used)?;
        if let Some(retry_err) =
            plan_compiler_prevalidate(context, &retried.plan, delta_report).await
        {
            return Err(format!("plan_compiler retry produced invalid plan: {retry_err}").into());
        }
        PlanCompilerOutput {
            tokens_used: first_output.tokens_used.saturating_add(retried.tokens_used),
            ..retried
        }
    } else {
        first_output
    };

    let trace = build_plan_compile_trace(
        task,
        hive,
        &output.plan,
        first_validation_error,
        output.tokens_used,
    );
    Ok(PlanCompilerExecution { output, trace })
}

async fn run_plan_compiler_with_context(
    context: &ArchitectAdminToolContext,
    task: &str,
    hive: &str,
    user_context: &str,
    delta_report: Option<&DeltaReport>,
    approved_artifacts: Option<&Value>,
) -> Result<PlanCompilerOutput, ArchitectError> {
    let runtime = context
        .ai_runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> ArchitectError {
            "AI provider not configured for the plan_compiler agent."
                .to_string()
                .into()
        })?;

    let actions = get_or_refresh_admin_actions(context)
        .await
        .unwrap_or_default();
    let cookbook = read_plan_compile_cookbook(&context.state_dir);

    let system_prompt = build_plan_compiler_prompt(&actions, &cookbook);

    // Build the submit_executor_plan function schema
    let submit_fn = json!({
        "name": "submit_executor_plan",
        "description": "Submit the executor plan you have built. Call this exactly once.",
        "parameters": {
            "type": "object",
            "properties": {
                "plan": {
                    "type": "object",
                    "description": "The complete executor_plan JSON.",
                    "properties": {
                        "plan_version": { "type": "string" },
                        "kind": { "type": "string" },
                        "metadata": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "target_hive": { "type": "string" }
                            },
                            "required": ["name", "target_hive"]
                        },
                        "execution": {
                            "type": "object",
                            "properties": {
                                "strict": { "type": "boolean" },
                                "stop_on_error": { "type": "boolean" },
                                "allow_help_lookup": { "type": "boolean" },
                                "steps": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "id": { "type": "string" },
                                            "action": { "type": "string" },
                                            "args": { "type": "object" }
                                        },
                                        "required": ["id", "action", "args"]
                                    }
                                }
                            },
                            "required": ["strict", "stop_on_error", "steps"]
                        }
                    },
                    "required": ["plan_version", "kind", "metadata", "execution"]
                },
                "human_summary": {
                    "type": "string",
                    "description": "One paragraph in plain language describing what this plan does. No JSON, no commands. Archi shows this to the user."
                },
                "cookbook_entry": {
                    "type": "object",
                    "description": "Optional. A reusable pattern extracted from this plan. Omit if not reusable.",
                    "properties": {
                        "layer": {
                            "type": "string",
                            "enum": ["plan_compile"]
                        },
                        "pattern_key": { "type": "string" },
                        "trigger": { "type": "string" },
                        "inputs_signature": { "type": "object" },
                        "successful_shape": { "type": "object" },
                        "constraints": { "type": "array", "items": { "type": "string" } },
                        "do_not_repeat": { "type": "array", "items": { "type": "string" } },
                        "failure_class": { "type": ["string", "null"] },
                        "recorded_from_run": { "type": ["string", "null"] },
                        "seed_kind": { "type": "string" }
                    },
                    "required": [
                        "layer",
                        "pattern_key",
                        "trigger",
                        "inputs_signature",
                        "successful_shape",
                        "constraints",
                        "do_not_repeat",
                        "seed_kind"
                    ]
                }
            },
            "required": ["plan", "human_summary"]
        }
    });

    let model = runtime.client.clone().function_model(
        runtime.model.clone(),
        Some(system_prompt),
        runtime.model_settings.clone(),
    );

    let mut tools = FunctionToolRegistry::new();
    tools
        .register(Arc::new(PlanCompilerSubmitTool::new(submit_fn)))
        .map_err(|e| -> ArchitectError { format!("plan_compiler tool register: {e}").into() })?;
    tools
        .register(Arc::new(PlanCompilerHelpTool {
            context: context.clone(),
        }))
        .map_err(|e| -> ArchitectError {
            format!("plan_compiler help tool register: {e}").into()
        })?;
    tools
        .register(Arc::new(PlanCompilerLiveQueryTool {
            context: context.clone(),
        }))
        .map_err(|e| -> ArchitectError {
            format!("plan_compiler query tool register: {e}").into()
        })?;

    let compiler_class_translation = delta_report.map(|report| {
        report
            .operations
            .iter()
            .map(|op| {
                json!({
                    "op_id": op.op_id,
                    "compiler_class": op.compiler_class,
                    "expected_admin_steps": compiler_class_admin_steps(&op.compiler_class),
                    "risk_class": compiler_class_risk(&op.compiler_class),
                    "resource_type": op.resource_type,
                    "resource_id": op.resource_id,
                    "change_type": op.change_type,
                })
            })
            .collect::<Vec<_>>()
    });

    let input_text = serde_json::to_string_pretty(&json!({
        "mode": if delta_report.is_some() { "delta_report" } else { "legacy_task" },
        "task": if task.trim().is_empty() { Value::Null } else { Value::String(task.to_string()) },
        "hive": hive,
        "context": user_context,
        "delta_report": delta_report,
        "approved_artifacts": approved_artifacts.cloned().unwrap_or(Value::Null),
        "compiler_class_translation": compiler_class_translation.unwrap_or_default(),
    }))
    .unwrap_or_else(|_| format!("task: {task}\nhive: {hive}"));

    let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
    let result = runner
        .run_with_input(
            &model,
            &tools,
            FunctionRunInput {
                current_user_message: input_text,
                current_user_parts: None,
                immediate_memory: None,
            },
        )
        .await
        .map_err(|e| -> ArchitectError { format!("plan_compiler AI call failed: {e}").into() })?;

    // Extract the submitted plan from the ToolResult item for submit_executor_plan
    let submitted = result.items.iter().find_map(|item| {
        if let FunctionLoopItem::ToolResult { result: tr } = item {
            if tr.name == "submit_executor_plan" && !tr.is_error {
                return Some(tr.output.clone());
            }
        }
        None
    });

    let submitted = submitted.ok_or_else(|| -> ArchitectError {
        "plan_compiler did not call submit_executor_plan"
            .to_string()
            .into()
    })?;

    let plan = submitted
        .get("plan")
        .cloned()
        .ok_or_else(|| -> ArchitectError {
            "submit_executor_plan missing 'plan'".to_string().into()
        })?;
    let human_summary = submitted
        .get("human_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("Plan ready.")
        .to_string();
    let cookbook_entry = submitted
        .get("cookbook_entry")
        .and_then(parse_plan_compile_cookbook_entry);

    // Validate the plan shape before returning
    validate_architect_executor_plan_shape(&plan)
        .map_err(|e| -> ArchitectError { format!("plan_compiler plan invalid: {e}").into() })?;

    Ok(PlanCompilerOutput {
        plan,
        human_summary,
        cookbook_entry,
        tokens_used: result.tokens_used,
    })
}

// Minimal shim tool that captures the submit_executor_plan call arguments
struct PlanCompilerSubmitTool {
    definition_schema: Value,
}

impl PlanCompilerSubmitTool {
    fn new(schema: Value) -> Self {
        Self {
            definition_schema: schema,
        }
    }
}

#[async_trait]
impl FunctionTool for PlanCompilerSubmitTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "submit_executor_plan".to_string(),
            description: "Submit the completed executor plan.".to_string(),
            parameters_json_schema: self
                .definition_schema
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        Ok(arguments)
    }
}

// Lets the plan_compiler look up the exact required/optional args for any admin action.
struct PlanCompilerHelpTool {
    context: ArchitectAdminToolContext,
}

fn llm_readable_admin_action_help(action_name: &str, raw: &Value) -> Value {
    let entry = raw
        .get("payload")
        .and_then(|payload| payload.get("entry"))
        .or_else(|| raw.get("payload"))
        .unwrap_or(raw);
    let contract = entry
        .get("request_contract")
        .cloned()
        .unwrap_or_else(|| Value::Null);
    let example_scmd = entry
        .get("example_scmd")
        .or_else(|| contract.get("example_scmd"))
        .cloned()
        .unwrap_or(Value::Null);
    let path_patterns = entry
        .get("path_patterns")
        .or_else(|| contract.get("path_patterns"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let summary = entry
        .get("summary")
        .or_else(|| entry.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "action_name": action_name,
        "summary": summary,
        "read_only": entry.get("read_only").cloned().unwrap_or(Value::Null),
        "confirmation_required": entry.get("confirmation_required").cloned().unwrap_or(Value::Null),
        "execution_preference": entry
            .get("execution_preference")
            .or_else(|| contract.get("execution_preference"))
            .cloned()
            .unwrap_or_else(|| json!("example_scmd")),
        "path_patterns_are_templates": true,
        "path_patterns": path_patterns,
        "example_scmd": example_scmd,
        "request_contract": contract,
        "model_instructions": [
            "Use example_scmd as the execution shape.",
            "Use request_contract for exact required and optional argument fields.",
            "Do not put literal {placeholders} from path_patterns into executor args.",
            "Never invent fields that are not present in request_contract."
        ],
    })
}

#[async_trait]
impl FunctionTool for PlanCompilerHelpTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "get_admin_action_help".to_string(),
            description: "Get the full schema, required args, optional args, and example for an admin action. Call this before generating a step for any action you are not fully certain about.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["action_name"],
                "properties": {
                    "action_name": {
                        "type": "string",
                        "description": "The admin action name to look up, e.g. 'run_node'."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let action_name = arguments
            .get("action_name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "get_admin_action_help requires 'action_name'".to_string(),
                )
            })?
            .to_string();

        let raw = execute_admin_action_with_context(
            &self.context,
            &format!("SY.admin@{}", self.context.hive_id),
            "get_admin_action_help",
            None,
            json!({ "action_name": action_name }),
            "plan_compiler.help_lookup",
        )
        .await
        .map_err(|e| fluxbee_ai_sdk::AiSdkError::Protocol(e.to_string()))?;
        Ok(json!({
            "status": "ok",
            "action_name": action_name,
            "help": llm_readable_admin_action_help(&action_name, &raw),
            "raw": raw,
        }))
    }
}

// Lets the plan_compiler query live hive state (read-only) during plan generation,
// so it can make informed decisions (e.g. which runtime exists) without embedding
// read steps in the executor plan.
struct PlanCompilerLiveQueryTool {
    context: ArchitectAdminToolContext,
}

const PROGRAMMER_QUERY_ALLOWED_ACTIONS: &[&str] = &[
    "list_runtimes",
    "get_runtime",
    "inventory",
    "list_nodes",
    "get_node_status",
    "get_node_config",
    "list_routes",
    "hive_status",
    "list_versions",
    "get_versions",
];

#[async_trait]
impl FunctionTool for PlanCompilerLiveQueryTool {
    fn definition(&self) -> FunctionToolDefinition {
        let allowed = PROGRAMMER_QUERY_ALLOWED_ACTIONS.join(", ");
        FunctionToolDefinition {
            name: "query_hive".to_string(),
            description: format!(
                "Run a read-only admin query against the hive to get live state before generating the plan. \
                Use this to check which runtimes are available, which nodes are running, what routes exist, etc. \
                Do NOT use this for mutations — only for reads. Allowed actions: {allowed}."
            ),
            parameters_json_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["action"],
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "The read-only admin action to run."
                    },
                    "hive": {
                        "type": "string",
                        "description": "Target hive. Defaults to the task hive if omitted."
                    },
                    "params": {
                        "type": "object",
                        "description": "Optional params for the action (e.g. {\"runtime\": \"ai.common\"} for get_runtime)."
                    }
                }
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol("query_hive requires 'action'".to_string())
            })?
            .to_string();

        if !PROGRAMMER_QUERY_ALLOWED_ACTIONS.contains(&action.as_str()) {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "query_hive: action '{}' is not allowed — only read-only actions are permitted",
                action
            )));
        }

        let hive = arguments
            .get("hive")
            .and_then(Value::as_str)
            .unwrap_or(&self.context.hive_id)
            .to_string();

        let params = arguments
            .get("params")
            .cloned()
            .unwrap_or_else(|| json!({}));

        execute_admin_action_with_context(
            &self.context,
            &format!("SY.admin@{hive}"),
            &action,
            Some(&hive),
            params,
            "plan_compiler.live_query",
        )
        .await
        .map_err(|e| fluxbee_ai_sdk::AiSdkError::Protocol(e.to_string()))
    }
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
#[cfg(test)]
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

#[cfg(test)]
fn extract_json_candidate(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("```") {
        let stripped = stripped.trim_start_matches(|c| c != '\n');
        let stripped = stripped.strip_prefix('\n').unwrap_or(stripped);
        let stripped = stripped.strip_suffix("```").unwrap_or(stripped);
        return stripped.trim().to_string();
    }

    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return trimmed.to_string();
    }

    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        return trimmed[start..=end].to_string();
    }

    trimmed.to_string()
}

#[cfg(test)]
fn parse_json_value_from_text(raw: &str) -> Result<Value, String> {
    let candidate = extract_json_candidate(raw);
    serde_json::from_str(&candidate).map_err(|err| err.to_string())
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
        admin_actions_cache: Arc::new(Mutex::new(None)),
        plan_compile_pending: Arc::new(Mutex::new(HashMap::new())),
        active_pipeline_runs: Arc::new(Mutex::new(HashMap::new())),
    });

    ensure_chat_storage(&state).await?;
    ensure_plan_compile_cookbook_dir(&state.state_dir);
    ensure_artifact_cookbook_dir(&state.state_dir);
    seed_cookbooks_from_defaults(&state.state_dir);
    mark_interrupted_pipeline_runs(&state).await;

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
        .route("/api/package/publish", any(dynamic_handler))
        .route("/api/attachments", any(dynamic_handler))
        .route("/api/software/upload", any(dynamic_handler))
        .route("/api/software/publish", any(dynamic_handler))
        .route("/api/sessions", any(dynamic_handler))
        .route("/api/sessions/*path", any(dynamic_handler))
        .route("/*path", any(dynamic_handler))
        .layer(DefaultBodyLimit::max(ARCHITECT_MAX_MULTIPART_UPLOAD_BYTES))
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
        instructions: build_archi_prompt(read_handbook_text().as_deref()),
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
        ai_runtime: Arc::clone(&state.ai_runtime),
        session_id: session_id.map(str::to_string),
        admin_actions_cache: Arc::clone(&state.admin_actions_cache),
        plan_compile_pending: Arc::clone(&state.plan_compile_pending),
        active_pipeline_runs: Arc::clone(&state.active_pipeline_runs),
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

fn is_terminal_pipeline_status(status: &PipelineRunStatus) -> bool {
    matches!(
        status,
        PipelineRunStatus::Completed | PipelineRunStatus::Failed
    )
}

async fn latest_nonterminal_pipeline_run_for_session(
    context: &ArchitectAdminToolContext,
    session_id: &str,
) -> Result<Option<PipelineRunRecord>, ArchitectError> {
    if let Some(run) = context
        .active_pipeline_runs
        .lock()
        .await
        .values()
        .filter(|run| run.session_id == session_id && !is_terminal_pipeline_status(&run.status))
        .cloned()
        .max_by_key(|run| (run.updated_at_ms, run.created_at_ms))
    {
        return Ok(Some(run));
    }

    Ok(
        list_pipeline_runs_for_session_by_hive(&context.hive_id, session_id)
            .await?
            .into_iter()
            .rev()
            .find(|run| !is_terminal_pipeline_status(&run.status)),
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
    architect_blob_root_from_config_dir(&state.config_dir)
}

fn architect_blob_root_from_config_dir(config_dir: &Path) -> Result<PathBuf, ArchitectError> {
    let hive = load_hive(config_dir)?;
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

fn detect_software_upload_kind(filename: &str) -> &'static str {
    if filename.trim().to_ascii_lowercase().ends_with(".zip") {
        "bundle_upload"
    } else {
        "binary_full_runtime"
    }
}

fn normalize_software_upload_mime(filename: &str) -> String {
    if detect_software_upload_kind(filename) == "bundle_upload" {
        "application/zip".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

async fn handle_software_upload(
    state: &ArchitectState,
    request: Request,
) -> Result<SoftwareUploadResponse, ArchitectError> {
    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|err| format!("invalid multipart upload: {err}"))?;
    let toolkit = architect_blob_toolkit(state)?;
    let mut uploaded: Option<UploadedAttachment> = None;

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
        if uploaded.is_some() {
            return Err("software upload accepts exactly one file".into());
        }

        let data = field
            .bytes()
            .await
            .map_err(|err| format!("failed to read uploaded file '{filename}': {err}"))?;
        if data.is_empty() {
            return Err(format!("software file '{filename}' is empty").into());
        }
        if data.len() > ARCHITECT_MAX_SOFTWARE_UPLOAD_BYTES {
            return Err(format!(
                "software file '{}' exceeds max size {} bytes",
                filename, ARCHITECT_MAX_SOFTWARE_UPLOAD_BYTES
            )
            .into());
        }

        let mime = normalize_software_upload_mime(&filename);
        let blob_ref = toolkit
            .put_bytes(data.as_ref(), &filename, &mime)
            .and_then(|blob_ref| {
                toolkit.promote(&blob_ref)?;
                Ok(blob_ref)
            })
            .map_err(|err| format!("failed to persist software file '{filename}': {err}"))?;

        uploaded = Some(UploadedAttachment {
            attachment_id: format!("soft_{}", Uuid::new_v4().simple()),
            filename,
            mime,
            size: blob_ref.size,
            blob_ref,
        });
    }

    let Some(attachment) = uploaded else {
        return Err("multipart upload did not contain any file fields".into());
    };
    Ok(SoftwareUploadResponse {
        status: "ok".to_string(),
        upload_kind: detect_software_upload_kind(&attachment.filename).to_string(),
        attachment,
    })
}

fn software_blob_path(state: &ArchitectState, blob_name: &str) -> Result<PathBuf, ArchitectError> {
    let trimmed = blob_name.trim();
    if trimmed.is_empty() {
        return Err("blob_name is required".into());
    }
    BlobToolkit::validate_blob_name(trimmed)
        .map_err(|err| format!("invalid software blob_name '{trimmed}': {err}"))?;
    Ok(architect_blob_root(state)?
        .join("active")
        .join(BlobToolkit::prefix(trimmed))
        .join(trimmed))
}

fn software_blob_rel_path(blob_name: &str) -> Result<String, ArchitectError> {
    let trimmed = blob_name.trim();
    if trimmed.is_empty() {
        return Err("blob_name is required".into());
    }
    BlobToolkit::validate_blob_name(trimmed)
        .map_err(|err| format!("invalid software blob_name '{trimmed}': {err}"))?;
    Ok(format!(
        "active/{}/{}",
        BlobToolkit::prefix(trimmed),
        trimmed
    ))
}

fn load_architect_runtime_manifest() -> Result<Option<RuntimeManifest>, ArchitectError> {
    load_runtime_manifest_from_paths(&[PathBuf::from(DIST_RUNTIME_MANIFEST_PATH)])
        .map_err(|err| err.into())
}

fn architect_runtime_manifest_entry_map(
    manifest: &RuntimeManifest,
) -> Result<BTreeMap<String, RuntimeManifestEntry>, ArchitectError> {
    let runtimes = manifest
        .runtimes
        .as_object()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let mut out = BTreeMap::new();
    for (runtime, value) in runtimes {
        let entry = serde_json::from_value::<RuntimeManifestEntry>(value.clone())
            .map_err(|err| format!("runtime manifest invalid entry for '{runtime}': {err}"))?;
        out.insert(runtime.to_string(), entry);
    }
    Ok(out)
}

fn runtime_case_variants(
    entries: &BTreeMap<String, RuntimeManifestEntry>,
    runtime_name: &str,
) -> Vec<String> {
    entries
        .keys()
        .filter(|name| name.eq_ignore_ascii_case(runtime_name))
        .cloned()
        .collect()
}

fn sanitize_exec_name(raw: &str, runtime_name: &str) -> String {
    let fallback = runtime_name
        .split('.')
        .next_back()
        .unwrap_or("runtime")
        .replace('.', "-");
    let trimmed = raw.trim();
    let candidate = if trimmed.is_empty() {
        fallback
    } else {
        trimmed.to_ascii_lowercase()
    };
    let mut out = String::with_capacity(candidate.len());
    for ch in candidate.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-').trim_matches('.').to_string();
    if out.is_empty() {
        "runtime-bin".to_string()
    } else {
        out
    }
}

fn resolve_runtime_name_from_binary_filename(filename: &str) -> Result<String, ArchitectError> {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("software filename '{}' is missing a usable stem", filename))?;
    let lowered = stem.to_ascii_lowercase();
    let normalized = lowered
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let segments: Vec<&str> = normalized
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect();
    let Some(prefix) = segments.first().copied() else {
        return Err(format!(
            "software filename '{}' does not resolve to a runtime",
            filename
        )
        .into());
    };
    match prefix {
        "ai" => {
            if segments.len() <= 1 || segments.get(1) == Some(&"common") {
                Ok("ai.common".to_string())
            } else {
                Ok(format!("ai.{}", segments[1..].join(".")))
            }
        }
        "wf" => {
            if segments.len() <= 1 || segments.get(1) == Some(&"common") {
                Ok("wf.common".to_string())
            } else {
                Ok(format!("wf.{}", segments[1..].join(".")))
            }
        }
        "io" => {
            if segments.len() <= 1 {
                Err("io executable upload requires a concrete runtime suffix like io-api or io-slack".into())
            } else {
                Ok(format!("io.{}", segments[1..].join(".")))
            }
        }
        _ => Err(format!(
            "software filename '{}' must start with ai, wf, or io",
            filename
        )
        .into()),
    }
}

fn semver_core_triplet(value: &str) -> Option<(u64, u64, u64)> {
    let core = value.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn suggest_binary_runtime_version(current: Option<&str>, now_ms: u64) -> String {
    match current.and_then(semver_core_triplet) {
        Some((major, minor, patch)) => format!("{major}.{minor}.{}-archi.{now_ms}", patch + 1),
        None => "1.0.0".to_string(),
    }
}

fn infer_single_binary_path(package_dir: &Path) -> Result<String, ArchitectError> {
    let bin_dir = package_dir.join("bin");
    if !bin_dir.is_dir() {
        return Err(format!(
            "runtime package '{}' missing bin/ directory",
            package_dir.display()
        )
        .into());
    }
    let mut candidates = Vec::new();
    for entry_res in fs::read_dir(&bin_dir)? {
        let entry = entry_res?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name == "start.sh" {
            continue;
        }
        candidates.push(name.to_string());
    }
    candidates.sort();
    candidates.dedup();
    match candidates.as_slice() {
        [single] => Ok(format!("bin/{single}")),
        [] => Err(format!(
            "current package '{}' does not contain a single binary candidate under bin/",
            package_dir.display()
        )
        .into()),
        _ => Err(format!(
            "current package '{}' has multiple binary candidates under bin/; executable replacement is ambiguous",
            package_dir.display()
        )
        .into()),
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), ArchitectError> {
    fs::create_dir_all(dst)?;
    let permissions = fs::metadata(src)?.permissions();
    fs::set_permissions(dst, permissions)?;
    for entry_res in fs::read_dir(src)? {
        let entry = entry_res?;
        let source_path = entry.path();
        let target_path = dst.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path)?;
            let perms = fs::metadata(&source_path)?.permissions();
            fs::set_permissions(&target_path, perms)?;
        }
    }
    Ok(())
}

fn write_generated_start_script(path: &Path, exec_name: &str) -> Result<(), ArchitectError> {
    let content = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nSCRIPT_DIR=\"$(cd \"$(dirname \"${{BASH_SOURCE[0]}}\")\" && pwd)\"\nexec \"$SCRIPT_DIR/{exec_name}\" \"$@\"\n"
    );
    fs::write(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn write_updated_package_json(
    package_json_path: &Path,
    runtime_name: &str,
    version: &str,
    preserve_existing: bool,
) -> Result<(), ArchitectError> {
    let value = if preserve_existing && package_json_path.exists() {
        let raw = fs::read_to_string(package_json_path)?;
        let mut value: serde_json::Value = serde_json::from_str(&raw)?;
        value["name"] = json!(runtime_name);
        value["version"] = json!(version);
        value["type"] = json!("full_runtime");
        value["runtime_base"] = serde_json::Value::Null;
        value
    } else {
        json!({
            "name": runtime_name,
            "version": version,
            "type": "full_runtime",
            "runtime_base": null
        })
    };
    let bytes = serde_json::to_vec_pretty(&value)?;
    fs::write(package_json_path, bytes)?;
    Ok(())
}

fn add_dir_to_zip(
    writer: &mut zip::ZipWriter<std::io::Cursor<Vec<u8>>>,
    root_dir: &Path,
    current: &Path,
    zip_root: &str,
) -> Result<(), ArchitectError> {
    let mut entries = fs::read_dir(current)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for path in entries {
        let rel = path
            .strip_prefix(root_dir)
            .map_err(|err| format!("failed to compute zip relative path: {err}"))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let zip_path = format!("{zip_root}/{rel_str}");
        if path.is_dir() {
            writer.add_directory(
                format!("{zip_path}/"),
                zip::write::FileOptions::default().unix_permissions(0o755),
            )?;
            add_dir_to_zip(writer, root_dir, &path, zip_root)?;
        } else {
            let bytes = fs::read(&path)?;
            let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
            writer.start_file(
                zip_path,
                zip::write::FileOptions::default().unix_permissions(mode.max(0o644)),
            )?;
            writer.write_all(&bytes)?;
        }
    }
    Ok(())
}

fn build_runtime_bundle_zip(package_dir: &Path, zip_root: &str) -> Result<Vec<u8>, ArchitectError> {
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(cursor);
    writer.add_directory(
        format!("{zip_root}/"),
        zip::write::FileOptions::default().unix_permissions(0o755),
    )?;
    add_dir_to_zip(&mut writer, package_dir, package_dir, zip_root)?;
    let cursor = writer.finish()?;
    Ok(cursor.into_inner())
}

fn resolve_software_publish(
    state: &ArchitectState,
    blob_name: &str,
    filename: Option<&str>,
) -> Result<ResolvedSoftwarePublish, ArchitectError> {
    let uploaded_blob_path = software_blob_path(state, blob_name)?;
    if !uploaded_blob_path.exists() {
        return Err(format!("uploaded software blob not found for '{}'", blob_name).into());
    }
    let uploaded_filename = filename
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| blob_name.to_string());
    let upload_kind = detect_software_upload_kind(&uploaded_filename).to_string();
    if upload_kind == "bundle_upload" {
        return Ok(ResolvedSoftwarePublish {
            resolution: SoftwarePublishResolution {
                upload_kind,
                runtime_name: None,
                operation: "zip_bundle_publish".to_string(),
                package_type: None,
                current_version: None,
                next_version: None,
                executable_path: None,
                summary: "zip bundle -> canonical publish path".to_string(),
            },
            uploaded_blob_path,
            generated_exec_filename: String::new(),
            source_package_dir: None,
        });
    }

    let runtime_name = resolve_runtime_name_from_binary_filename(&uploaded_filename)?;
    let generated_exec_filename = sanitize_exec_name(
        Path::new(&uploaded_filename)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("runtime-bin"),
        &runtime_name,
    );
    let manifest = load_architect_runtime_manifest()?;
    let runtime_entries = manifest
        .as_ref()
        .map(architect_runtime_manifest_entry_map)
        .transpose()?;
    if let Some(entries) = runtime_entries.as_ref() {
        let variants = runtime_case_variants(entries, &runtime_name);
        if variants.len() > 1 {
            return Err(format!(
                "runtime naming conflict for '{}': found case variants [{}]. Executable upload pilot stopped to avoid publishing another duplicate runtime; use ZIP publish or clean up legacy naming first",
                runtime_name,
                variants.join(", ")
            )
            .into());
        }
        if let Some(existing_variant) = variants.first() {
            if existing_variant != &runtime_name {
                return Err(format!(
                    "runtime '{}' exists as legacy-cased '{}'. Executable upload pilot currently publishes lowercase runtime names only and would create a duplicate; use ZIP publish or migrate naming first",
                    runtime_name,
                    existing_variant
                )
                .into());
            }
        }
    }
    let existing_entry = runtime_entries
        .as_ref()
        .and_then(|entries| entries.get(&runtime_name))
        .cloned();

    if let Some(entry) = existing_entry {
        let package_type = entry
            .package_type
            .clone()
            .unwrap_or_else(|| "full_runtime".to_string());
        if package_type != "full_runtime" {
            return Err(format!(
                "runtime '{}' exists but current package type is '{}' (binary upload pilot supports full_runtime only)",
                runtime_name,
                package_type
            )
            .into());
        }
        let current_version = entry.current.clone().ok_or_else(|| {
            format!(
                "runtime '{}' exists but has no current version in manifest",
                runtime_name
            )
        })?;
        let source_package_dir = Path::new(DIST_RUNTIME_ROOT_DIR)
            .join(&runtime_name)
            .join(&current_version);
        if !source_package_dir.is_dir() {
            return Err(format!(
                "runtime '{}' current package dir missing '{}'",
                runtime_name,
                source_package_dir.display()
            )
            .into());
        }
        let executable_path = infer_single_binary_path(&source_package_dir)?;
        let next_version = suggest_binary_runtime_version(Some(&current_version), now_ms());
        return Ok(ResolvedSoftwarePublish {
            resolution: SoftwarePublishResolution {
                upload_kind,
                runtime_name: Some(runtime_name.clone()),
                operation: "existing_runtime_new_version".to_string(),
                package_type: Some(package_type),
                current_version: Some(current_version),
                next_version: Some(next_version),
                executable_path: Some(executable_path),
                summary: format!("existing runtime -> new version ({runtime_name})"),
            },
            uploaded_blob_path,
            generated_exec_filename,
            source_package_dir: Some(source_package_dir),
        });
    }

    Ok(ResolvedSoftwarePublish {
        resolution: SoftwarePublishResolution {
            upload_kind,
            runtime_name: Some(runtime_name.clone()),
            operation: "new_runtime_first_version".to_string(),
            package_type: Some("full_runtime".to_string()),
            current_version: None,
            next_version: Some("1.0.0".to_string()),
            executable_path: Some(format!("bin/{generated_exec_filename}")),
            summary: format!("new runtime -> first version ({runtime_name})"),
        },
        uploaded_blob_path,
        generated_exec_filename,
        source_package_dir: None,
    })
}

async fn handle_software_publish_request(
    state: &ArchitectState,
    req: SoftwarePublishRequest,
) -> Value {
    let resolved = match resolve_software_publish(state, &req.blob_name, req.filename.as_deref()) {
        Ok(resolved) => resolved,
        Err(err) => {
            return json!({
                "status": "error",
                "action": "software_publish",
                "error": err.to_string()
            });
        }
    };

    if resolved.resolution.upload_kind == "bundle_upload" {
        let package_out = handle_package_publish_request(
            state,
            PackagePublishRequest {
                blob_name: req.blob_name,
                set_current: req.set_current,
                preserve_node_config: req.preserve_node_config,
                sync_to: Vec::new(),
                update_to: Vec::new(),
            },
        )
        .await;
        return json!({
            "status": package_out.get("status").and_then(|value| value.as_str()).unwrap_or("error"),
            "action": "software_publish",
            "resolution": resolved.resolution,
            "publish": package_out,
            "redeploy": Value::Null,
        });
    }

    let runtime_name = match resolved.resolution.runtime_name.clone() {
        Some(value) => value,
        None => {
            return json!({
                "status": "error",
                "action": "software_publish",
                "error": "binary publish resolution missing runtime name"
            });
        }
    };
    let runtime_version = match resolved.resolution.next_version.clone() {
        Some(value) => value,
        None => {
            return json!({
                "status": "error",
                "action": "software_publish",
                "error": "binary publish resolution missing next version"
            });
        }
    };
    let executable_path = match resolved.resolution.executable_path.clone() {
        Some(value) => value,
        None => {
            return json!({
                "status": "error",
                "action": "software_publish",
                "error": "binary publish resolution missing executable path"
            });
        }
    };

    let staging_root = state.state_dir.join("software-upload-staging");
    let bundle_root_name = format!(
        "{}-{}",
        runtime_name.replace('.', "-"),
        runtime_version.replace('.', "-")
    );
    let build_dir = staging_root.join(format!("build-{}", Uuid::new_v4().simple()));
    let package_root = build_dir.join(&bundle_root_name);

    let publish_out = (|| -> Result<String, ArchitectError> {
        fs::create_dir_all(&package_root)?;

        if let Some(source_package_dir) = resolved.source_package_dir.as_ref() {
            copy_dir_recursive(source_package_dir, &package_root)?;
        } else {
            fs::create_dir_all(package_root.join("bin"))?;
        }

        write_updated_package_json(
            &package_root.join("package.json"),
            &runtime_name,
            &runtime_version,
            resolved.source_package_dir.is_some(),
        )?;

        let exec_target = package_root.join(&executable_path);
        if let Some(parent) = exec_target.parent() {
            fs::create_dir_all(parent)?;
        }
        let uploaded_bytes = fs::read(&resolved.uploaded_blob_path)?;
        fs::write(&exec_target, uploaded_bytes)?;
        fs::set_permissions(&exec_target, fs::Permissions::from_mode(0o755))?;

        if resolved.source_package_dir.is_none() {
            write_generated_start_script(
                &package_root.join("bin/start.sh"),
                &resolved.generated_exec_filename,
            )?;
        }

        let zip_bytes = build_runtime_bundle_zip(&package_root, &bundle_root_name)?;
        let toolkit = architect_blob_toolkit(state)?;
        let zip_blob = toolkit.put_bytes(
            &zip_bytes,
            &format!("{bundle_root_name}.zip"),
            "application/zip",
        )?;
        toolkit.promote(&zip_blob)?;
        Ok(zip_blob.blob_name)
    })();

    match publish_out {
        Ok(blob_name) => {
            let publish = handle_package_publish_request(
                state,
                PackagePublishRequest {
                    blob_name,
                    set_current: req.set_current,
                    preserve_node_config: req.preserve_node_config,
                    sync_to: Vec::new(),
                    update_to: Vec::new(),
                },
            )
            .await;
            let _ = fs::remove_dir_all(&build_dir);
            json!({
                "status": publish.get("status").and_then(|value| value.as_str()).unwrap_or("error"),
                "action": "software_publish",
                "resolution": resolved.resolution,
                "publish": publish,
                "redeploy": Value::Null,
            })
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&build_dir);
            json!({
                "status": "error",
                "action": "software_publish",
                "resolution": resolved.resolution,
                "error": err.to_string(),
                "redeploy": Value::Null,
            })
        }
    }
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
        (Method::GET, _) if is_session_meta_path(path) => {
            let Some(session_id) = session_meta_id_from_path(path) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "missing session id" })),
                )
                    .into_response();
            };
            match load_chat_session_meta(&state, session_id).await {
                Ok(Some(detail)) => Json(detail).into_response(),
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "session_not_found" })),
                )
                    .into_response(),
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("failed to load session meta: {err}") })),
                )
                    .into_response(),
            }
        }
        (Method::POST, _) if is_session_pipeline_recovery_path(path) => {
            let Some(session_id) = session_pipeline_recovery_id_from_path(path) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "missing session id" })),
                )
                    .into_response();
            };
            let body = match axum::body::to_bytes(request.into_body(), 32 * 1024).await {
                Ok(body) => body,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid body: {err}") })),
                    )
                        .into_response()
                }
            };
            let req = match serde_json::from_slice::<PipelineRecoveryActionRequest>(&body) {
                Ok(req) => req,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid json: {err}") })),
                    )
                        .into_response()
                }
            };
            match apply_pipeline_recovery_action(
                &state,
                session_id,
                &req.action,
                req.pipeline_run_id.as_deref(),
            )
            .await
            {
                Ok(response) => Json(response).into_response(),
                Err(err) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": err.to_string() })),
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
        (Method::POST, _) if is_package_publish_path(path) => {
            let body = match axum::body::to_bytes(request.into_body(), 1024 * 1024).await {
                Ok(b) => b,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid body: {err}") })),
                    )
                        .into_response()
                }
            };
            let req: PackagePublishRequest = match serde_json::from_slice(&body) {
                Ok(r) => r,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid json: {err}") })),
                    )
                        .into_response()
                }
            };
            let out = handle_package_publish_request(&state, req).await;
            Json(out).into_response()
        }
        (Method::POST, _) if is_software_upload_path(path) => {
            match handle_software_upload(&state, request).await {
                Ok(response) => Json(response).into_response(),
                Err(err) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": err.to_string() })),
                )
                    .into_response(),
            }
        }
        (Method::POST, _) if is_software_publish_path(path) => {
            let body = match axum::body::to_bytes(request.into_body(), 1024 * 1024).await {
                Ok(b) => b,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid body: {err}") })),
                    )
                        .into_response()
                }
            };
            let req: SoftwarePublishRequest = match serde_json::from_slice(&body) {
                Ok(r) => r,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("invalid json: {err}") })),
                    )
                        .into_response()
                }
            };
            let out = handle_software_publish_request(&state, req).await;
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
        if let Some(pipeline_run) =
            active_pipeline_at_stage(state, &resolved_session_id, &PipelineStage::Confirm1).await
        {
            handle_pipeline_confirm1(state, &session, pipeline_run).await
        } else if let Some(pipeline_run) =
            active_pipeline_at_stage(state, &resolved_session_id, &PipelineStage::Confirm2).await
        {
            handle_pipeline_confirm2(state, &session, pipeline_run).await
        } else {
            // Check plan_compile_pending before SCMD pending — they are distinct confirmation flows.
            let plan_compile_plan = state
                .plan_compile_pending
                .lock()
                .await
                .remove(&resolved_session_id);
            if let Some(prog_pending) = plan_compile_plan {
                let tool_ctx = admin_tool_context(state, Some(&resolved_session_id));
                let execution_id = uuid::Uuid::new_v4().to_string();
                match execute_executor_plan_with_context(
                    &tool_ctx,
                    execution_id,
                    prog_pending.plan,
                    json!({}),
                    json!({ "origin": "plan_compiler_confirmed" }),
                )
                .await
                {
                    Ok(mut output) => {
                        if let Some(trace) = prog_pending.trace.as_ref() {
                            output["plan_compile_trace"] = plan_compile_trace_to_value(trace);
                            output["token_usage"] = json!({
                                "plan_compile": trace.tokens_used,
                                "total": trace.tokens_used,
                                "budget": trace.token_budget,
                            });
                        }
                        if let Some(entry) = prog_pending.cookbook_entry {
                            append_plan_compile_cookbook_entry(&state.state_dir, entry);
                        }
                        ChatResponse {
                            status: "ok".to_string(),
                            mode: "executor".to_string(),
                            output,
                            session_id: Some(resolved_session_id.clone()),
                            session_title: Some(session.title.clone()),
                        }
                    }
                    Err(err) => {
                        let mut output = json!({ "error": err.to_string() });
                        if let Some(trace) = prog_pending.trace.as_ref() {
                            output["plan_compile_trace"] = plan_compile_trace_to_value(trace);
                            output["token_usage"] = json!({
                                "plan_compile": trace.tokens_used,
                                "total": trace.tokens_used,
                                "budget": trace.token_budget,
                            });
                        }
                        ChatResponse {
                            status: "error".to_string(),
                            mode: "executor".to_string(),
                            output,
                            session_id: Some(resolved_session_id.clone()),
                            session_title: Some(session.title.clone()),
                        }
                    }
                }
            } else {
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
            } // end else (no plan_compile_pending)
        }
    } else if cancellation_requested(trimmed_message) {
        let pipeline_cancel_run = if let Some(run) =
            active_pipeline_at_stage(state, &resolved_session_id, &PipelineStage::Confirm1).await
        {
            Some(run)
        } else {
            active_pipeline_at_stage(state, &resolved_session_id, &PipelineStage::Confirm2).await
        };
        if let Some(pipeline_run) = pipeline_cancel_run {
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                "operator canceled at pipeline confirmation",
                &FailureClass::DesignConflict,
            )
            .await;
            ChatResponse {
                status: "ok".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": "Pipeline canceled at confirmation.",
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                }),
                session_id: Some(resolved_session_id.clone()),
                session_title: Some(session.title.clone()),
            }
        } else {
            let plan_compile_plan_cancel = state
                .plan_compile_pending
                .lock()
                .await
                .remove(&resolved_session_id);
            if plan_compile_plan_cancel.is_some() {
                // plan_compile_pending was present; discard it
                ChatResponse {
                    status: "ok".to_string(),
                    mode: "executor".to_string(),
                    output: json!({ "message": "Programmer plan discarded." }),
                    session_id: Some(resolved_session_id.clone()),
                    session_title: Some(session.title.clone()),
                }
            } else {
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
            }
        } // end else (no plan_compile_pending)
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

async fn handle_package_publish_request(
    state: &ArchitectState,
    req: PackagePublishRequest,
) -> Value {
    let blob_name = req.blob_name.trim().to_string();
    if blob_name.is_empty() {
        return json!({ "status": "error", "error": "blob_name is required" });
    }
    let blob_path = match software_blob_rel_path(&blob_name) {
        Ok(value) => value,
        Err(err) => {
            return json!({
                "status": "error",
                "error": err.to_string(),
            });
        }
    };
    let set_current = req.set_current.unwrap_or(true);
    let preserve_node_config = req.preserve_node_config;
    let translation = AdminTranslation {
        admin_target: format!("SY.admin@{}", state.hive_id),
        action: "publish_runtime_package".to_string(),
        target_hive: state.hive_id.clone(),
        params: json!({
            "source": {
                "kind": "bundle_upload",
                "blob_path": blob_path
            },
            "set_current": set_current,
            "sync_to": req.sync_to,
            "update_to": req.update_to
        }),
    };
    match execute_admin_translation(state, translation).await {
        Ok(output) => {
            let status = output
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("ok")
                .to_string();
            let payload = output.get("payload").cloned().unwrap_or(Value::Null);
            let runtime_name = payload
                .get("runtime_name")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            let redeploy = if set_current && output_status_is_success(Some(status.as_str())) {
                match runtime_name {
                    Some(runtime_name) => match redeploy_runtime_nodes_with_current(
                        state,
                        &state.hive_id,
                        &runtime_name,
                        preserve_node_config,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(err) => json!({
                            "status": "error",
                            "hive": state.hive_id,
                            "runtime": runtime_name,
                            "errors": [err.to_string()],
                        }),
                    },
                    None => Value::Null,
                }
            } else {
                Value::Null
            };
            json!({
                "status": status,
                "action": "publish_runtime_package",
                "payload": payload,
                "redeploy": redeploy,
                "error": output.get("error_detail").or_else(|| output.get("error")).cloned()
            })
        }
        Err(err) => json!({
            "status": "error",
            "action": "publish_runtime_package",
            "error": err.to_string()
        }),
    }
}

fn output_status_is_success(status: Option<&str>) -> bool {
    matches!(status, Some("ok") | Some("sync_pending"))
}

async fn list_nodes_for_hive(
    state: &ArchitectState,
    hive_id: &str,
) -> Result<Vec<Value>, ArchitectError> {
    let translation = AdminTranslation {
        admin_target: format!("SY.admin@{}", state.hive_id),
        action: "list_nodes".to_string(),
        target_hive: hive_id.to_string(),
        params: json!({}),
    };
    let output = execute_admin_translation(state, translation).await?;
    let nodes = output
        .get("payload")
        .and_then(|value| value.get("nodes"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(nodes)
}

async fn get_node_config_for_hive(
    state: &ArchitectState,
    hive_id: &str,
    node_name: &str,
) -> Result<Value, ArchitectError> {
    let translation = AdminTranslation {
        admin_target: format!("SY.admin@{}", state.hive_id),
        action: "get_node_config".to_string(),
        target_hive: hive_id.to_string(),
        params: json!({ "node_name": node_name }),
    };
    execute_admin_translation(state, translation).await
}

fn sanitized_redeploy_config(config: &Value) -> Value {
    let Some(obj) = config.as_object() else {
        return json!({});
    };
    let mut sanitized = obj.clone();
    sanitized.remove("_system");
    Value::Object(sanitized)
}

fn runtime_name_from_node_config_output(output: &Value) -> Option<&str> {
    output
        .get("payload")
        .and_then(|value| value.get("config"))
        .and_then(|value| value.get("_system"))
        .and_then(|value| value.get("runtime"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn node_config_from_output(output: &Value) -> Option<Value> {
    output
        .get("payload")
        .and_then(|value| value.get("config"))
        .cloned()
        .filter(Value::is_object)
}

async fn redeploy_runtime_nodes_with_current(
    state: &ArchitectState,
    hive_id: &str,
    runtime_name: &str,
    preserve_node_config: bool,
) -> Result<Value, ArchitectError> {
    let nodes = list_nodes_for_hive(state, hive_id).await?;

    let mut recreated = Vec::new();
    let mut errors = Vec::new();

    for node in nodes {
        let node_name = match node.get("node_name").and_then(|value| value.as_str()) {
            Some(value) if !value.trim().is_empty() => value.to_string(),
            _ => continue,
        };
        let mut config_output: Option<Value> = None;
        let resolved_runtime_name = match node
            .get("runtime")
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => Some(value.to_string()),
            None => match get_node_config_for_hive(state, hive_id, &node_name).await {
                Ok(output) => {
                    let runtime = runtime_name_from_node_config_output(&output).map(str::to_string);
                    if runtime.is_some() {
                        config_output = Some(output);
                    }
                    runtime
                }
                Err(_) => None,
            },
        };
        if resolved_runtime_name.as_deref() != Some(runtime_name) {
            continue;
        }
        let config = if preserve_node_config {
            let output = match config_output.take() {
                Some(output) => Ok(output),
                None => get_node_config_for_hive(state, hive_id, &node_name).await,
            };
            match output {
                Ok(output) => {
                    let config = node_config_from_output(&output).unwrap_or(Value::Null);
                    if !config.is_object() {
                        errors.push(json!({
                            "node_name": node_name,
                            "action": "get_node_config",
                            "status": "error",
                            "error": "stored node config is missing or invalid",
                        }));
                        continue;
                    }
                    Some(sanitized_redeploy_config(&config))
                }
                Err(err) => {
                    errors.push(json!({
                        "node_name": node_name,
                        "action": "get_node_config",
                        "status": "error",
                        "error": err.to_string(),
                    }));
                    continue;
                }
            }
        } else {
            None
        };

        let kill_translation = AdminTranslation {
            admin_target: format!("SY.admin@{}", state.hive_id),
            action: "kill_node".to_string(),
            target_hive: hive_id.to_string(),
            params: json!({
                "node_name": node_name,
                "force": true,
                "purge_instance": true,
            }),
        };
        let kill_output = match execute_admin_translation(state, kill_translation).await {
            Ok(output) => output,
            Err(err) => {
                errors.push(json!({
                    "node_name": node_name,
                    "action": "kill_node",
                    "status": "error",
                    "error": err.to_string(),
                }));
                continue;
            }
        };
        let kill_status = kill_output
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("error");
        if !output_status_is_success(Some(kill_status)) {
            errors.push(json!({
                "node_name": node_name,
                "action": "kill_node",
                "status": kill_status,
                "error": kill_output.get("error_detail").or_else(|| kill_output.get("error")).cloned().unwrap_or(Value::Null),
            }));
            continue;
        }

        let mut run_params = json!({
            "node_name": node_name,
            "runtime_version": "current",
        });
        if let Some(config) = config {
            if let Some(obj) = run_params.as_object_mut() {
                obj.insert("config".to_string(), config);
            }
        }
        let run_translation = AdminTranslation {
            admin_target: format!("SY.admin@{}", state.hive_id),
            action: "run_node".to_string(),
            target_hive: hive_id.to_string(),
            params: run_params,
        };
        match execute_admin_translation(state, run_translation).await {
            Ok(output) => {
                let step_status = output
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("error");
                if output_status_is_success(Some(step_status)) {
                    recreated.push(json!({
                        "node_name": node_name,
                        "status": step_status,
                        "config_mode": if preserve_node_config { "preserved" } else { "template_default" },
                    }));
                } else {
                    errors.push(json!({
                        "node_name": node_name,
                        "action": "run_node",
                        "status": step_status,
                        "error": output.get("error_detail").or_else(|| output.get("error")).cloned().unwrap_or(Value::Null),
                    }));
                }
            }
            Err(err) => {
                errors.push(json!({
                    "node_name": node_name,
                    "action": "run_node",
                    "status": "error",
                    "error": err.to_string(),
                }));
            }
        }
    }

    Ok(json!({
        "hive": hive_id,
        "runtime": runtime_name,
        "matched_nodes": recreated.len() + errors.len(),
        "preserve_node_config": preserve_node_config,
        "recreated": recreated,
        "errors": errors,
        "status": if errors.is_empty() { "ok" } else { "error" },
    }))
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

    // If the plan_compiler stored a pending plan this turn, inject its trace into the response
    // so the frontend can render the full agent handshake (task → plan → validation).
    let plan_compile_trace: Option<Value> = {
        let guard = state.plan_compile_pending.lock().await;
        guard.get(&session.session_id).and_then(|pending| {
            pending
                .trace
                .as_ref()
                .map(|trace| plan_compile_trace_to_value(trace))
        })
    };

    Ok(json!({
        "message": message,
        "provider": "openai",
        "model": runtime.model,
        "tool_results": tool_results,
        "plan_compile_trace": plan_compile_trace,
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
    let recent_interactions = recent_messages_to_immediate_with_blobs(state, recent_messages).await;

    Ok(ImmediateConversationMemory {
        thread_id: none_if_empty(&session.thread_id),
        scope_id: session_scope_id(session),
        summary: Some(summary),
        recent_interactions,
        active_operations: operations_to_immediate(&session.session_id, operations),
    })
}

async fn recent_messages_to_immediate_with_blobs(
    state: &ArchitectState,
    messages: Vec<PersistedChatMessage>,
) -> Vec<ImmediateInteraction> {
    let mut pairs: Vec<(PersistedChatMessage, ImmediateInteraction)> = messages
        .into_iter()
        .filter_map(|msg| {
            immediate_interaction_from_message(&msg).map(|interaction| (msg, interaction))
        })
        .collect();
    let start = pairs.len().saturating_sub(10);
    pairs.drain(0..start);

    let mut attachment_budget = 3usize;
    for (msg, interaction) in pairs.iter_mut().rev() {
        if attachment_budget == 0 {
            break;
        }
        if interaction.role != ImmediateRole::User {
            continue;
        }
        let attachments = persisted_message_attachments(msg);
        if attachments.is_empty() {
            continue;
        }
        if let Ok(enriched) =
            enrich_interaction_with_blob_content(state, &interaction.content, &attachments).await
        {
            interaction.content = enriched;
        }
        attachment_budget -= 1;
    }

    pairs
        .into_iter()
        .map(|(_, interaction)| interaction)
        .collect()
}

async fn enrich_interaction_with_blob_content(
    state: &ArchitectState,
    base_content: &str,
    attachments: &[UploadedAttachment],
) -> Result<String, ArchitectError> {
    const MAX_ATTACHMENT_CHARS: usize = 6000;
    let toolkit = architect_blob_toolkit(state)?;
    let mut parts = Vec::new();
    if !base_content.trim().is_empty() {
        parts.push(base_content.to_string());
    }
    for attachment in attachments {
        let path = toolkit.resolve(&attachment.blob_ref);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(_) => {
                parts.push(format!(
                    "[Attachment: {} — not available in blob store]",
                    attachment.filename
                ));
                continue;
            }
        };
        let is_text = attachment.mime.starts_with("text/")
            || matches!(
                attachment.mime.as_str(),
                "application/json"
                    | "application/xml"
                    | "application/javascript"
                    | "application/x-yaml"
                    | "application/x-toml"
            )
            || (!attachment.mime.starts_with("image/")
                && !attachment.mime.starts_with("audio/")
                && !attachment.mime.starts_with("video/")
                && bytes.iter().take(512).all(|&b| b != 0));
        if is_text {
            let text = String::from_utf8_lossy(&bytes);
            let char_count = text.chars().count();
            let (body, truncated) = if char_count > MAX_ATTACHMENT_CHARS {
                let s: String = text.chars().take(MAX_ATTACHMENT_CHARS).collect();
                (s, true)
            } else {
                (text.into_owned(), false)
            };
            let label = if truncated {
                format!(
                    "[Attachment: {} ({} bytes, showing first {} chars)]",
                    attachment.filename, attachment.size, MAX_ATTACHMENT_CHARS
                )
            } else {
                format!("[Attachment: {}]", attachment.filename)
            };
            parts.push(format!("{label}\n{body}"));
        } else {
            parts.push(format!(
                "[Attachment: {} ({}, {} bytes) — binary content not included in context]",
                attachment.filename, attachment.mime, attachment.size
            ));
        }
    }
    Ok(parts.join("\n\n"))
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
                "SCMD: curl -X GET /hives/motherbee/runtimes/ai.chat",
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
            | "publish_runtime_package"
            | "add_route"
            | "delete_route"
            | "add_vpn"
            | "delete_vpn"
            | "run_node"
            | "start_node"
            | "restart_node"
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
        "executor_validate_plan" | "executor_execute_plan" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_EXECUTOR_TIMEOUT_SECS").unwrap_or(120))
        }
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
    let execution =
        obj.get("execution")
            .and_then(Value::as_object)
            .ok_or_else(|| -> ArchitectError {
                "executor plan requires object field 'execution'".into()
            })?;
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

fn validate_plan_against_delta(plan: &Value, delta: &DeltaReport) -> Result<(), ArchitectError> {
    if delta.status != DeltaReportStatus::Ready {
        return Err(format!(
            "delta_report status must be ready before plan compilation, got '{}'",
            serde_json::to_value(&delta.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{:?}", delta.status))
        )
        .into());
    }

    let steps = plan
        .get("execution")
        .and_then(|value| value.get("steps"))
        .and_then(Value::as_array)
        .ok_or_else(|| -> ArchitectError {
            "executor plan execution.steps must be an array".into()
        })?;
    let actual_actions = steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            step.get("action")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| -> ArchitectError {
                    format!(
                        "executor plan step {} is missing string field 'action'",
                        index + 1
                    )
                    .into()
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut expected_actions = Vec::new();
    for op in &delta.operations {
        if op.change_type == ChangeType::Noop {
            continue;
        }
        let Some(steps) = compiler_class_admin_steps(&op.compiler_class) else {
            return Err(format!(
                "delta op '{}' with compiler_class '{:?}' has no executable translation",
                op.op_id, op.compiler_class
            )
            .into());
        };
        expected_actions.extend(steps.into_iter().map(str::to_string));
    }

    if actual_actions.len() != expected_actions.len() {
        return Err(format!(
            "executor plan step count {} does not match expected {} derived from delta_report",
            actual_actions.len(),
            expected_actions.len()
        )
        .into());
    }

    for (index, (actual, expected)) in actual_actions
        .iter()
        .zip(expected_actions.iter())
        .enumerate()
    {
        if actual != expected {
            return Err(format!(
                "executor plan step {} action '{}' does not match expected '{}' from delta_report",
                index + 1,
                actual,
                expected
            )
            .into());
        }
    }

    Ok(())
}

async fn plan_compiler_prevalidate(
    context: &ArchitectAdminToolContext,
    plan: &Value,
    delta_report: Option<&DeltaReport>,
) -> Option<String> {
    if let Some(delta) = delta_report {
        if let Err(err) = validate_plan_against_delta(plan, delta) {
            return Some(format!("delta validation failed: {err}"));
        }
    }

    let admin_target = format!("SY.admin@{}", context.hive_id);
    match execute_admin_action_with_context(
        context,
        &admin_target,
        "executor_validate_plan",
        None,
        plan.clone(),
        "plan_compiler.pre_validate",
    )
    .await
    {
        Err(e) => Some(e.to_string()),
        Ok(val) => {
            if val.get("status").and_then(Value::as_str) != Some("ok") {
                Some(
                    val.get("error_detail")
                        .and_then(Value::as_str)
                        .or_else(|| val.get("error_code").and_then(Value::as_str))
                        .unwrap_or("executor validation failed")
                        .to_string(),
                )
            } else {
                None
            }
        }
    }
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
        ("POST", ["admin", "runtime-packages", "publish"]) => {
            let params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for publish_runtime_package must be a JSON object".into());
            }
            Ok(AdminTranslation {
                admin_target,
                action: "publish_runtime_package".to_string(),
                target_hive: local_hive_id.to_string(),
                params,
            })
        }
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
        ("POST", ["hives", hive_id, "nodes", node_name, "start"]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for start_node must be a JSON object".into());
            }
            params["node_name"] = json!(node_name);
            Ok(AdminTranslation {
                admin_target,
                action: "start_node".to_string(),
                target_hive: (*hive_id).to_string(),
                params,
            })
        }
        ("POST", ["hives", hive_id, "nodes", node_name, "restart"]) => {
            let mut params = parsed.body.unwrap_or_else(|| json!({}));
            if !params.is_object() {
                return Err("SCMD body for restart_node must be a JSON object".into());
            }
            params["node_name"] = json!(node_name);
            Ok(AdminTranslation {
                admin_target,
                action: "restart_node".to_string(),
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
            response_detail_text(&validate_output)
                .unwrap_or_else(|| "unknown validation error".to_string())
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
    let params_json = serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_string());
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
    let response = admin_command(
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
    .await;
    let _ = sender.close().await;
    let out = response.map_err(|err| -> ArchitectError {
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

    let summary_response = admin_command(
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
    .await;
    let summary = match summary_response {
        Ok(summary) => summary,
        Err(err) => {
            let _ = sender.close().await;
            return Err(Box::new(err));
        }
    };
    ensure_admin_ok("inventory summary", &summary)?;

    let hive_response = admin_command(
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
    let _ = sender.close().await;
    let hive_payload = match hive_response {
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
    open_architect_db_for_hive(&state.hive_id).await
}

async fn open_architect_db_for_hive(hive_id: &str) -> Result<Connection, ArchitectError> {
    let path = architect_db_path_for_hive(hive_id);
    fs::create_dir_all(&path)?;
    lancedb::connect(path.to_string_lossy().as_ref())
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })
}

fn architect_db_path_for_hive(hive_id: &str) -> PathBuf {
    architect_node_dir(hive_id).join("architect.lance")
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
        pipeline_recovery: None,
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
    let pipeline_recovery = latest_interrupted_pipeline_for_session(state, session_id).await?;
    Ok(Some(SessionDetailResponse {
        session: session_record_to_summary(&session),
        messages: persisted,
        pipeline_recovery,
    }))
}

fn session_revision_string(message_count: u64, last_activity_at_ms: u64) -> String {
    format!("{message_count}:{last_activity_at_ms}")
}

async fn load_chat_session_meta(
    state: &ArchitectState,
    session_id: &str,
) -> Result<Option<SessionMetaResponse>, ArchitectError> {
    let _guard = state.chat_lock.lock().await;
    let db = open_architect_db(state).await?;
    let sessions = ensure_sessions_table(&db).await?;
    let filter = format!("session_id = '{}'", session_id.replace('\'', "''"));
    let batches = sessions
        .query()
        .only_if(filter)
        .select(Select::columns(&[
            "session_id",
            "last_activity_at_ms",
            "message_count",
        ]))
        .execute()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|err| -> ArchitectError { Box::new(err) })?;
    for batch in &batches {
        let session_ids = string_column(batch, "session_id")?;
        let updated = u64_column(batch, "last_activity_at_ms")?;
        let counts = u64_column(batch, "message_count")?;
        if batch.num_rows() == 0 {
            continue;
        }
        let session_id = session_ids.value(0).to_string();
        let last_activity_at_ms = updated.value(0);
        let message_count = counts.value(0);
        let pipeline_recovery = latest_interrupted_pipeline_for_session(state, &session_id).await?;
        return Ok(Some(SessionMetaResponse {
            revision: session_revision_string(message_count, last_activity_at_ms),
            session_id,
            message_count,
            last_activity_at_ms,
            pipeline_recovery,
        }));
    }
    Ok(None)
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
        let now = now_epoch_ms();
        let record = ChatSessionRecord {
            session_id: session_id.clone(),
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
        return Ok((session_id, record));
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
        || path.starts_with("/api/session-meta/")
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

fn is_package_publish_path(path: &str) -> bool {
    path == "/api/package/publish" || path.ends_with("/api/package/publish")
}

fn is_software_upload_path(path: &str) -> bool {
    path == "/api/software/upload" || path.ends_with("/api/software/upload")
}

fn is_software_publish_path(path: &str) -> bool {
    path == "/api/software/publish" || path.ends_with("/api/software/publish")
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

fn is_session_meta_path(path: &str) -> bool {
    path.starts_with("/api/session-meta/") && session_meta_id_from_path(path).is_some()
}

fn is_session_pipeline_recovery_path(path: &str) -> bool {
    path.starts_with("/api/sessions/")
        && path.ends_with("/pipeline-recovery")
        && session_pipeline_recovery_id_from_path(path).is_some()
}

fn is_session_detail_path(path: &str) -> bool {
    path.starts_with("/api/sessions/")
        && !path.ends_with("/meta")
        && !path.ends_with("/pipeline-recovery")
        && session_id_from_path(path).is_some()
}

fn session_id_from_path(path: &str) -> Option<&str> {
    path.split("/api/sessions/")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn session_meta_id_from_path(path: &str) -> Option<&str> {
    path.split("/api/session-meta/")
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn session_pipeline_recovery_id_from_path(path: &str) -> Option<&str> {
    path.strip_prefix("/api/sessions/")
        .and_then(|value| value.strip_suffix("/pipeline-recovery"))
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
      gap: 5px;
      margin-top: 5px;
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
      gap: 7px;
    }}
    .history-card {{
      position: relative;
      padding: 10px 12px 9px;
      border-radius: 14px;
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
      font-size: 0.87rem;
      font-weight: 700;
      margin-bottom: 2px;
      max-width: 100%;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }}
    .history-meta,
    .meta-grid {{
      color: var(--muted);
      font-size: 0.76rem;
      line-height: 1.45;
    }}
    .history-meta {{
      display: flex;
      flex-wrap: wrap;
      align-items: center;
      gap: 4px;
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
      margin-top: 5px;
      color: var(--text);
      font-size: 0.78rem;
      line-height: 1.4;
      max-width: 100%;
      display: -webkit-box;
      -webkit-line-clamp: 1;
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
    .publish-panel {{
      margin-top: 14px;
      padding-top: 14px;
      border-top: 1px solid var(--line);
      display: grid;
      gap: 8px;
    }}
    .publish-panel-head {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
    }}
    .publish-panel-title {{
      font-size: 0.76rem;
      font-weight: 800;
      letter-spacing: 0.1em;
      text-transform: uppercase;
      color: var(--muted);
    }}
    .publish-drop {{
      border: 1.5px dashed var(--line);
      border-radius: 12px;
      padding: 12px 10px;
      text-align: center;
      cursor: pointer;
      transition: border-color 120ms, background 120ms;
      background: #fbfcfe;
    }}
    .publish-drop:hover, .publish-drop.dragover {{
      border-color: var(--accent);
      background: #f0f5ff;
    }}
    .publish-drop-label {{
      font-size: 0.80rem;
      color: var(--muted);
      pointer-events: none;
    }}
    .publish-drop-link {{
      color: var(--accent);
      font-weight: 700;
    }}
    .publish-filename {{
      font-size: 0.78rem;
      font-weight: 700;
      color: var(--text);
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }}
    .publish-options {{
      display: grid;
      gap: 7px;
    }}
    .publish-label {{
      display: flex;
      align-items: center;
      gap: 6px;
      font-size: 0.80rem;
      color: var(--text);
      cursor: pointer;
    }}
    .publish-input {{
      width: 100%;
      border: 1px solid var(--line);
      border-radius: 10px;
      padding: 7px 10px;
      font: inherit;
      font-size: 0.80rem;
      color: var(--text);
      background: #fff;
      outline: none;
      box-sizing: border-box;
    }}
    .publish-input:focus {{
      border-color: #b8caef;
      box-shadow: 0 0 0 3px rgba(69,117,220,0.10);
    }}
    .publish-btn {{
      width: 100%;
      padding: 8px 12px;
      border-radius: 10px;
      border: none;
      background: var(--accent);
      color: #fff;
      font: inherit;
      font-size: 0.82rem;
      font-weight: 700;
      cursor: pointer;
      transition: opacity 120ms;
    }}
    .publish-btn:disabled {{
      opacity: 0.5;
      cursor: not-allowed;
    }}
    .publish-btn:hover:not(:disabled) {{
      opacity: 0.88;
    }}
    .publish-result {{
      font-size: 0.78rem;
      line-height: 1.45;
      border-radius: 10px;
      padding: 8px 10px;
      display: none;
      white-space: pre-line;
    }}
    .publish-result.progress {{
      background: var(--panel-soft);
      color: var(--muted);
      border: 1px solid var(--line);
      display: block;
    }}
    .publish-result.ok {{
      background: var(--success-soft);
      color: var(--success);
      display: block;
    }}
    .publish-result.err {{
      background: var(--warning-soft);
      color: var(--warning);
      display: block;
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
    .recovery-shell {{
      display: grid;
      gap: 10px;
    }}
    .recovery-copy {{
      color: var(--text);
      line-height: 1.5;
      white-space: normal;
    }}
    .recovery-actions {{
      display: flex;
      gap: 10px;
      flex-wrap: wrap;
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
    .exec-head-right {{
      display: flex;
      align-items: center;
      gap: 8px;
      flex-wrap: wrap;
      justify-content: flex-end;
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
    .exec-token-chip {{
      font-size: 0.72rem;
      font-weight: 800;
      border-radius: 999px;
      padding: 6px 10px;
      letter-spacing: 0.03em;
      background: #eef4ff;
      color: #1d4ed8;
      text-transform: uppercase;
      white-space: nowrap;
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
    /* ── agent trace panel ── */
    .agent-trace {{
      border: 1px solid var(--line);
      border-radius: 14px;
      overflow: hidden;
      margin-bottom: 2px;
    }}
    .agent-trace-toggle {{
      list-style: none;
      display: flex;
      align-items: center;
      gap: 8px;
      padding: 9px 13px;
      cursor: pointer;
      user-select: none;
      font-size: 0.82rem;
      font-weight: 700;
      color: var(--muted);
    }}
    .agent-trace-toggle::-webkit-details-marker {{ display: none; }}
    .agent-trace-toggle:hover {{ color: var(--text); }}
    .agent-trace-label {{ flex: 1; }}
    .agent-trace-badge {{
      font-size: 0.70rem;
      font-weight: 700;
      border-radius: 999px;
      padding: 2px 8px;
      text-transform: uppercase;
      letter-spacing: 0.04em;
    }}
    .agent-trace-badge.ok {{ background: var(--success-soft); color: var(--success); }}
    .agent-trace-badge.retry {{ background: var(--warning-soft); color: var(--warning); }}
    .agent-trace-body {{
      border-top: 1px solid var(--line);
      padding: 10px 13px;
      display: grid;
      gap: 8px;
    }}
    .agent-trace-section {{
      display: grid;
      gap: 4px;
    }}
    .agent-trace-section-label {{
      font-size: 0.72rem;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: 0.06em;
      color: var(--muted);
    }}
    .agent-trace-row {{
      display: flex;
      align-items: baseline;
      gap: 8px;
      padding: 5px 8px;
      border-radius: 8px;
      background: #f8f9fb;
      font-size: 0.82rem;
    }}
    .agent-trace-row-id {{
      font-family: monospace;
      font-size: 0.72rem;
      color: var(--muted);
      flex-shrink: 0;
      width: 28px;
    }}
    .agent-trace-row-action {{
      font-weight: 700;
      color: var(--text);
      flex-shrink: 0;
      min-width: 140px;
    }}
    .agent-trace-row-args {{
      font-family: monospace;
      font-size: 0.73rem;
      color: var(--muted);
      overflow: hidden;
      white-space: nowrap;
      text-overflow: ellipsis;
    }}
    .agent-trace-error {{
      font-size: 0.78rem;
      color: var(--warning);
      background: var(--warning-soft);
      border-radius: 8px;
      padding: 6px 10px;
      line-height: 1.45;
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
        <div class="publish-panel">
          <div class="publish-panel-head">
            <div class="publish-panel-title">Publish Software</div>
          </div>
          <div id="publish-drop" class="publish-drop">
            <input id="publish-file-input" type="file" hidden />
            <div class="publish-drop-label">Drop .zip or executable <span class="publish-drop-link">browse</span></div>
          </div>
          <div id="publish-options" class="publish-options" style="display:none">
            <div id="publish-filename" class="publish-filename"></div>
            <label class="publish-label">
              <input type="checkbox" id="publish-promote-current" />
              Promote To Current and recreate matching node(s) on this hive
            </label>
            <label id="publish-preserve-config-wrap" class="publish-label" style="display:none">
              <input type="checkbox" id="publish-preserve-node-config" checked />
              Preserve existing node config during recreate
            </label>
            <button id="publish-btn" class="publish-btn" type="button">Publish</button>
          </div>
          <div id="publish-result" class="publish-result"></div>
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
    const softwareUploadUrl = (base || "") + "/api/software/upload";
    const softwarePublishUrl = (base || "") + "/api/software/publish";
    const packagePublishUrl = (base || "") + "/api/package/publish";
    const sessionsUrl = (base || "") + "/api/sessions";
    const sessionMetaUrl = (base || "") + "/api/session-meta";
    const identityIchOptionsUrl = (base || "") + "/api/identity/ich-options";
    function pipelineRecoveryUrl(sessionId) {{
      return sessionsUrl + "/" + encodeURIComponent(sessionId) + "/pipeline-recovery";
    }}
    const currentSessionStorageKey = "sy.architect.currentSession.{hive}";
    const statusRefreshActiveMs = 15000;
    const statusRefreshHiddenMs = 60000;
    const sessionRefreshActiveMs = 5000;
    const sessionRefreshHiddenMs = 15000;
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
    const publishDrop = document.getElementById("publish-drop");
    const publishFileInput = document.getElementById("publish-file-input");
    const publishOptions = document.getElementById("publish-options");
    const publishFilename = document.getElementById("publish-filename");
    const publishPromoteCurrent = document.getElementById("publish-promote-current");
    const publishPreserveConfigWrap = document.getElementById("publish-preserve-config-wrap");
    const publishPreserveNodeConfig = document.getElementById("publish-preserve-node-config");
    const publishBtn = document.getElementById("publish-btn");
    const publishResult = document.getElementById("publish-result");
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
    function formatTokenCount(value) {{
      const n = Number(value || 0);
      if (!Number.isFinite(n) || n <= 0) return "";
      return Math.round(n).toLocaleString("es-AR");
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
    async function submitPipelineRecoveryAction(action, recovery) {{
      if (!currentSessionId || !recovery || !recovery.pipeline_run_id) {{
        throw new Error("No interrupted pipeline run is available.");
      }}
      const res = await fetch(pipelineRecoveryUrl(currentSessionId), {{
        method: "POST",
        headers: {{ "Content-Type": "application/json" }},
        body: JSON.stringify({{
          action,
          pipeline_run_id: recovery.pipeline_run_id
        }})
      }});
      if (!res.ok) {{
        const error = await res.json().catch(() => ({{ error: "pipeline recovery request failed" }}));
        throw new Error(error && error.error ? error.error : "pipeline recovery request failed");
      }}
      return res.json();
    }}
    function renderPipelineRecoveryPrompt(recovery) {{
      if (!recovery || !recovery.pipeline_run_id) return;
      const shell = document.createElement("div");
      const head = document.createElement("div");
      const title = document.createElement("div");
      const badge = document.createElement("div");
      const copy = document.createElement("div");
      const actions = document.createElement("div");
      const resumeButton = document.createElement("button");
      const discardButton = document.createElement("button");
      shell.className = "recovery-shell";
      head.className = "result-head";
      title.className = "result-title";
      badge.className = "result-status warn";
      copy.className = "recovery-copy";
      actions.className = "recovery-actions";
      title.textContent = "Pipeline recovery available";
      badge.textContent = recovery.current_stage || "interrupted";
      copy.textContent =
        "An interrupted pipeline run is available for this chat" +
        (recovery.solution_id ? " (" + recovery.solution_id + ")" : "") +
        ". Resume returns to the last checkpoint. Discard marks that run as failed so you can start over cleanly.";
      resumeButton.type = "button";
      resumeButton.textContent = "Resume pipeline";
      discardButton.type = "button";
      discardButton.className = "secondary-button";
      discardButton.textContent = "Discard run";
      const setBusy = (busy) => {{
        resumeButton.disabled = busy;
        discardButton.disabled = busy;
      }};
      resumeButton.addEventListener("click", async () => {{
        setBusy(true);
        try {{
          await submitPipelineRecoveryAction("resume", recovery);
          addMessage("system", "Pipeline resumed at stage " + (recovery.current_stage || "unknown") + ".");
          await loadSession(currentSessionId, null, false, true);
          await refreshSessionList(currentSessionId);
        }} catch (err) {{
          addMessage("system", "Pipeline resume failed: " + err);
        }} finally {{
          setBusy(false);
        }}
      }});
      discardButton.addEventListener("click", async () => {{
        const confirmed = await openConfirmModal({{
          title: "Discard interrupted pipeline",
          message: "This marks the interrupted pipeline run as failed. Continue?",
          kicker: "Pipeline Recovery",
          confirmLabel: "Discard run",
          tone: "danger"
        }});
        if (!confirmed) return;
        setBusy(true);
        try {{
          await submitPipelineRecoveryAction("discard", recovery);
          addMessage("system", "Interrupted pipeline discarded.");
          await loadSession(currentSessionId, null, false, true);
          await refreshSessionList(currentSessionId);
        }} catch (err) {{
          addMessage("system", "Pipeline discard failed: " + err);
        }} finally {{
          setBusy(false);
        }}
      }});
      head.appendChild(title);
      head.appendChild(badge);
      actions.appendChild(resumeButton);
      actions.appendChild(discardButton);
      shell.appendChild(head);
      shell.appendChild(copy);
      shell.appendChild(actions);
      appendMessage("system", "Pipeline recovery", shell);
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
    function renderPlanCompileTrace(trace) {{
      if (!trace || !trace.step_count) return null;
      const isRetry = trace.validation === "ok_after_retry";
      const validationLabel = isRetry ? "retry" : "ok";

      const wrapper = document.createElement("details");
      wrapper.className = "agent-trace";

      const toggle = document.createElement("summary");
      toggle.className = "agent-trace-toggle";

      const icon = document.createElement("span");
      icon.textContent = "⟳ ";
      icon.style.fontSize = "0.9rem";
      const label = document.createElement("span");
      label.className = "agent-trace-label";
      label.textContent = "Agent activity · plan_compiler → " + trace.step_count + " step" + (trace.step_count !== 1 ? "s" : "");
      const badge = document.createElement("span");
      badge.className = "agent-trace-badge " + validationLabel;
      badge.textContent = isRetry ? "retried" : "valid";
      const tokenBadge = document.createElement("span");
      tokenBadge.className = "agent-trace-badge ok";
      const tokenText = formatTokenCount(trace.tokens_used);
      tokenBadge.textContent = tokenText ? tokenText + " tokens" : "";

      toggle.appendChild(icon);
      toggle.appendChild(label);
      toggle.appendChild(badge);
      if (tokenText) toggle.appendChild(tokenBadge);
      wrapper.appendChild(toggle);

      const body = document.createElement("div");
      body.className = "agent-trace-body";

      // ── section: call ──
      const callSection = document.createElement("div");
      callSection.className = "agent-trace-section";
      const callLabel = document.createElement("div");
      callLabel.className = "agent-trace-section-label";
      callLabel.textContent = "plan_compiler call";
      callSection.appendChild(callLabel);
      const callRow = document.createElement("div");
      callRow.className = "agent-trace-row";
      callRow.innerHTML =
        "<span class='agent-trace-row-action'>task →</span>" +
        "<span class='agent-trace-row-args'>" +
        (trace.task ? String(trace.task).slice(0, 200) : "") +
        "</span>";
      callSection.appendChild(callRow);
      const hiveRow = document.createElement("div");
      hiveRow.className = "agent-trace-row";
      hiveRow.innerHTML =
        "<span class='agent-trace-row-action'>hive →</span>" +
        "<span class='agent-trace-row-args'>" + (trace.hive || "") + "</span>";
      callSection.appendChild(hiveRow);
      body.appendChild(callSection);

      // ── section: plan steps ──
      const steps = Array.isArray(trace.steps) ? trace.steps : [];
      if (steps.length > 0) {{
        const stepsSection = document.createElement("div");
        stepsSection.className = "agent-trace-section";
        const stepsLabel = document.createElement("div");
        stepsLabel.className = "agent-trace-section-label";
        stepsLabel.textContent = "generated plan · " + steps.length + " step" + (steps.length !== 1 ? "s" : "");
        stepsSection.appendChild(stepsLabel);
        steps.forEach((step, i) => {{
          const row = document.createElement("div");
          row.className = "agent-trace-row";
          const idEl = document.createElement("span");
          idEl.className = "agent-trace-row-id";
          idEl.textContent = (step.id || String(i + 1));
          const actionEl = document.createElement("span");
          actionEl.className = "agent-trace-row-action";
          actionEl.textContent = step.action || "?";
          const argsEl = document.createElement("span");
          argsEl.className = "agent-trace-row-args";
          argsEl.title = step.args_preview || "";
          argsEl.textContent = step.args_preview || "";
          row.appendChild(idEl);
          row.appendChild(actionEl);
          row.appendChild(argsEl);
          stepsSection.appendChild(row);
        }});
        body.appendChild(stepsSection);
      }}

      // ── section: pre-validation ──
      const validSection = document.createElement("div");
      validSection.className = "agent-trace-section";
      const validLabel = document.createElement("div");
      validLabel.className = "agent-trace-section-label";
      validLabel.textContent = "pre-validation";
      validSection.appendChild(validLabel);

      if (isRetry && trace.first_validation_error) {{
        const errEl = document.createElement("div");
        errEl.className = "agent-trace-error";
        errEl.textContent = "✗ first plan rejected: " + trace.first_validation_error;
        validSection.appendChild(errEl);
        const retryOk = document.createElement("div");
        retryOk.className = "agent-trace-row";
        retryOk.innerHTML = "<span class='agent-trace-row-action'>retry →</span><span class='agent-trace-row-args'>plan corrected and accepted</span>";
        validSection.appendChild(retryOk);
      }} else {{
        const okRow = document.createElement("div");
        okRow.className = "agent-trace-row";
        okRow.innerHTML = "<span class='agent-trace-row-action'>validation →</span><span class='agent-trace-row-args'>ok</span>";
        validSection.appendChild(okRow);
      }}
      body.appendChild(validSection);

      wrapper.appendChild(body);
      return wrapper;
    }}
    function renderDesignerTrace(traces) {{
      const items = Array.isArray(traces) ? traces : [];
      if (!items.length) return null;

      const wrapper = document.createElement("details");
      wrapper.className = "agent-trace";

      const toggle = document.createElement("summary");
      toggle.className = "agent-trace-toggle";

      const icon = document.createElement("span");
      icon.textContent = "✎ ";
      icon.style.fontSize = "0.9rem";
      const label = document.createElement("span");
      label.className = "agent-trace-label";
      label.textContent = "Agent activity · designer";
      const badge = document.createElement("span");
      badge.className = "agent-trace-badge ok";
      badge.textContent = items.length === 1 ? "1 iteration" : (items.length + " iterations");

      toggle.appendChild(icon);
      toggle.appendChild(label);
      toggle.appendChild(badge);
      wrapper.appendChild(toggle);

      const body = document.createElement("div");
      body.className = "agent-trace-body";

      items.forEach((trace, index) => {{
        const section = document.createElement("div");
        section.className = "agent-trace-section";
        const sectionLabel = document.createElement("div");
        sectionLabel.className = "agent-trace-section-label";
        sectionLabel.textContent = "designer iteration " + (index + 1);
        section.appendChild(sectionLabel);

        const taskRow = document.createElement("div");
        taskRow.className = "agent-trace-row";
        taskRow.innerHTML =
          "<span class='agent-trace-row-action'>task →</span>" +
          "<span class='agent-trace-row-args'>" + (trace && trace.task ? String(trace.task).slice(0, 200) : "") + "</span>";
        section.appendChild(taskRow);

        if (trace && trace.solution_id) {{
          const solutionRow = document.createElement("div");
          solutionRow.className = "agent-trace-row";
          solutionRow.innerHTML =
            "<span class='agent-trace-row-action'>solution →</span>" +
            "<span class='agent-trace-row-args'>" + trace.solution_id + "</span>";
          section.appendChild(solutionRow);
        }}

        const metaRow = document.createElement("div");
        metaRow.className = "agent-trace-row";
        metaRow.innerHTML =
          "<span class='agent-trace-row-action'>manifest →</span>" +
          "<span class='agent-trace-row-args'>v" + (trace && trace.manifest_version ? trace.manifest_version : "?") +
          " · " + Number(trace && trace.section_count ? trace.section_count : 0) + " section(s)" +
          " · query_hive " + Number(trace && trace.query_hive_calls ? trace.query_hive_calls : 0) + " time(s)" +
          "</span>";
        section.appendChild(metaRow);

        const validationRow = document.createElement("div");
        validationRow.className = "agent-trace-row";
        validationRow.innerHTML =
          "<span class='agent-trace-row-action'>validation →</span>" +
          "<span class='agent-trace-row-args'>" + (trace && trace.validation_result ? trace.validation_result : "unknown") + "</span>";
        section.appendChild(validationRow);

        body.appendChild(section);
      }});

      wrapper.appendChild(body);
      return wrapper;
    }}
    function renderDesignLoopTrace(events) {{
      const items = Array.isArray(events) ? events : [];
      if (!items.length) return null;

      const wrapper = document.createElement("details");
      wrapper.className = "agent-trace";

      const toggle = document.createElement("summary");
      toggle.className = "agent-trace-toggle";

      const icon = document.createElement("span");
      icon.textContent = "↺ ";
      icon.style.fontSize = "0.9rem";
      const label = document.createElement("span");
      label.className = "agent-trace-label";
      label.textContent = "Agent activity · design_loop";
      const badge = document.createElement("span");
      badge.className = "agent-trace-badge ok";
      badge.textContent = items.length === 1 ? "1 event" : (items.length + " events");

      toggle.appendChild(icon);
      toggle.appendChild(label);
      toggle.appendChild(badge);
      wrapper.appendChild(toggle);

      const body = document.createElement("div");
      body.className = "agent-trace-body";

      const section = document.createElement("div");
      section.className = "agent-trace-section";
      const sectionLabel = document.createElement("div");
      sectionLabel.className = "agent-trace-section-label";
      sectionLabel.textContent = "design loop progression";
      section.appendChild(sectionLabel);

      items.forEach((event) => {{
        const row = document.createElement("div");
        row.className = "agent-trace-row";
        const iteration = event && event.iteration != null ? event.iteration : "?";
        const stage = event && event.stage ? event.stage : "unknown";
        const score = event && event.score != null ? event.score : "?";
        const status = event && event.status ? event.status : "unknown";
        const blockers = event && event.blocking_issue_count != null ? event.blocking_issue_count : 0;
        const reason = event && event.stopped_reason ? " · stop " + event.stopped_reason : "";
        row.innerHTML =
          "<span class='agent-trace-row-id'>" + iteration + "</span>" +
          "<span class='agent-trace-row-action'>" + stage + "</span>" +
          "<span class='agent-trace-row-args'>" +
          "status " + status + " · score " + score + "/10 · blockers " + blockers + reason +
          "</span>";
        section.appendChild(row);
      }});

      body.appendChild(section);
      wrapper.appendChild(body);
      return wrapper;
    }}
    function renderArtifactLoopTrace(events) {{
      const items = Array.isArray(events) ? events : [];
      if (!items.length) return null;

      const wrapper = document.createElement("details");
      wrapper.className = "agent-trace";
      const hasFailure = items.some((event) => {{
        const status = event && event.status ? String(event.status) : "";
        return status === "rejected" || status === "blocked" || status === "repairable";
      }});
      if (hasFailure) {{
        wrapper.open = true;
      }}

      const toggle = document.createElement("summary");
      toggle.className = "agent-trace-toggle";

      const icon = document.createElement("span");
      icon.textContent = "⚙ ";
      icon.style.fontSize = "0.9rem";
      const label = document.createElement("span");
      label.className = "agent-trace-label";
      label.textContent = "Agent activity · artifact_loop";
      const badge = document.createElement("span");
      badge.className = "agent-trace-badge " + (hasFailure ? "warn" : "ok");
      badge.textContent = items.length === 1 ? "1 event" : (items.length + " events");

      toggle.appendChild(icon);
      toggle.appendChild(label);
      toggle.appendChild(badge);
      wrapper.appendChild(toggle);

      const body = document.createElement("div");
      body.className = "agent-trace-body";
      const section = document.createElement("div");
      section.className = "agent-trace-section";
      const sectionLabel = document.createElement("div");
      sectionLabel.className = "agent-trace-section-label";
      sectionLabel.textContent = "artifact loop progression";
      section.appendChild(sectionLabel);

      items.forEach((event) => {{
        const row = document.createElement("div");
        row.className = "agent-trace-row";
        const attempt = event && event.attempt != null ? event.attempt : "?";
        const taskId = event && event.task_id ? event.task_id : "unknown-task";
        const status = event && event.status ? event.status : "unknown";
        const findingCount = event && event.findings_count != null ? event.findings_count : 0;
        const failureClass = event && event.failure_class ? " · " + event.failure_class : "";
        const bundleId = event && event.bundle_id ? " · bundle " + event.bundle_id : "";
        row.innerHTML =
          "<span class='agent-trace-row-id'>" + attempt + "</span>" +
          "<span class='agent-trace-row-action'>" + status + "</span>" +
          "<span class='agent-trace-row-args'>" +
          taskId + " · findings " + findingCount + failureClass + bundleId +
          "</span>";
        section.appendChild(row);
      }});

      body.appendChild(section);
      wrapper.appendChild(body);
      return wrapper;
    }}
    function extractPipelineTrace(toolResults) {{
      const results = Array.isArray(toolResults) ? toolResults : [];
      for (const tool of results) {{
        if (!tool || tool.is_error) continue;
        if (tool.name !== "fluxbee_start_pipeline") continue;
        const output = tool.output || {{}};
        return {{
          designerTraces: Array.isArray(output.designer_traces) ? output.designer_traces : [],
          designLoopTrace: Array.isArray(output.design_loop_trace) ? output.design_loop_trace : [],
          artifactLoopTrace: Array.isArray(output.artifact_loop_trace) ? output.artifact_loop_trace : [],
          planCompileTrace: output.plan_compile_trace || null,
        }};
      }}
      return null;
    }}
    function extractDirectPipelineTrace(output) {{
      if (!output || typeof output !== "object") return null;
      const designerTraces = Array.isArray(output.designer_traces) ? output.designer_traces : [];
      const designLoopTrace = Array.isArray(output.design_loop_trace) ? output.design_loop_trace : [];
      const artifactLoopTrace = Array.isArray(output.artifact_loop_trace) ? output.artifact_loop_trace : [];
      const planCompileTrace = output.plan_compile_trace || null;
      if (!designerTraces.length && !designLoopTrace.length && !artifactLoopTrace.length && !planCompileTrace) {{
        return null;
      }}
      return {{ designerTraces, designLoopTrace, artifactLoopTrace, planCompileTrace }};
    }}
    function extractExecutorTokenUsage(data) {{
      const output = data && data.output ? data.output : {{}};
      if (output.token_usage && typeof output.token_usage === "object") {{
        return output.token_usage;
      }}
      if (output.plan_compile_trace && typeof output.plan_compile_trace === "object") {{
        const planTokens = Number(output.plan_compile_trace.tokens_used || 0);
        const budget = Number(output.plan_compile_trace.token_budget || 0);
        if (planTokens > 0) {{
          return {{ plan_compile: planTokens, total: planTokens, budget }};
        }}
      }}
      return null;
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
      const headRight = document.createElement("div");
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
      headRight.className = "exec-head-right";
      const tokenUsage = extractExecutorTokenUsage(data);
      const tokenTotal = tokenUsage ? Number(tokenUsage.total || tokenUsage.plan_compile || 0) : 0;
      if (tokenTotal > 0) {{
        const tokenChip = document.createElement("div");
        tokenChip.className = "exec-token-chip";
        const budgetText = tokenUsage && tokenUsage.budget ? " / " + formatTokenCount(tokenUsage.budget) : "";
        tokenChip.textContent = "tokens " + formatTokenCount(tokenTotal) + budgetText;
        headRight.appendChild(tokenChip);
      }}
      const badge = document.createElement("div");
      badge.className = "result-status " + (isError ? "warn" : "ok");
      badge.textContent = overallStatus;
      head.appendChild(titleBlock);
      headRight.appendChild(badge);
      head.appendChild(headRight);
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
      const directPipelineTrace = extractDirectPipelineTrace(output);
      if (data && data.mode === "chat" && output && typeof output.message === "string" && output.message.trim()) {{
        const toolSummary = createToolSummarySection(output.tool_results);
        const pipelineTrace = extractPipelineTrace(output.tool_results) || directPipelineTrace;
        const progTrace = renderPlanCompileTrace(
          pipelineTrace && pipelineTrace.planCompileTrace
            ? pipelineTrace.planCompileTrace
            : output.plan_compile_trace
        );
        const designerTrace = pipelineTrace ? renderDesignerTrace(pipelineTrace.designerTraces) : null;
        const designLoopTrace = pipelineTrace ? renderDesignLoopTrace(pipelineTrace.designLoopTrace) : null;
        const artifactLoopTrace = pipelineTrace ? renderArtifactLoopTrace(pipelineTrace.artifactLoopTrace) : null;
        if (!toolSummary && !progTrace && !designerTrace && !designLoopTrace && !artifactLoopTrace) {{
          addMessage(kind, output.message);
          return;
        }}
        const body = document.createElement("div");
        const message = document.createElement("div");
        body.className = "chat-response";
        message.textContent = output.message;
        body.appendChild(message);
        if (designerTrace) body.appendChild(designerTrace);
        if (designLoopTrace) body.appendChild(designLoopTrace);
        if (artifactLoopTrace) body.appendChild(artifactLoopTrace);
        if (progTrace) body.appendChild(progTrace);
        if (toolSummary) body.appendChild(toolSummary);
        appendMessage(kind, kind === "architect" ? "archi" : "System", body);
        return;
      }}
      if (data && data.mode === "pipeline" && output && typeof output.message === "string" && output.message.trim()) {{
        const pipelineTrace = directPipelineTrace;
        const progTrace = pipelineTrace ? renderPlanCompileTrace(pipelineTrace.planCompileTrace) : null;
        const designerTrace = pipelineTrace ? renderDesignerTrace(pipelineTrace.designerTraces) : null;
        const designLoopTrace = pipelineTrace ? renderDesignLoopTrace(pipelineTrace.designLoopTrace) : null;
        const artifactLoopTrace = pipelineTrace ? renderArtifactLoopTrace(pipelineTrace.artifactLoopTrace) : null;
        if (progTrace || designerTrace || designLoopTrace || artifactLoopTrace) {{
          const body = document.createElement("div");
          const message = document.createElement("div");
          body.className = "chat-response";
          message.textContent = output.message;
          body.appendChild(message);
          if (designerTrace) body.appendChild(designerTrace);
          if (designLoopTrace) body.appendChild(designLoopTrace);
          if (artifactLoopTrace) body.appendChild(artifactLoopTrace);
          if (progTrace) body.appendChild(progTrace);
          appendMessage(kind, kind === "architect" ? "archi" : "System", body);
          return;
        }}
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
      const recovery = detail && detail.pipeline_recovery ? detail.pipeline_recovery : null;
      const recoveryToken = recovery
        ? [
            recovery.pipeline_run_id || "",
            recovery.status || "",
            recovery.current_stage || "",
            recovery.interrupted_at_ms || 0
          ].join(":")
        : "none";
      return String(count) + ":" + String(updated) + ":" + recoveryToken;
    }}
    function renderSession(detail, showWelcome = false, preserveComposer = false) {{
      resetChatViewport(preserveComposer);
      const session = detail && detail.session ? detail.session : null;
      const recovery = detail && detail.pipeline_recovery ? detail.pipeline_recovery : null;
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
        if (recovery) {{
          renderPipelineRecoveryPrompt(recovery);
        }}
        return;
      }}
      detail.messages.forEach((message) => renderStoredMessage(message));
      if (recovery) {{
        renderPipelineRecoveryPrompt(recovery);
      }}
    }}
    async function loadSession(sessionId, existingDetail, showWelcome = false, preserveComposer = false) {{
      currentSessionId = sessionId;
      renderHistory();
      localStorage.setItem(currentSessionStorageKey, sessionId);
      if (existingDetail) {{
        renderSession(existingDetail, showWelcome, preserveComposer);
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
      renderSession(detail, showWelcome, preserveComposer);
    }}
    async function refreshCurrentSession(options = {{}}) {{
      const force = !!(options && options.force);
      if (!currentSessionId || sessionRefreshInFlight) {{
        return;
      }}
      sessionRefreshInFlight = true;
      try {{
        const res = await fetch(sessionMetaUrl + "/" + encodeURIComponent(currentSessionId), {{ cache: "no-store" }});
        if (!res.ok) {{
          return;
        }}
        const detail = await res.json();
        const nextRevision = detail && detail.revision
          ? String(detail.revision) + ":" + (
              detail.pipeline_recovery
                ? [
                    detail.pipeline_recovery.pipeline_run_id || "",
                    detail.pipeline_recovery.status || "",
                    detail.pipeline_recovery.current_stage || "",
                    detail.pipeline_recovery.interrupted_at_ms || 0
                  ].join(":")
                : "none"
            )
          : sessionRevision({{ session: detail || null }});
        if (force || nextRevision !== currentSessionRevision) {{
          await loadSession(currentSessionId, null, false, true);
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
    let submitInFlight = false;
    async function submit() {{
      if (submitInFlight) return;
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
      submitInFlight = true;
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
        submitInFlight = false;
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

    // ── Publish Software panel ─────────────────────────────────
    let publishSelectedFile = null;
    let publishProgressLines = [];
    function isZipPublishFile(file) {{
      return !!(file && file.name && file.name.toLowerCase().endsWith(".zip"));
    }}
    function resetPublishProgress() {{
      publishProgressLines = [];
    }}
    function renderPublishProgress() {{
      publishResult.className = "publish-result progress";
      publishResult.textContent = publishProgressLines.join("\n");
    }}
    function pushPublishProgress(line) {{
      publishProgressLines.push(line);
      renderPublishProgress();
    }}
    function updatePublishOptionsState() {{
      const promote = !!(publishPromoteCurrent && publishPromoteCurrent.checked);
      if (publishPreserveConfigWrap) {{
        publishPreserveConfigWrap.style.display = promote ? "" : "none";
      }}
      if (publishPreserveNodeConfig) {{
        publishPreserveNodeConfig.disabled = !promote;
      }}
    }}
    function publishSetFile(file) {{
      if (!file) {{
        publishResult.className = "publish-result err";
        publishResult.textContent = "Choose one .zip or executable file.";
        return;
      }}
      publishSelectedFile = file;
      const isZip = isZipPublishFile(file);
      const label = isZip ? "ZIP package" : "Executable upload pilot";
      publishFilename.textContent = file.name + " \u00B7 " + label;
      if (publishPromoteCurrent) publishPromoteCurrent.checked = false;
      if (publishPreserveNodeConfig) publishPreserveNodeConfig.checked = true;
      updatePublishOptionsState();
      publishOptions.style.display = "";
      resetPublishProgress();
      publishResult.className = "publish-result";
      publishResult.textContent = "";
    }}
    publishDrop.addEventListener("click", () => {{ publishFileInput.click(); }});
    publishDrop.addEventListener("dragover", (e) => {{ e.preventDefault(); publishDrop.classList.add("dragover"); }});
    publishDrop.addEventListener("dragleave", () => {{ publishDrop.classList.remove("dragover"); }});
    publishDrop.addEventListener("drop", (e) => {{
      e.preventDefault();
      publishDrop.classList.remove("dragover");
      const file = e.dataTransfer && e.dataTransfer.files && e.dataTransfer.files[0];
      if (file) publishSetFile(file);
    }});
    publishFileInput.addEventListener("change", (e) => {{
      const file = e.target && e.target.files && e.target.files[0];
      publishFileInput.value = "";
      if (file) publishSetFile(file);
    }});
    if (publishPromoteCurrent) {{
      publishPromoteCurrent.addEventListener("change", () => {{
        updatePublishOptionsState();
      }});
    }}
    publishBtn.addEventListener("click", async () => {{
      if (!publishSelectedFile) return;
      publishBtn.disabled = true;
      resetPublishProgress();
      const promoteToCurrent = !!(publishPromoteCurrent && publishPromoteCurrent.checked);
      const preserveNodeConfig = promoteToCurrent && !!(publishPreserveNodeConfig && publishPreserveNodeConfig.checked);
      pushPublishProgress("1. Uploading file...");
      try {{
        const formData = new FormData();
        formData.append("file", publishSelectedFile, publishSelectedFile.name);
        const uploadRes = await fetch(softwareUploadUrl, {{ method: "POST", body: formData }});
        const uploadData = await uploadRes.json();
        if (!uploadRes.ok || uploadData.status !== "ok" || !uploadData.attachment) {{
          throw new Error(uploadData.error || "Upload failed");
        }}
        const blobName = uploadData.attachment.blob_ref && uploadData.attachment.blob_ref.blob_name;
        if (!blobName) throw new Error("No blob_name in upload response");
        pushPublishProgress("2. Upload complete.");
        if (uploadData.upload_kind === "bundle_upload") {{
          pushPublishProgress("3. Publishing ZIP package...");
          if (promoteToCurrent) {{
            pushPublishProgress("4. Promoting to current and recreating matching node(s)...");
          }}
        }} else {{
          pushPublishProgress("3. Resolving runtime from executable name...");
          pushPublishProgress("4. Building package from executable...");
          pushPublishProgress("5. Publishing generated package...");
          if (promoteToCurrent) {{
            pushPublishProgress("6. Promoting to current and recreating matching node(s)...");
          }}
        }}
        let pubRes;
        if (uploadData.upload_kind === "bundle_upload") {{
          pubRes = await fetch(packagePublishUrl, {{
            method: "POST",
            headers: {{ "Content-Type": "application/json" }},
            body: JSON.stringify({{
              blob_name: blobName,
              set_current: promoteToCurrent,
              preserve_node_config: preserveNodeConfig,
              sync_to: [],
              update_to: []
            }})
          }});
        }} else {{
          pubRes = await fetch(softwarePublishUrl, {{
            method: "POST",
            headers: {{ "Content-Type": "application/json" }},
            body: JSON.stringify({{
              blob_name: blobName,
              filename: uploadData.attachment.filename,
              set_current: promoteToCurrent,
              preserve_node_config: preserveNodeConfig
            }})
          }});
        }}
        const pubData = await pubRes.json();
        const effectiveStatus = pubData.status || (pubData.publish && pubData.publish.status) || "error";
        if (effectiveStatus === "ok" || effectiveStatus === "sync_pending") {{
          const publishPayload = pubData.payload || (pubData.publish && pubData.publish.payload) || {{}};
          const resolution = pubData.resolution || null;
          const redeploy = pubData.redeploy || (pubData.publish && pubData.publish.redeploy) || null;
          const name = publishPayload.runtime_name || (resolution && resolution.runtime_name) || "";
          const ver = publishPayload.runtime_version || (resolution && resolution.next_version) || "";
          const summary = resolution && resolution.summary ? " \u00B7 " + resolution.summary : "";
          const redeploySummary = redeploy && redeploy !== null && redeploy.status
            ? " \u00B7 redeploy " + redeploy.status
                + (typeof redeploy.matched_nodes === "number" ? " (" + redeploy.matched_nodes + " node(s))" : "")
            : "";
          publishResult.className = "publish-result ok";
          publishResult.textContent =
            publishProgressLines.join("\n")
            + "\n"
            + "Published"
            + (name ? ": " + name + (ver ? " v" + ver : "") : "")
            + summary
            + redeploySummary
            + " ✓";
          publishOptions.style.display = "none";
          publishSelectedFile = null;
          resetPublishProgress();
        }} else {{
          const errMsg = pubData.error
            || (pubData.publish && (pubData.publish.error || (pubData.publish.payload && pubData.publish.payload.error_detail)))
            || (pubData.payload && pubData.payload.error_detail)
            || "Publish failed";
          publishResult.className = "publish-result err";
          publishResult.textContent = publishProgressLines.join("\n") + "\nError: " + (typeof errMsg === "string" ? errMsg : JSON.stringify(errMsg));
          resetPublishProgress();
        }}
      }} catch (err) {{
        publishResult.className = "publish-result err";
        publishResult.textContent = publishProgressLines.join("\n") + "\nError: " + err;
        resetPublishProgress();
      }} finally {{
        publishBtn.disabled = false;
      }}
    }});
    // ──────────────────────────────────────────────────────────

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

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline infrastructure — Track A (manifest storage, pipeline_runs, state
// machine) and Track B (snapshot builder).
// ═══════════════════════════════════════════════════════════════════════════

// ── TA-1: manifest validation ─────────────────────────────────────────────

fn validate_manifest_v2(manifest: &SolutionManifestV2) -> Result<(), String> {
    if manifest.manifest_version.trim().is_empty() {
        return Err("manifest_version must not be empty".to_string());
    }
    if !manifest.solution.is_object() {
        return Err("solution must be a JSON object".to_string());
    }

    let unknown_sections = desired_state_unknown_sections(&manifest.desired_state);
    if let Some(section) = unknown_sections.first() {
        let detail = if UNSUPPORTED_DESIRED_STATE_SECTIONS.contains(&section.as_str()) {
            "is not supported in desired_state for this architecture version. Move it to advisory."
        } else {
            "is not a recognized desired_state section for solution_manifest v2."
        };
        return Err(format!(
            "UNSUPPORTED_DESIRED_STATE_SECTION: '{section}' {detail}"
        ));
    }

    if let Some(topology) = &manifest.desired_state.topology {
        let topology_obj = topology
            .as_object()
            .ok_or_else(|| "desired_state.topology must be a JSON object".to_string())?;
        if let Some(hives) = topology_obj.get("hives") {
            let items = hives
                .as_array()
                .ok_or_else(|| "desired_state.topology.hives must be an array".to_string())?;
            for (idx, item) in items.iter().enumerate() {
                let hive: DesiredHive = serde_json::from_value(item.clone())
                    .map_err(|err| format!("desired_state.topology.hives[{idx}] invalid: {err}"))?;
                if hive.hive_id.trim().is_empty() {
                    return Err(format!(
                        "desired_state.topology.hives[{idx}].hive_id must not be empty"
                    ));
                }
                if !is_valid_ownership_label(&hive.ownership) {
                    return Err(format!(
                        "desired_state.topology.hives[{idx}].ownership must be one of solution/system/external"
                    ));
                }
            }
        }
        if let Some(vpns) = topology_obj.get("vpns") {
            let items = vpns
                .as_array()
                .ok_or_else(|| "desired_state.topology.vpns must be an array".to_string())?;
            for (idx, item) in items.iter().enumerate() {
                let vpn: DesiredVpn = serde_json::from_value(item.clone())
                    .map_err(|err| format!("desired_state.topology.vpns[{idx}] invalid: {err}"))?;
                if vpn.vpn_id.trim().is_empty() || vpn.hive_id.trim().is_empty() {
                    return Err(format!(
                        "desired_state.topology.vpns[{idx}] requires non-empty vpn_id and hive_id"
                    ));
                }
                if !is_valid_ownership_label(&vpn.ownership) {
                    return Err(format!(
                        "desired_state.topology.vpns[{idx}].ownership must be one of solution/system/external"
                    ));
                }
            }
        }
    }

    if let Some(runtimes) = &manifest.desired_state.runtimes {
        for (idx, item) in runtimes.iter().enumerate() {
            let runtime: DesiredRuntime = serde_json::from_value(item.clone())
                .map_err(|err| format!("desired_state.runtimes[{idx}] invalid: {err}"))?;
            if runtime.name.trim().is_empty() {
                return Err(format!(
                    "desired_state.runtimes[{idx}].name must not be empty"
                ));
            }
            if !is_valid_ownership_label(&runtime.ownership) {
                return Err(format!(
                    "desired_state.runtimes[{idx}].ownership must be one of solution/system/external"
                ));
            }
        }
    }

    if let Some(nodes) = &manifest.desired_state.nodes {
        for (idx, item) in nodes.iter().enumerate() {
            let node: DesiredNode = serde_json::from_value(item.clone())
                .map_err(|err| format!("desired_state.nodes[{idx}] invalid: {err}"))?;
            if node.node_name.trim().is_empty()
                || node.hive.trim().is_empty()
                || node.runtime.trim().is_empty()
            {
                return Err(format!(
                    "desired_state.nodes[{idx}] requires non-empty node_name, hive, and runtime"
                ));
            }
            if !is_valid_ownership_label(&node.ownership) {
                return Err(format!(
                    "desired_state.nodes[{idx}].ownership must be one of solution/system/external"
                ));
            }
        }
    }

    if let Some(routes) = &manifest.desired_state.routing {
        for (idx, item) in routes.iter().enumerate() {
            let route: DesiredRoute = serde_json::from_value(item.clone())
                .map_err(|err| format!("desired_state.routing[{idx}] invalid: {err}"))?;
            if route.hive.trim().is_empty()
                || route.prefix.trim().is_empty()
                || route.action.trim().is_empty()
            {
                return Err(format!(
                    "desired_state.routing[{idx}] requires non-empty hive, prefix, and action"
                ));
            }
            if !is_valid_ownership_label(&route.ownership) {
                return Err(format!(
                    "desired_state.routing[{idx}].ownership must be one of solution/system/external"
                ));
            }
        }
    }

    if let Some(workflows) = &manifest.desired_state.wf_deployments {
        for (idx, item) in workflows.iter().enumerate() {
            let wf: DesiredWfDeployment = serde_json::from_value(item.clone())
                .map_err(|err| format!("desired_state.wf_deployments[{idx}] invalid: {err}"))?;
            if wf.hive.trim().is_empty() || wf.workflow_name.trim().is_empty() {
                return Err(format!(
                    "desired_state.wf_deployments[{idx}] requires non-empty hive and workflow_name"
                ));
            }
            if !is_valid_ownership_label(&wf.ownership) {
                return Err(format!(
                    "desired_state.wf_deployments[{idx}].ownership must be one of solution/system/external"
                ));
            }
        }
    }

    if let Some(policies) = &manifest.desired_state.opa_deployments {
        for (idx, item) in policies.iter().enumerate() {
            let opa: DesiredOpaDeployment = serde_json::from_value(item.clone())
                .map_err(|err| format!("desired_state.opa_deployments[{idx}] invalid: {err}"))?;
            if opa.hive.trim().is_empty()
                || opa.policy_id.trim().is_empty()
                || opa.rego_source.trim().is_empty()
            {
                return Err(format!(
                    "desired_state.opa_deployments[{idx}] requires non-empty hive, policy_id, and rego_source"
                ));
            }
            if !is_valid_ownership_label(&opa.ownership) {
                return Err(format!(
                    "desired_state.opa_deployments[{idx}].ownership must be one of solution/system/external"
                ));
            }
        }
    }

    if let Some(ownership) = &manifest.desired_state.ownership {
        if !ownership.is_object() {
            return Err("desired_state.ownership must be a JSON object when present".to_string());
        }
    }
    Ok(())
}

// ── TA-2: manifest storage ───────────────────────────────────────────────

fn manifest_version_string(created_at_ms: u64) -> String {
    format!("v{created_at_ms}")
}

async fn save_manifest_record_with_context(
    context: &ArchitectAdminToolContext,
    solution_id: &str,
    version: &str,
    path: &Path,
) -> Result<(), ArchitectError> {
    let db = open_architect_db_for_hive(&context.hive_id).await?;
    save_manifest_record_with_db(&db, solution_id, version, path).await
}

async fn save_manifest_record_with_db(
    db: &Connection,
    solution_id: &str,
    version: &str,
    path: &Path,
) -> Result<(), ArchitectError> {
    let table = ensure_manifest_refs_table(&db).await?;
    let now = now_epoch_ms();
    let batch = RecordBatch::try_new(
        manifest_refs_schema(),
        vec![
            Arc::new(StringArray::from(vec![solution_id])),
            Arc::new(StringArray::from(vec![version])),
            Arc::new(StringArray::from(vec![path.to_string_lossy().as_ref()])),
            Arc::new(UInt64Array::from(vec![now])),
        ],
    )?;
    let mut merge = table.merge_insert(&["solution_id", "version"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge
        .execute(single_batch_reader(batch))
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    Ok(())
}

async fn save_manifest_from_context(
    context: &ArchitectAdminToolContext,
    solution_id: &str,
    manifest: &SolutionManifestV2,
) -> Result<PathBuf, ArchitectError> {
    validate_manifest_v2(manifest).map_err(|err| -> ArchitectError { err.into() })?;
    let now = now_epoch_ms();
    let version = manifest_version_string(now);
    let path = manifest_path(&context.state_dir, solution_id, &version);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(manifest)
        .map_err(|e| -> ArchitectError { e.to_string().into() })?;
    fs::write(&path, json)?;
    save_manifest_record_with_context(context, solution_id, &version, &path).await?;
    Ok(path)
}

async fn load_manifest(
    state: &ArchitectState,
    solution_id: &str,
    version: Option<&str>,
) -> Result<Option<SolutionManifestV2>, ArchitectError> {
    load_manifest_from_state_dir(&state.state_dir, solution_id, version).await
}

async fn load_manifest_from_state_dir(
    state_dir: &Path,
    solution_id: &str,
    version: Option<&str>,
) -> Result<Option<SolutionManifestV2>, ArchitectError> {
    let path = if let Some(v) = version {
        manifest_path(state_dir, solution_id, v)
    } else {
        // Find latest by listing directory
        let dir = state_dir.join("manifests").join(solution_id);
        if !dir.exists() {
            return Ok(None);
        }
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        entries.sort();
        match entries.into_iter().last() {
            Some(p) => p,
            None => return Ok(None),
        }
    };
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    let manifest: SolutionManifestV2 = serde_json::from_str(&raw)
        .map_err(|e| -> ArchitectError { format!("manifest parse error: {e}").into() })?;
    validate_manifest_v2(&manifest)
        .map_err(|err| -> ArchitectError { format!("manifest validation error: {err}").into() })?;
    Ok(Some(manifest))
}

fn manifest_refs_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("solution_id", DataType::Utf8, false),
        Field::new("version", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("created_at_ms", DataType::UInt64, false),
    ]))
}

async fn ensure_manifest_refs_table(db: &Connection) -> Result<lancedb::Table, ArchitectError> {
    match db.open_table(MANIFEST_REFS_TABLE).execute().await {
        Ok(t) => Ok(t),
        Err(_) => db
            .create_empty_table(MANIFEST_REFS_TABLE, manifest_refs_schema())
            .execute()
            .await
            .map_err(|e| -> ArchitectError { Box::new(e) }),
    }
}

// ── TA-4: pipeline_runs DB table ──────────────────────────────────────────

fn pipeline_runs_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("pipeline_run_id", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, false),
        Field::new("solution_id", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("current_stage", DataType::Utf8, false),
        Field::new("current_loop", DataType::UInt64, false),
        Field::new("current_attempt", DataType::UInt64, false),
        Field::new("state_json", DataType::Utf8, false),
        Field::new("created_at_ms", DataType::UInt64, false),
        Field::new("updated_at_ms", DataType::UInt64, false),
        Field::new("interrupted_at_ms", DataType::UInt64, false),
    ]))
}

async fn ensure_pipeline_runs_table(db: &Connection) -> Result<lancedb::Table, ArchitectError> {
    match db.open_table(PIPELINE_RUNS_TABLE).execute().await {
        Ok(t) => Ok(t),
        Err(_) => db
            .create_empty_table(PIPELINE_RUNS_TABLE, pipeline_runs_schema())
            .execute()
            .await
            .map_err(|e| -> ArchitectError { Box::new(e) }),
    }
}

fn pipeline_run_batch(rows: &[&PipelineRunRecord]) -> Result<RecordBatch, ArchitectError> {
    let status_str = |r: &PipelineRunRecord| {
        serde_json::to_value(&r.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", r.status))
    };
    let stage_str = |r: &PipelineRunRecord| {
        serde_json::to_value(&r.current_stage)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", r.current_stage))
    };
    let batch = RecordBatch::try_new(
        pipeline_runs_schema(),
        vec![
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.pipeline_run_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.session_id.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.solution_id.as_deref().unwrap_or(""))
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| status_str(r)).collect::<Vec<String>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter().map(|r| stage_str(r)).collect::<Vec<String>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|r| r.current_loop as u64)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|r| r.current_attempt as u64)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                rows.iter()
                    .map(|r| r.state_json.as_str())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.created_at_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter().map(|r| r.updated_at_ms).collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                rows.iter()
                    .map(|r| r.interrupted_at_ms.unwrap_or(0))
                    .collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(batch)
}

async fn save_pipeline_run(
    state: &ArchitectState,
    record: &PipelineRunRecord,
) -> Result<(), ArchitectError> {
    // Write to DB
    let db = open_architect_db(state).await?;
    let table = ensure_pipeline_runs_table(&db).await?;
    let batch = pipeline_run_batch(&[record])?;
    let mut merge = table.merge_insert(&["pipeline_run_id"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge
        .execute(single_batch_reader(batch))
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    // Mirror to in-memory map
    state
        .active_pipeline_runs
        .lock()
        .await
        .insert(record.pipeline_run_id.clone(), record.clone());
    Ok(())
}

async fn save_pipeline_run_with_context(
    context: &ArchitectAdminToolContext,
    record: &PipelineRunRecord,
) -> Result<(), ArchitectError> {
    let db = open_architect_db_for_hive(&context.hive_id).await?;
    let table = ensure_pipeline_runs_table(&db).await?;
    let batch = pipeline_run_batch(&[record])?;
    let mut merge = table.merge_insert(&["pipeline_run_id"]);
    merge
        .when_matched_update_all(None)
        .when_not_matched_insert_all();
    merge
        .execute(single_batch_reader(batch))
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    context
        .active_pipeline_runs
        .lock()
        .await
        .insert(record.pipeline_run_id.clone(), record.clone());
    Ok(())
}

async fn load_pipeline_run(
    state: &ArchitectState,
    run_id: &str,
) -> Result<Option<PipelineRunRecord>, ArchitectError> {
    // Check in-memory first
    if let Some(r) = state.active_pipeline_runs.lock().await.get(run_id) {
        return Ok(Some(r.clone()));
    }
    // Fall back to full table scan filtered in Rust
    let db = open_architect_db(state).await?;
    let table = ensure_pipeline_runs_table(&db).await?;
    let batches = table
        .query()
        .execute()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    Ok(pipeline_run_from_batches(&batches)
        .into_iter()
        .find(|r| r.pipeline_run_id == run_id))
}

async fn load_pipeline_run_with_context(
    context: &ArchitectAdminToolContext,
    run_id: &str,
) -> Result<Option<PipelineRunRecord>, ArchitectError> {
    if let Some(run) = context.active_pipeline_runs.lock().await.get(run_id) {
        return Ok(Some(run.clone()));
    }
    let db = open_architect_db_for_hive(&context.hive_id).await?;
    let table = ensure_pipeline_runs_table(&db).await?;
    let batches = table
        .query()
        .execute()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    Ok(pipeline_run_from_batches(&batches)
        .into_iter()
        .find(|run| run.pipeline_run_id == run_id))
}

fn pipeline_run_with_state_update(
    record: &PipelineRunRecord,
    new_stage: PipelineStage,
    state_update: Option<Value>,
    current_loop: Option<u32>,
) -> Result<PipelineRunRecord, ArchitectError> {
    let mut updated = record.clone();
    updated.current_stage = new_stage.clone();
    updated.status = pipeline_run_status_for_stage(&new_stage);
    updated.updated_at_ms = now_epoch_ms();
    if let Some(loop_value) = current_loop {
        updated.current_loop = loop_value;
    }
    if updated.status == PipelineRunStatus::Interrupted {
        updated
            .interrupted_at_ms
            .get_or_insert(updated.updated_at_ms);
    } else {
        updated.interrupted_at_ms = None;
    }
    if let Some(update) = state_update {
        let mut current =
            serde_json::from_str::<Value>(&updated.state_json).unwrap_or_else(|_| json!({}));
        if let (Some(current_obj), Some(update_obj)) = (current.as_object_mut(), update.as_object())
        {
            for (key, value) in update_obj {
                current_obj.insert(key.clone(), value.clone());
            }
        }
        updated.state_json =
            serde_json::to_string(&current).unwrap_or_else(|_| updated.state_json.clone());
    }
    Ok(updated)
}

async fn list_interrupted_runs_for_startup(
    state: &ArchitectState,
) -> Result<Vec<PipelineRunRecord>, ArchitectError> {
    let db = open_architect_db(state).await?;
    let table = ensure_pipeline_runs_table(&db).await?;
    let batches = table
        .query()
        .execute()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    Ok(pipeline_run_from_batches(&batches)
        .into_iter()
        .filter(|r| r.status == PipelineRunStatus::InProgress)
        .collect())
}

fn pipeline_run_from_batches(batches: &[arrow_array::RecordBatch]) -> Vec<PipelineRunRecord> {
    let mut result = Vec::new();
    for batch in batches {
        let n = batch.num_rows();
        let col_str = |name: &str| -> Vec<String> {
            batch
                .column_by_name(name)
                .and_then(|c| c.as_any().downcast_ref::<StringArray>())
                .map(|a| (0..n).map(|i| a.value(i).to_string()).collect())
                .unwrap_or_else(|| vec!["".to_string(); n])
        };
        let col_u64 = |name: &str| -> Vec<u64> {
            batch
                .column_by_name(name)
                .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
                .map(|a| (0..n).map(|i| a.value(i)).collect())
                .unwrap_or_else(|| vec![0u64; n])
        };
        let ids = col_str("pipeline_run_id");
        let sessions = col_str("session_id");
        let solutions = col_str("solution_id");
        let statuses = col_str("status");
        let stages = col_str("current_stage");
        let loops = col_u64("current_loop");
        let attempts = col_u64("current_attempt");
        let state_jsons = col_str("state_json");
        let created = col_u64("created_at_ms");
        let updated = col_u64("updated_at_ms");
        let interrupted = col_u64("interrupted_at_ms");
        for i in 0..n {
            let status: PipelineRunStatus = serde_json::from_str(&format!("\"{}\"", statuses[i]))
                .unwrap_or(PipelineRunStatus::Interrupted);
            let stage: PipelineStage = serde_json::from_str(&format!("\"{}\"", stages[i]))
                .unwrap_or(PipelineStage::Interrupted);
            result.push(PipelineRunRecord {
                pipeline_run_id: ids[i].clone(),
                session_id: sessions[i].clone(),
                solution_id: if solutions[i].is_empty() {
                    None
                } else {
                    Some(solutions[i].clone())
                },
                status,
                current_stage: stage,
                current_loop: loops[i] as u32,
                current_attempt: attempts[i] as u32,
                state_json: state_jsons[i].clone(),
                created_at_ms: created[i],
                updated_at_ms: updated[i],
                interrupted_at_ms: if interrupted[i] == 0 {
                    None
                } else {
                    Some(interrupted[i])
                },
            });
        }
    }
    result
}

async fn list_pipeline_runs_for_session(
    state: &ArchitectState,
    session_id: &str,
) -> Result<Vec<PipelineRunRecord>, ArchitectError> {
    list_pipeline_runs_for_session_by_hive(&state.hive_id, session_id).await
}

async fn list_pipeline_runs_for_session_by_hive(
    hive_id: &str,
    session_id: &str,
) -> Result<Vec<PipelineRunRecord>, ArchitectError> {
    let db = open_architect_db_for_hive(hive_id).await?;
    let table = ensure_pipeline_runs_table(&db).await?;
    let filter = format!("session_id = '{}'", session_id.replace('\'', "''"));
    let batches = table
        .query()
        .only_if(filter)
        .execute()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| -> ArchitectError { Box::new(e) })?;
    let mut runs = pipeline_run_from_batches(&batches);
    runs.sort_by_key(|run| (run.updated_at_ms, run.created_at_ms));
    Ok(runs)
}

async fn latest_solution_id_for_session(
    context: &ArchitectAdminToolContext,
    session_id: &str,
) -> Result<Option<String>, ArchitectError> {
    if let Some(solution_id) = context
        .active_pipeline_runs
        .lock()
        .await
        .values()
        .filter(|run| run.session_id == session_id)
        .filter_map(|run| {
            run.solution_id
                .clone()
                .map(|solution_id| (run.updated_at_ms, solution_id))
        })
        .max_by_key(|(updated_at_ms, _)| *updated_at_ms)
        .map(|(_, solution_id)| solution_id)
    {
        return Ok(Some(solution_id));
    }

    Ok(
        list_pipeline_runs_for_session_by_hive(&context.hive_id, session_id)
            .await?
            .into_iter()
            .rev()
            .find_map(|run| run.solution_id),
    )
}

fn pipeline_recovery_info_from_record(run: &PipelineRunRecord) -> PipelineRecoveryInfo {
    PipelineRecoveryInfo {
        pipeline_run_id: run.pipeline_run_id.clone(),
        solution_id: run.solution_id.clone(),
        status: serde_json::to_value(&run.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", run.status)),
        current_stage: serde_json::to_value(&run.current_stage)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{:?}", run.current_stage)),
        interrupted_at_ms: run.interrupted_at_ms,
        can_resume: run.status == PipelineRunStatus::Interrupted,
        can_discard: true,
    }
}

async fn latest_interrupted_pipeline_for_session(
    state: &ArchitectState,
    session_id: &str,
) -> Result<Option<PipelineRecoveryInfo>, ArchitectError> {
    Ok(list_pipeline_runs_for_session(state, session_id)
        .await?
        .into_iter()
        .rev()
        .find(|run| run.status == PipelineRunStatus::Interrupted)
        .map(|run| pipeline_recovery_info_from_record(&run)))
}

async fn interrupted_pipeline_run_for_session(
    state: &ArchitectState,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<Option<PipelineRunRecord>, ArchitectError> {
    let runs = list_pipeline_runs_for_session(state, session_id).await?;
    Ok(runs.into_iter().rev().find(|run| {
        run.status == PipelineRunStatus::Interrupted
            && run_id
                .map(|requested| run.pipeline_run_id == requested)
                .unwrap_or(true)
    }))
}

async fn apply_pipeline_recovery_action(
    state: &ArchitectState,
    session_id: &str,
    action: &str,
    run_id: Option<&str>,
) -> Result<PipelineRecoveryActionResponse, ArchitectError> {
    let normalized = action.trim().to_ascii_lowercase();
    let mut run = interrupted_pipeline_run_for_session(state, session_id, run_id)
        .await?
        .ok_or_else(|| -> ArchitectError {
            format!(
                "no interrupted pipeline run found for session '{session_id}'{}",
                run_id
                    .map(|value| format!(" and run '{value}'"))
                    .unwrap_or_default()
            )
            .into()
        })?;

    run = pipeline_run_after_recovery_action(&run, &normalized)?;
    save_pipeline_run(state, &run).await?;
    if normalized == "discard" {
        state
            .active_pipeline_runs
            .lock()
            .await
            .remove(&run.pipeline_run_id);
    }
    Ok(PipelineRecoveryActionResponse {
        status: "ok".to_string(),
        action: normalized,
        pipeline_run_id: Some(run.pipeline_run_id.clone()),
        current_stage: Some(run.current_stage.to_string()),
        pipeline_recovery: None,
    })
}

fn pipeline_run_after_recovery_action(
    run: &PipelineRunRecord,
    action: &str,
) -> Result<PipelineRunRecord, ArchitectError> {
    let mut updated = run.clone();
    updated.updated_at_ms = now_epoch_ms();
    match action {
        "resume" => {
            updated.status = PipelineRunStatus::InProgress;
            updated.interrupted_at_ms = None;
            Ok(updated)
        }
        "discard" => {
            updated.status = PipelineRunStatus::Failed;
            updated.current_stage = PipelineStage::Failed;
            Ok(updated)
        }
        _ => Err(format!(
            "unsupported pipeline recovery action '{}'; expected 'resume' or 'discard'",
            action
        )
        .into()),
    }
}

// ── TA-5: pipeline state machine ──────────────────────────────────────────

async fn start_pipeline_run_with_context(
    context: &ArchitectAdminToolContext,
    session_id: &str,
    solution_id: Option<&str>,
    task_description: &str,
) -> Result<PipelineRunRecord, ArchitectError> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let now = now_epoch_ms();
    let initial_state = serde_json::json!({
        "task": task_description,
        "solution_id": solution_id,
        "manifest_ref": null,
        "snapshot_ref": null,
        "delta_report_ref": null,
        "approved_artifacts": [],
        "executor_plan_ref": null,
    });
    let record = PipelineRunRecord {
        pipeline_run_id: run_id,
        session_id: session_id.to_string(),
        solution_id: solution_id.map(str::to_string),
        status: PipelineRunStatus::InProgress,
        current_stage: PipelineStage::Design,
        current_loop: 0,
        current_attempt: 0,
        state_json: serde_json::to_string(&initial_state).unwrap_or_else(|_| "{}".to_string()),
        created_at_ms: now,
        updated_at_ms: now,
        interrupted_at_ms: None,
    };
    save_pipeline_run_with_context(context, &record).await?;
    Ok(record)
}

async fn advance_pipeline_run(
    state: &ArchitectState,
    run_id: &str,
    new_stage: PipelineStage,
    state_update: Option<serde_json::Value>,
) -> Result<(), ArchitectError> {
    let mut guard = state.active_pipeline_runs.lock().await;
    if !guard.contains_key(run_id) {
        drop(guard);
        let loaded = load_pipeline_run(state, run_id)
            .await?
            .ok_or_else(|| -> ArchitectError {
                format!("pipeline run {run_id} not found").into()
            })?;
        let mut refill = state.active_pipeline_runs.lock().await;
        refill.insert(run_id.to_string(), loaded);
        guard = refill;
    }
    let run = guard.get_mut(run_id).ok_or_else(|| -> ArchitectError {
        format!("pipeline run {run_id} not found after reload").into()
    })?;
    run.current_stage = new_stage.clone();
    run.status = pipeline_run_status_for_stage(&new_stage);
    run.updated_at_ms = now_epoch_ms();
    if run.status == PipelineRunStatus::Interrupted {
        run.interrupted_at_ms.get_or_insert(run.updated_at_ms);
    } else {
        run.interrupted_at_ms = None;
    }
    if let Some(update) = state_update {
        if let Ok(mut current) = serde_json::from_str::<serde_json::Value>(&run.state_json) {
            if let (Some(c), Some(u)) = (current.as_object_mut(), update.as_object()) {
                for (k, v) in u {
                    c.insert(k.clone(), v.clone());
                }
            }
            run.state_json =
                serde_json::to_string(&current).unwrap_or_else(|_| run.state_json.clone());
        }
    }
    let updated = run.clone();
    drop(guard);
    save_pipeline_run(state, &updated).await?;
    Ok(())
}

fn pipeline_run_status_for_stage(stage: &PipelineStage) -> PipelineRunStatus {
    match stage {
        PipelineStage::Completed => PipelineRunStatus::Completed,
        PipelineStage::Failed => PipelineRunStatus::Failed,
        PipelineStage::Blocked => PipelineRunStatus::Blocked,
        PipelineStage::Interrupted => PipelineRunStatus::Interrupted,
        _ => PipelineRunStatus::InProgress,
    }
}

async fn block_pipeline_run(
    state: &ArchitectState,
    run_id: &str,
    reason: &str,
    failure_class: &FailureClass,
) -> Result<(), ArchitectError> {
    let class_str = serde_json::to_value(failure_class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{failure_class:?}"));
    let pre_block_stage = load_pipeline_run(state, run_id)
        .await
        .ok()
        .flatten()
        .map(|run| run.current_stage);
    advance_pipeline_run(
        state,
        run_id,
        PipelineStage::Blocked,
        Some(serde_json::json!({
            "blocked_reason": reason,
            "blocked_failure_class": class_str,
            "pre_block_stage": pre_block_stage,
        })),
    )
    .await?;
    tracing::warn!(
        pipeline_run_id = %run_id,
        failure_class = %class_str,
        reason = %reason,
        "pipeline run blocked"
    );
    Ok(())
}

async fn advance_pipeline_run_with_context(
    context: &ArchitectAdminToolContext,
    run_id: &str,
    new_stage: PipelineStage,
    state_update: Option<Value>,
) -> Result<PipelineRunRecord, ArchitectError> {
    let loaded = load_pipeline_run_with_context(context, run_id)
        .await?
        .ok_or_else(|| -> ArchitectError { format!("pipeline run {run_id} not found").into() })?;
    let updated = pipeline_run_with_state_update(&loaded, new_stage, state_update, None)?;
    save_pipeline_run_with_context(context, &updated).await?;
    Ok(updated)
}

async fn block_pipeline_run_with_context(
    context: &ArchitectAdminToolContext,
    run_id: &str,
    reason: &str,
    failure_class: &FailureClass,
) -> Result<PipelineRunRecord, ArchitectError> {
    let class_str = serde_json::to_value(failure_class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{failure_class:?}"));
    let pre_block_stage = load_pipeline_run_with_context(context, run_id)
        .await
        .ok()
        .flatten()
        .map(|run| run.current_stage);
    let updated = advance_pipeline_run_with_context(
        context,
        run_id,
        PipelineStage::Blocked,
        Some(json!({
            "blocked_reason": reason,
            "blocked_failure_class": class_str,
            "pre_block_stage": pre_block_stage,
        })),
    )
    .await?;
    tracing::warn!(
        pipeline_run_id = %run_id,
        failure_class = %class_str,
        reason = %reason,
        "pipeline run blocked"
    );
    Ok(updated)
}

/// Resolve the active blocked pipeline run for a session — returns most recent if any.
async fn latest_blocked_pipeline_run_for_session(
    context: &ArchitectAdminToolContext,
    session_id: &str,
) -> Result<Option<PipelineRunRecord>, ArchitectError> {
    if let Some(run) = context
        .active_pipeline_runs
        .lock()
        .await
        .values()
        .filter(|run| {
            run.session_id == session_id && run.status == PipelineRunStatus::Blocked
        })
        .cloned()
        .max_by_key(|run| (run.updated_at_ms, run.created_at_ms))
    {
        return Ok(Some(run));
    }
    Ok(
        list_pipeline_runs_for_session_by_hive(&context.hive_id, session_id)
            .await?
            .into_iter()
            .rev()
            .find(|run| run.status == PipelineRunStatus::Blocked),
    )
}

/// Resolve a blocked run by id (preferred) or by session (latest blocked).
async fn resolve_blocked_pipeline_run(
    context: &ArchitectAdminToolContext,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<PipelineRunRecord, ArchitectError> {
    if let Some(id) = run_id.filter(|s| !s.trim().is_empty()) {
        let run = load_pipeline_run_with_context(context, id)
            .await?
            .ok_or_else(|| -> ArchitectError {
                format!("pipeline run '{id}' not found").into()
            })?;
        if run.session_id != session_id {
            return Err(format!(
                "pipeline run '{id}' does not belong to this session"
            )
            .into());
        }
        if run.status != PipelineRunStatus::Blocked {
            return Err(format!(
                "pipeline run '{id}' is in status '{:?}', not 'blocked'; pipeline_action only operates on blocked runs",
                run.status
            )
            .into());
        }
        return Ok(run);
    }
    latest_blocked_pipeline_run_for_session(context, session_id)
        .await?
        .ok_or_else(|| -> ArchitectError {
            "no blocked pipeline run in this session to act on"
                .to_string()
                .into()
        })
}

/// Map a `pre_block_stage` to the CONFIRM checkpoint where retry should park the run.
/// Pure for unit testing.
fn retry_target_stage_for(pre_block_stage: Option<&PipelineStage>) -> Result<PipelineStage, String> {
    match pre_block_stage {
        Some(PipelineStage::Reconcile)
        | Some(PipelineStage::ArtifactLoop)
        | Some(PipelineStage::PlanCompile)
        | Some(PipelineStage::PlanValidation)
        | Some(PipelineStage::Design)
        | Some(PipelineStage::DesignAudit)
        | Some(PipelineStage::Confirm1) => Ok(PipelineStage::Confirm1),
        Some(PipelineStage::Execute)
        | Some(PipelineStage::Verify)
        | Some(PipelineStage::Confirm2) => Ok(PipelineStage::Confirm2),
        Some(other) => Err(format!(
            "cannot retry from stage '{other:?}' — restart_from_design or discard instead"
        )),
        None => Err(
            "blocked pipeline has no recorded pre_block_stage; use restart_from_design or discard"
                .to_string(),
        ),
    }
}

/// Apply one of the operator-options actions to a blocked pipeline run.
/// `action` must be one of: "discard", "restart_from_design", "retry".
async fn apply_pipeline_action_with_context(
    context: &ArchitectAdminToolContext,
    session_id: &str,
    run_id: Option<&str>,
    action: &str,
) -> Result<Value, ArchitectError> {
    let run = resolve_blocked_pipeline_run(context, session_id, run_id).await?;
    let run_id = run.pipeline_run_id.clone();
    let run_state = pipeline_state_from_run(&run);
    let pre_block_stage: Option<PipelineStage> = run_state
        .get("pre_block_stage")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    match action {
        "discard" | "cancel_pipeline" => {
            advance_pipeline_run_with_context(
                context,
                &run_id,
                PipelineStage::Failed,
                Some(json!({
                    "discarded_by_operator": true,
                    "discarded_at_ms": now_epoch_ms(),
                })),
            )
            .await?;
            Ok(json!({
                "status": "ok",
                "action": "discard",
                "pipeline_run_id": run_id,
                "message": format!(
                    "Pipeline run {run_id} discarded. You can start a new pipeline now."
                ),
            }))
        }
        "restart_from_design" => {
            let task = run_state
                .get("task")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| -> ArchitectError {
                    format!("blocked pipeline run '{run_id}' has no original task to restart from")
                        .into()
                })?;
            let solution_id = run.solution_id.clone();
            advance_pipeline_run_with_context(
                context,
                &run_id,
                PipelineStage::Failed,
                Some(json!({
                    "discarded_by_operator": true,
                    "discarded_for_restart": true,
                    "discarded_at_ms": now_epoch_ms(),
                })),
            )
            .await?;
            let restart = execute_pipeline_start_with_context(
                context,
                session_id,
                &task,
                solution_id.as_deref(),
                "",
            )
            .await?;
            Ok(json!({
                "status": "ok",
                "action": "restart_from_design",
                "previous_pipeline_run_id": run_id,
                "restart_payload": restart,
                "message": format!(
                    "Previous pipeline {run_id} closed; new pipeline started from Design."
                ),
            }))
        }
        "retry" | "fix_manifest_and_retry" => {
            let target_stage = retry_target_stage_for(pre_block_stage.as_ref())
                .map_err(|err| -> ArchitectError { err.into() })?;
            advance_pipeline_run_with_context(
                context,
                &run_id,
                target_stage.clone(),
                Some(json!({
                    "retried_by_operator": true,
                    "retried_at_ms": now_epoch_ms(),
                })),
            )
            .await?;
            let confirm_label = match target_stage {
                PipelineStage::Confirm1 => "Confirm1",
                PipelineStage::Confirm2 => "Confirm2",
                _ => "Confirm",
            };
            Ok(json!({
                "status": "ok",
                "action": "retry",
                "pipeline_run_id": run_id,
                "stage": target_stage,
                "message": format!(
                    "Pipeline {run_id} returned to {confirm_label}. Reply CONFIRM to re-engage execution."
                ),
            }))
        }
        other => Err(format!(
            "unknown pipeline action '{other}'; expected 'discard', 'restart_from_design', or 'retry'"
        )
        .into()),
    }
}

/// On startup: mark any in_progress runs as interrupted so they surface for recovery.
async fn mark_interrupted_pipeline_runs(state: &ArchitectState) {
    match list_interrupted_runs_for_startup(state).await {
        Ok(runs) => {
            for mut run in runs {
                run.status = PipelineRunStatus::Interrupted;
                run.interrupted_at_ms = Some(now_epoch_ms());
                run.updated_at_ms = now_epoch_ms();
                if let Err(err) = save_pipeline_run(state, &run).await {
                    tracing::warn!(error = %err, run_id = %run.pipeline_run_id, "failed to mark pipeline run as interrupted");
                }
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "could not check for interrupted pipeline runs on startup");
        }
    }
}

/// Returns the active pipeline run for a session at a given stage, if any.
async fn active_pipeline_at_stage(
    state: &ArchitectState,
    session_id: &str,
    stage: &PipelineStage,
) -> Option<PipelineRunRecord> {
    if let Some(run) = state
        .active_pipeline_runs
        .lock()
        .await
        .values()
        .find(|r| {
            r.session_id == session_id
                && &r.current_stage == stage
                && r.status == PipelineRunStatus::InProgress
        })
        .cloned()
    {
        return Some(run);
    }

    list_pipeline_runs_for_session(state, session_id)
        .await
        .ok()
        .and_then(|runs| {
            runs.into_iter()
                .rev()
                .find(|r| &r.current_stage == stage && r.status == PipelineRunStatus::InProgress)
        })
}

fn pipeline_state_from_run(run: &PipelineRunRecord) -> serde_json::Value {
    serde_json::from_str(&run.state_json).unwrap_or_else(|_| json!({}))
}

fn pipeline_task_summary(
    run_state: &Value,
    solution_id: Option<&str>,
    delta_report: &DeltaReport,
) -> String {
    let base = run_state
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            solution_id.map(|solution| {
                format!(
                    "Compile executor plan for solution '{solution}' from reconciler delta_report"
                )
            })
        })
        .unwrap_or_else(|| "Compile executor plan from reconciler delta_report".to_string());
    format!("{base} [{} operation(s)]", delta_report.operations.len())
}

fn delta_expected_outcome_summary(delta_report: &DeltaReport) -> String {
    let summary = &delta_report.summary;
    let mut parts = Vec::new();
    if summary.creates > 0 {
        parts.push(format!("{} create(s)", summary.creates));
    }
    if summary.updates > 0 {
        parts.push(format!("{} update(s)", summary.updates));
    }
    if summary.deletes > 0 {
        parts.push(format!("{} delete(s)", summary.deletes));
    }
    if summary.noops > 0 {
        parts.push(format!("{} noop(s)", summary.noops));
    }
    if summary.blocked > 0 {
        parts.push(format!("{} blocked op(s)", summary.blocked));
    }
    if parts.is_empty() {
        "No effective changes.".to_string()
    } else {
        format!("Expected outcome: {}.", parts.join(", "))
    }
}

fn manifest_solution_name(manifest: &SolutionManifestV2) -> String {
    manifest
        .solution
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            manifest
                .solution
                .get("solution_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unnamed solution".to_string())
}

fn advisory_highlights(advisory: &Value) -> Vec<String> {
    let mut highlights = Vec::new();
    let mut push_text = |text: &str| {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            highlights.push(trimmed.to_string());
        }
    };

    match advisory {
        Value::String(text) => push_text(text),
        Value::Array(items) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    push_text(text);
                }
            }
        }
        Value::Object(map) => {
            for key in ["highlights", "warnings", "risks", "notes"] {
                if let Some(value) = map.get(key) {
                    match value {
                        Value::String(text) => push_text(text),
                        Value::Array(items) => {
                            for item in items {
                                if let Some(text) = item.as_str() {
                                    push_text(text);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }

    highlights.sort();
    highlights.dedup();
    highlights.truncate(5);
    highlights
}

fn desired_runtime_names_from_manifest(manifest: &SolutionManifestV2) -> Vec<String> {
    let mut runtimes = manifest
        .desired_state
        .runtimes
        .as_ref()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<DesiredRuntime>(item.clone()).ok())
                .map(|runtime| runtime.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if runtimes.is_empty() {
        runtimes.extend(
            manifest
                .desired_state
                .nodes
                .as_ref()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| serde_json::from_value::<DesiredNode>(item.clone()).ok())
                        .map(|node| node.runtime)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
    }

    runtimes.sort();
    runtimes.dedup();
    runtimes
}

fn build_confirm1_summary(
    manifest: &SolutionManifestV2,
    verdict: &DesignAuditVerdict,
) -> Confirm1Summary {
    let hive_count = manifest
        .desired_state
        .topology
        .as_ref()
        .and_then(|topology| topology.get("hives"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let node_count = manifest
        .desired_state
        .nodes
        .as_ref()
        .map(Vec::len)
        .unwrap_or(0);
    let route_count = manifest
        .desired_state
        .routing
        .as_ref()
        .map(Vec::len)
        .unwrap_or(0);
    let main_runtimes = desired_runtime_names_from_manifest(manifest);
    let advisory_highlights = advisory_highlights(&manifest.advisory);
    let warnings = verdict
        .findings
        .iter()
        .filter(|finding| matches!(finding.severity, AuditSeverity::Warning))
        .map(|finding| format!("{}: {}", finding.section, finding.message))
        .collect::<Vec<_>>();
    let solution_name = manifest_solution_name(manifest);
    let message = format!(
        "Design review for {}: {} hive(s), {} node(s), {} route(s), audit score {}/10.",
        solution_name, hive_count, node_count, route_count, verdict.score
    );

    Confirm1Summary {
        solution_name,
        hive_count,
        node_count,
        route_count,
        main_runtimes,
        audit_status: verdict.status.clone(),
        audit_score: verdict.score,
        blocking_issues: verdict.blocking_issues.clone(),
        advisory_highlights,
        warnings,
        message,
    }
}

fn summarize_delta_operations_by_risk(
    delta_report: &DeltaReport,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut destructive = Vec::new();
    let mut restarting = Vec::new();
    let mut blocked = Vec::new();
    for op in &delta_report.operations {
        let entry = json!({
            "op_id": op.op_id,
            "resource_type": op.resource_type,
            "resource_id": op.resource_id,
            "compiler_class": op.compiler_class,
            "change_type": op.change_type,
            "notes": op.notes,
        });
        match compiler_class_risk(&op.compiler_class) {
            RiskClass::Destructive => destructive.push(entry),
            RiskClass::Restarting => restarting.push(entry),
            RiskClass::BlockedUntilOperator => blocked.push(entry),
            RiskClass::NonDestructive => {}
        }
    }
    (destructive, restarting, blocked)
}

fn build_confirm2_payload(
    pipeline_run_id: &str,
    delta_report: &DeltaReport,
    human_summary: &str,
    trace: &PlanCompileTrace,
) -> Value {
    let (destructive_actions, restarting_actions, blocked_actions) =
        summarize_delta_operations_by_risk(delta_report);
    json!({
        "message": "Plan compiled and validated. Reply CONFIRM to execute or CANCEL to discard.",
        "pipeline_run_id": pipeline_run_id,
        "stage": "confirm2",
        "human_summary": human_summary,
        "expected_outcome": delta_expected_outcome_summary(delta_report),
        "validation": {
            "status": trace.validation,
            "step_count": trace.step_count,
            "first_validation_error": trace.first_validation_error,
        },
        "destructive_actions": destructive_actions,
        "restarting_actions": restarting_actions,
        "blocked_actions": blocked_actions,
    })
}

fn executor_output_is_successful(output: &Value) -> bool {
    let transport_ok = output
        .get("status")
        .and_then(Value::as_str)
        .map(|value| value.eq_ignore_ascii_case("ok"))
        .unwrap_or(false);
    if !transport_ok {
        return false;
    }
    !matches!(
        output
            .get("payload")
            .and_then(|payload| payload.get("summary"))
            .and_then(|summary| summary.get("status"))
            .and_then(Value::as_str),
        Some("failed" | "stopped" | "error")
    )
}

fn executor_failure_text(output: &Value) -> String {
    response_detail_text(output).unwrap_or_else(|| "executor plan failed".to_string())
}

fn verify_execution_result(execution_report: &Value) -> VerificationVerdict {
    let summary = execution_report
        .get("payload")
        .and_then(|payload| payload.get("summary"));
    let status = summary
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str);
    let failed_step_present = summary
        .and_then(|value| value.get("failed_step_id"))
        .map(|value| !value.is_null())
        .unwrap_or(false);
    let completed_steps = summary
        .and_then(|value| value.get("completed_steps"))
        .and_then(Value::as_u64);
    let total_steps = summary
        .and_then(|value| value.get("total_steps"))
        .and_then(Value::as_u64);

    let verified_success = matches!(status, Some("done")) && !failed_step_present;
    let eligible_for_cookbook = verified_success
        && matches!((completed_steps, total_steps), (Some(completed), Some(total)) if completed == total);

    let reason = if !verified_success {
        match status {
            Some(value) if !value.is_empty() => {
                format!("execution report is not verified successful: summary.status={value}")
            }
            _ if failed_step_present => {
                "execution report is not verified successful: failed_step_id is present".to_string()
            }
            _ => "execution report is not verified successful".to_string(),
        }
    } else {
        match (completed_steps, total_steps) {
            (Some(completed), Some(total)) if completed == total => {
                "execution report verified; eligible for cookbook".to_string()
            }
            (Some(completed), Some(total)) => format!(
                "execution verified but cookbook write skipped: completed_steps={completed} total_steps={total}"
            ),
            _ => "execution verified but cookbook write skipped: step counts are missing"
                .to_string(),
        }
    };

    VerificationVerdict {
        eligible_for_cookbook,
        reason,
        verified_success,
    }
}

fn pipeline_approved_artifacts_from_run_state(
    run_state: &Value,
) -> Vec<artifact_loop::ApprovedArtifact> {
    run_state
        .get("approved_artifacts")
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<Vec<artifact_loop::ApprovedArtifact>>(value).ok()
        })
        .unwrap_or_default()
}

fn observed_artifact_cookbook_entry(
    state_dir: &Path,
    pipeline_run_id: &str,
    approved: &artifact_loop::ApprovedArtifact,
) -> Option<CookbookEntryV2> {
    let bundle =
        artifact_loop::load_artifact_bundle(state_dir, pipeline_run_id, &approved.bundle_id)?;
    let task_packet = std::fs::read_to_string(artifact_loop::task_packet_path(
        state_dir,
        pipeline_run_id,
        &approved.task_id,
    ))
    .ok()
    .and_then(|raw| serde_json::from_str::<BuildTaskPacket>(&raw).ok());
    let runtime_name = task_packet
        .as_ref()
        .and_then(|packet| packet.runtime_name.clone());
    let target_kind = task_packet
        .as_ref()
        .map(|packet| packet.target_kind.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let artifact_keys = bundle
        .artifact
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    Some(CookbookEntryV2 {
        layer: CookbookLayer::Artifact,
        pattern_key: sanitize_pattern_key(&format!(
            "{}_{}_{}",
            approved.artifact_kind,
            target_kind,
            runtime_name
                .clone()
                .unwrap_or_else(|| "artifact".to_string())
        )),
        trigger: format!(
            "When reconciler requires a {} artifact{}.",
            approved.artifact_kind,
            runtime_name
                .as_deref()
                .map(|value| format!(" for runtime {value}"))
                .unwrap_or_default()
        ),
        inputs_signature: json!({
            "artifact_kind": approved.artifact_kind,
            "target_kind": target_kind,
            "runtime_name": runtime_name,
            "source_delta_op": approved.source_delta_op,
        }),
        successful_shape: json!({
            "summary": bundle.summary,
            "artifact_keys": artifact_keys,
            "content_digest": approved.content_digest,
        }),
        constraints: vec![],
        do_not_repeat: vec![],
        failure_class: None,
        recorded_from_run: Some(pipeline_run_id.to_string()),
        seed_kind: "observed".to_string(),
    })
}

fn observed_design_cookbook_entry_from_run_state(
    run_state: &Value,
    pipeline_run_id: &str,
) -> Option<CookbookEntryV2> {
    let solution_id = run_state
        .get("solution_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let summary = run_state
        .get("confirm1_summary")
        .cloned()
        .and_then(|value| serde_json::from_value::<Confirm1Summary>(value).ok())?;
    let verdict = run_state
        .get("design_audit_verdict")
        .cloned()
        .and_then(|value| serde_json::from_value::<DesignAuditVerdict>(value).ok())?;
    if verdict.status != DesignAuditStatus::Pass {
        return None;
    }

    Some(CookbookEntryV2 {
        layer: CookbookLayer::Design,
        pattern_key: sanitize_pattern_key(&format!(
            "design_{}_{}_{}_{}",
            solution_id,
            summary.hive_count,
            summary.node_count,
            summary.main_runtimes.join("_")
        )),
        trigger: format!(
            "When designing solution '{}' with {} hive(s) and {} node(s).",
            summary.solution_name, summary.hive_count, summary.node_count
        ),
        inputs_signature: json!({
            "solution_id": solution_id,
            "solution_name": summary.solution_name,
            "hive_count": summary.hive_count,
            "node_count": summary.node_count,
            "route_count": summary.route_count,
            "main_runtimes": summary.main_runtimes,
        }),
        successful_shape: json!({
            "audit_score": verdict.score,
            "audit_status": verdict.status,
            "design_iterations_used": run_state.get("design_iterations_used").cloned().unwrap_or(Value::Null),
            "design_stop_reason": run_state.get("design_stop_reason").cloned().unwrap_or(Value::Null),
            "message": summary.message,
            "advisory_highlights": summary.advisory_highlights,
        }),
        constraints: summary.warnings,
        do_not_repeat: verdict.blocking_issues,
        failure_class: None,
        recorded_from_run: Some(pipeline_run_id.to_string()),
        seed_kind: "observed".to_string(),
    })
}

fn pipeline_cookbook_entry_from_run_state(run_state: &Value) -> Option<CookbookEntryV2> {
    run_state
        .get("plan_compile_cookbook_entry")
        .cloned()
        .and_then(|value| serde_json::from_value::<CookbookEntryV2>(value).ok())
        .map(normalize_plan_compile_cookbook_entry)
}

fn observed_plan_compile_cookbook_entry(
    entry: &CookbookEntryV2,
    pipeline_run_id: &str,
) -> CookbookEntryV2 {
    let mut observed = normalize_plan_compile_cookbook_entry(entry.clone());
    observed.recorded_from_run = Some(pipeline_run_id.to_string());
    observed.seed_kind = "observed".to_string();
    observed
}

fn artifact_content_digest(value: &Value) -> String {
    let normalized = serde_json::to_vec(&normalize_json(value)).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(normalized);
    format!("sha256:{:x}", hasher.finalize())
}

fn runtime_package_file_map_from_value(
    artifact: &Value,
) -> Result<serde_json::Map<String, Value>, String> {
    let files = artifact
        .get("files")
        .and_then(Value::as_object)
        .ok_or_else(|| "artifact.files must be an object".to_string())?;
    let mut out = serde_json::Map::new();
    for (path, value) in files {
        let Some(content) = value.as_str() else {
            return Err(format!("artifact file '{}' must be a string", path));
        };
        out.insert(path.clone(), Value::String(content.to_string()));
    }
    Ok(out)
}

fn runtime_package_publish_source_inline(
    files: &serde_json::Map<String, Value>,
) -> Result<Value, String> {
    for (path, value) in files {
        if !value.is_string() {
            return Err(format!("artifact file '{}' must be a string", path));
        }
    }
    Ok(json!({
        "kind": "inline_package",
        "files": Value::Object(files.clone()),
    }))
}

fn runtime_package_name_from_packet_or_files(
    packet: &BuildTaskPacket,
    files: &serde_json::Map<String, Value>,
) -> String {
    packet
        .runtime_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            packet
                .requirements
                .get("runtime_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            files
                .get("package.json")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|pkg| {
                    pkg.get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                })
        })
        .unwrap_or_else(|| "runtime.package".to_string())
}

fn runtime_package_version_from_packet_or_files(
    packet: &BuildTaskPacket,
    files: &serde_json::Map<String, Value>,
) -> String {
    packet
        .requirements
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            files
                .get("package.json")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .and_then(|pkg| {
                    pkg.get("version")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                })
        })
        .unwrap_or_else(|| "0.1.0".to_string())
}

fn runtime_package_bundle_root_dir(runtime_name: &str) -> String {
    let trimmed = runtime_name.trim();
    if trimmed.is_empty() {
        "runtime-package".to_string()
    } else {
        trimmed.to_string()
    }
}

fn sanitize_runtime_bundle_upload_filename(runtime_name: &str, version: &str) -> String {
    let runtime_part = sanitize_exec_name(&runtime_name.replace('.', "-"), runtime_name);
    let version_part = version
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("{runtime_part}-{version_part}.zip")
}

fn normalize_artifact_relative_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Err("artifact file path must not be empty".to_string());
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(format!("artifact file path '{}' must be relative", path));
    }
    let mut normalized = Vec::new();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(value) => {
                let segment = value.to_string_lossy();
                if segment.is_empty() {
                    return Err(format!("artifact file path '{}' is invalid", path));
                }
                normalized.push(segment.to_string());
            }
            std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "artifact file path '{}' contains unsupported path traversal",
                    path
                ));
            }
        }
    }
    if normalized.is_empty() {
        return Err(format!("artifact file path '{}' is invalid", path));
    }
    Ok(normalized.join("/"))
}

fn runtime_package_file_mode(path: &str) -> u32 {
    if path.starts_with("bin/") || path.ends_with(".sh") {
        0o755
    } else {
        0o644
    }
}

fn build_runtime_bundle_zip_from_files(
    files: &serde_json::Map<String, Value>,
    root_dir: &str,
) -> Result<Vec<u8>, String> {
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(cursor);
    writer
        .add_directory(
            format!("{root_dir}/"),
            zip::write::FileOptions::default().unix_permissions(0o755),
        )
        .map_err(|err| format!("failed to create bundle root directory in zip: {err}"))?;

    let mut paths = files.keys().cloned().collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let normalized = normalize_artifact_relative_path(&path)?;
        let content = files
            .get(&path)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("artifact file '{}' must be a string", path))?;
        writer
            .start_file(
                format!("{root_dir}/{normalized}"),
                zip::write::FileOptions::default()
                    .unix_permissions(runtime_package_file_mode(&normalized)),
            )
            .map_err(|err| format!("failed to start zip entry '{}': {err}", normalized))?;
        writer
            .write_all(content.as_bytes())
            .map_err(|err| format!("failed to write zip entry '{}': {err}", normalized))?;
    }

    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|err| format!("failed to finalize runtime bundle zip: {err}"))
}

fn publish_runtime_bundle_blob(
    blob_root: &Path,
    filename: &str,
    zip_bytes: &[u8],
) -> Result<(String, String), String> {
    let mut cfg = BlobConfig::default();
    cfg.blob_root = blob_root.to_path_buf();
    let toolkit = BlobToolkit::new(cfg)
        .map_err(|err| format!("failed to initialize blob toolkit for artifact bundle: {err}"))?;
    let blob_ref = toolkit
        .put_bytes(zip_bytes, filename, "application/zip")
        .and_then(|blob_ref| {
            toolkit.promote(&blob_ref)?;
            Ok(blob_ref)
        })
        .map_err(|err| {
            format!(
                "failed to persist runtime bundle upload '{}': {err}",
                filename
            )
        })?;
    let rel_path = format!(
        "active/{}/{}",
        BlobToolkit::prefix(&blob_ref.blob_name),
        blob_ref.blob_name
    );
    Ok((blob_ref.blob_name, rel_path))
}

fn finalize_runtime_package_bundle_for_target(
    blob_root: &Path,
    packet: &BuildTaskPacket,
    bundle: &mut ArtifactBundle,
) -> Result<(), String> {
    let files = runtime_package_file_map_from_value(&bundle.artifact)?;
    let publish_source = match packet.target_kind.as_str() {
        "inline_package" => runtime_package_publish_source_inline(&files)?,
        "bundle_upload" => {
            let runtime_name = runtime_package_name_from_packet_or_files(packet, &files);
            let version = runtime_package_version_from_packet_or_files(packet, &files);
            let root_dir = runtime_package_bundle_root_dir(&runtime_name);
            let zip_bytes = build_runtime_bundle_zip_from_files(&files, &root_dir)?;
            let filename = sanitize_runtime_bundle_upload_filename(&runtime_name, &version);
            let (blob_name, blob_path) =
                publish_runtime_bundle_blob(blob_root, &filename, &zip_bytes)?;
            json!({
                "kind": "bundle_upload",
                "blob_path": blob_path,
                "blob_name": blob_name,
            })
        }
        other => {
            return Err(format!(
                "runtime_package artifact finalization does not support target_kind '{}'",
                other
            ));
        }
    };
    bundle.artifact["publish_source"] = publish_source;
    bundle.content_digest = artifact_content_digest(&bundle.artifact);
    Ok(())
}

fn expand_approved_artifacts_for_plan_compile(
    state_dir: &Path,
    pipeline_run_id: &str,
    approved_artifacts: &[artifact_loop::ApprovedArtifact],
) -> Result<Value, String> {
    let mut expanded = Vec::new();
    for approved in approved_artifacts {
        let bundle =
            artifact_loop::load_artifact_bundle(state_dir, pipeline_run_id, &approved.bundle_id)
                .ok_or_else(|| {
                    format!(
                        "approved artifact bundle '{}' could not be loaded from disk",
                        approved.bundle_id
                    )
                })?;
        let packet = std::fs::read_to_string(artifact_loop::task_packet_path(
            state_dir,
            pipeline_run_id,
            &approved.task_id,
        ))
        .map_err(|err| {
            format!(
                "approved artifact task packet '{}' could not be read: {err}",
                approved.task_id
            )
        })
        .and_then(|raw| {
            serde_json::from_str::<BuildTaskPacket>(&raw).map_err(|err| {
                format!(
                    "approved artifact task packet '{}' is invalid JSON: {err}",
                    approved.task_id
                )
            })
        })?;

        let publish_source = if let Some(value) = bundle.artifact.get("publish_source") {
            value.clone()
        } else if approved.artifact_kind == "runtime_package"
            && packet.target_kind == "inline_package"
        {
            let files = runtime_package_file_map_from_value(&bundle.artifact)?;
            runtime_package_publish_source_inline(&files)?
        } else {
            return Err(format!(
                "approved artifact '{}' is missing publish_source for target_kind '{}'",
                approved.bundle_id, packet.target_kind
            ));
        };

        expanded.push(json!({
            "bundle_id": approved.bundle_id,
            "task_id": approved.task_id,
            "source_delta_op": approved.source_delta_op,
            "artifact_kind": approved.artifact_kind,
            "target_kind": packet.target_kind,
            "runtime_name": packet.runtime_name,
            "content_digest": approved.content_digest,
            "summary": bundle.summary,
            "verification_hints": bundle.verification_hints,
            "publish_source": publish_source,
        }));
    }
    Ok(Value::Array(expanded))
}

fn build_real_programmer_request(packet: &BuildTaskPacket, repair: Option<&RepairPacket>) -> Value {
    let mut request = json!({
        "task_id": packet.task_id,
        "artifact_kind": packet.artifact_kind,
        "source_delta_op": packet.source_delta_op,
        "runtime_name": packet.runtime_name,
        "target_kind": packet.target_kind,
        "requirements": packet.requirements,
        "constraints": packet.constraints,
        "known_context": packet.known_context,
        "attempt": packet.attempt,
        "max_attempts": packet.max_attempts,
        "cookbook_context": packet.cookbook_context,
    });
    if let Some(repair_packet) = repair {
        request["repair_packet"] = serde_json::to_value(repair_packet).unwrap_or(Value::Null);
    }
    request
}

fn artifact_bundle_from_real_programmer_submission(
    packet: &BuildTaskPacket,
    submitted: &Value,
) -> Result<ArtifactBundle, String> {
    let artifact = submitted
        .get("artifact")
        .and_then(Value::as_object)
        .ok_or_else(|| "submit_artifact_bundle missing artifact object".to_string())?;
    if packet.artifact_kind != "runtime_package" {
        return Err(format!(
            "real programmer bundle conversion does not yet support artifact_kind '{}'",
            packet.artifact_kind
        ));
    }
    if packet.target_kind != "inline_package" && packet.target_kind != "bundle_upload" {
        return Err(format!(
            "real programmer bundle conversion currently supports inline_package and bundle_upload targets, got '{}'",
            packet.target_kind
        ));
    }
    let file_map = runtime_package_file_map_from_value(&Value::Object(artifact.clone()))?;
    let publish_source = if packet.target_kind == "inline_package" {
        runtime_package_publish_source_inline(&file_map)?
    } else {
        Value::Null
    };
    let artifact_payload = json!({
        "files": Value::Object(file_map),
        "publish_source": publish_source,
    });
    let summary = submitted
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Generated artifact bundle.")
        .to_string();
    let assumptions = submitted
        .get("assumptions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let verification_hints = submitted
        .get("verification_hints")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ArtifactBundle {
        bundle_version: "0.1".to_string(),
        bundle_id: format!("bundle-{}", Uuid::new_v4().simple()),
        source_task_id: packet.task_id.clone(),
        artifact_kind: packet.artifact_kind.clone(),
        status: pipeline_types::ArtifactBundleStatus::Pending,
        summary,
        artifact: artifact_payload.clone(),
        assumptions,
        verification_hints,
        content_digest: artifact_content_digest(&artifact_payload),
        generated_at_ms: now_epoch_ms(),
        generator_model: "real_programmer".to_string(),
    })
}

async fn run_pipeline_artifact_loop(
    state: &ArchitectState,
    session: &ChatSessionRecord,
    pipeline_run: &PipelineRunRecord,
    build_task_packets: &[BuildTaskPacket],
    initial_tokens_used: u32,
) -> Result<artifact_loop::ArtifactLoopResult, ArchitectError> {
    let tool_ctx = admin_tool_context(state, Some(&session.session_id));
    run_pipeline_artifact_loop_with_context(
        &tool_ctx,
        pipeline_run,
        build_task_packets,
        initial_tokens_used,
    )
    .await
}

async fn run_pipeline_artifact_loop_with_context(
    context: &ArchitectAdminToolContext,
    pipeline_run: &PipelineRunRecord,
    build_task_packets: &[BuildTaskPacket],
    initial_tokens_used: u32,
) -> Result<artifact_loop::ArtifactLoopResult, ArchitectError> {
    let blob_root = architect_blob_root_from_config_dir(&context.config_dir)?;
    let mut approved = Vec::new();
    let mut trace_events: Vec<artifact_loop::ArtifactLoopTraceEvent> = Vec::new();
    let mut token_budget = TaskTokenBudget::new(initial_tokens_used);
    let mut tokens_accumulated: u32 = 0;

    for packet in build_task_packets {
        artifact_loop::save_task_packet(&context.state_dir, &pipeline_run.pipeline_run_id, packet)
            .map_err(|err| -> ArchitectError { err.into() })?;

        if packet.artifact_kind != "runtime_package" {
            return Ok(artifact_loop::ArtifactLoopResult {
                failed_task_id: Some(packet.task_id.clone()),
                failure_class: Some(FailureClass::ArtifactTaskUnderspecified),
                error: Some(format!(
                    "artifact loop does not yet support artifact_kind '{}'",
                    packet.artifact_kind
                )),
                tokens_used: tokens_accumulated,
            });
        }
        if packet.target_kind != "inline_package" && packet.target_kind != "bundle_upload" {
            return Ok(artifact_loop::ArtifactLoopResult {
                failed_task_id: Some(packet.task_id.clone()),
                failure_class: Some(FailureClass::ArtifactTaskUnderspecified),
                error: Some(format!(
                    "artifact loop currently supports runtime_package task packets with inline_package or bundle_upload targets; '{}' is not yet implemented",
                    packet.target_kind
                )),
                tokens_used: tokens_accumulated,
            });
        }

        let mut repeated_failure_signature: Option<String> = None;
        let mut repair_packet: Option<RepairPacket> = None;
        let mut attempt = 1u32;
        let max_attempts = packet.max_attempts.max(1);

        while attempt <= max_attempts {
            trace_events.push(artifact_loop::ArtifactLoopTraceEvent {
                task_id: packet.task_id.clone(),
                artifact_kind: packet.artifact_kind.clone(),
                attempt,
                status: "generating".to_string(),
                verdict: None,
                failure_class: None,
                findings_count: 0,
                bundle_id: None,
                timestamp_ms: now_epoch_ms(),
            });

            if let Err(err) = token_budget.ensure_room("artifact.programmer") {
                return Ok(artifact_loop::ArtifactLoopResult {
                    failed_task_id: Some(packet.task_id.clone()),
                    failure_class: Some(FailureClass::UnknownResidual),
                    error: Some(err.to_string()),
                    tokens_used: tokens_accumulated,
                });
            }
            let (bundle, programmer_tokens) =
                match run_real_programmer_with_context(context, packet, repair_packet.as_ref())
                    .await
                {
                    Ok(result) => result,
                    Err(err) => {
                        return Ok(artifact_loop::ArtifactLoopResult {
                            failed_task_id: Some(packet.task_id.clone()),
                            failure_class: Some(FailureClass::UnknownResidual),
                            error: Some(format!("artifact generation failed: {err}")),
                            tokens_used: tokens_accumulated,
                        });
                    }
                };
            tokens_accumulated = tokens_accumulated.saturating_add(programmer_tokens);
            if let Err(err) = token_budget.add("artifact.programmer", programmer_tokens) {
                return Ok(artifact_loop::ArtifactLoopResult {
                    failed_task_id: Some(packet.task_id.clone()),
                    failure_class: Some(FailureClass::UnknownResidual),
                    error: Some(err.to_string()),
                    tokens_used: tokens_accumulated,
                });
            }
            let mut bundle = bundle;

            trace_events.push(artifact_loop::ArtifactLoopTraceEvent {
                task_id: packet.task_id.clone(),
                artifact_kind: packet.artifact_kind.clone(),
                attempt,
                status: "auditing".to_string(),
                verdict: None,
                failure_class: None,
                findings_count: 0,
                bundle_id: Some(bundle.bundle_id.clone()),
                timestamp_ms: now_epoch_ms(),
            });

            let verdict = artifact_loop::audit_artifact(packet, &bundle);
            bundle.status = match verdict.status {
                pipeline_types::AuditStatus::Approved => {
                    pipeline_types::ArtifactBundleStatus::Approved
                }
                pipeline_types::AuditStatus::Rejected => {
                    pipeline_types::ArtifactBundleStatus::Rejected
                }
                pipeline_types::AuditStatus::Repairable => {
                    pipeline_types::ArtifactBundleStatus::Repairable
                }
            };

            trace_events.push(artifact_loop::ArtifactLoopTraceEvent {
                task_id: packet.task_id.clone(),
                artifact_kind: packet.artifact_kind.clone(),
                attempt,
                status: match verdict.status {
                    pipeline_types::AuditStatus::Approved => "approved".to_string(),
                    pipeline_types::AuditStatus::Rejected => "rejected".to_string(),
                    pipeline_types::AuditStatus::Repairable => "repairable".to_string(),
                },
                verdict: Some(format!("{:?}", verdict.status)),
                failure_class: verdict
                    .failure_class
                    .as_ref()
                    .map(|value| format!("{value:?}")),
                findings_count: verdict.findings.len() as u32,
                bundle_id: Some(bundle.bundle_id.clone()),
                timestamp_ms: now_epoch_ms(),
            });

            match verdict.status {
                pipeline_types::AuditStatus::Approved => {
                    if let Err(err) =
                        finalize_runtime_package_bundle_for_target(&blob_root, packet, &mut bundle)
                    {
                        let _ = advance_pipeline_run_with_context(
                            context,
                            &pipeline_run.pipeline_run_id,
                            PipelineStage::ArtifactLoop,
                            Some(json!({
                                "artifact_loop_trace": serde_json::to_value(&trace_events).unwrap_or(Value::Null),
                            })),
                        )
                        .await;
                        return Ok(artifact_loop::ArtifactLoopResult {
                            failed_task_id: Some(packet.task_id.clone()),
                            failure_class: Some(FailureClass::ArtifactContractInvalid),
                            error: Some(format!(
                                "approved artifact could not be finalized for target '{}': {err}",
                                packet.target_kind
                            )),
                            tokens_used: tokens_accumulated,
                        });
                    }
                    artifact_loop::save_artifact_bundle(
                        &context.state_dir,
                        &pipeline_run.pipeline_run_id,
                        &bundle,
                    )
                    .map_err(|err| -> ArchitectError { err.into() })?;
                    approved.push(artifact_loop::ApprovedArtifact {
                        bundle_id: bundle.bundle_id.clone(),
                        task_id: packet.task_id.clone(),
                        source_delta_op: packet.source_delta_op.clone(),
                        artifact_kind: packet.artifact_kind.clone(),
                        content_digest: bundle.content_digest.clone(),
                        path: artifact_loop::artifact_bundle_dir(
                            &context.state_dir,
                            &pipeline_run.pipeline_run_id,
                            &bundle.bundle_id,
                        )
                        .to_string_lossy()
                        .to_string(),
                    });
                    break;
                }
                pipeline_types::AuditStatus::Rejected => {
                    artifact_loop::save_artifact_bundle(
                        &context.state_dir,
                        &pipeline_run.pipeline_run_id,
                        &bundle,
                    )
                    .map_err(|err| -> ArchitectError { err.into() })?;
                    let _ = advance_pipeline_run_with_context(
                        context,
                        &pipeline_run.pipeline_run_id,
                        PipelineStage::ArtifactLoop,
                        Some(json!({
                            "artifact_loop_trace": serde_json::to_value(&trace_events).unwrap_or(Value::Null),
                        })),
                    )
                    .await;
                    return Ok(artifact_loop::ArtifactLoopResult {
                        failed_task_id: Some(packet.task_id.clone()),
                        failure_class: verdict.failure_class.clone(),
                        error: Some("artifact auditor rejected generated bundle".to_string()),
                        tokens_used: tokens_accumulated,
                    });
                }
                pipeline_types::AuditStatus::Repairable => {
                    artifact_loop::save_artifact_bundle(
                        &context.state_dir,
                        &pipeline_run.pipeline_run_id,
                        &bundle,
                    )
                    .map_err(|err| -> ArchitectError { err.into() })?;
                    let signature = format!(
                        "{:?}",
                        artifact_loop::build_repair_packet(&verdict, &bundle, attempt)
                            .failure_class
                    );
                    let current_signature =
                        format!("{}:{}", signature, verdict.blocking_issues.join(","));
                    if repeated_failure_signature.as_deref() == Some(current_signature.as_str()) {
                        let _ = advance_pipeline_run_with_context(
                            context,
                            &pipeline_run.pipeline_run_id,
                            PipelineStage::ArtifactLoop,
                            Some(json!({
                                "artifact_loop_trace": serde_json::to_value(&trace_events).unwrap_or(Value::Null),
                            })),
                        )
                        .await;
                        return Ok(artifact_loop::ArtifactLoopResult {
                            failed_task_id: Some(packet.task_id.clone()),
                            failure_class: verdict.failure_class.clone(),
                            error: Some(
                                "artifact loop repeated the same repairable failure signature"
                                    .to_string(),
                            ),
                            tokens_used: tokens_accumulated,
                        });
                    }
                    let repair = artifact_loop::build_repair_packet(&verdict, &bundle, attempt);
                    artifact_loop::save_repair_packet(
                        &context.state_dir,
                        &pipeline_run.pipeline_run_id,
                        &repair,
                    )
                    .map_err(|err| -> ArchitectError { err.into() })?;
                    repeated_failure_signature = Some(current_signature);
                    repair_packet = Some(repair);
                    attempt += 1;
                }
            }
        }

        if approved.iter().all(|entry| entry.task_id != packet.task_id) {
            let _ = advance_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                PipelineStage::ArtifactLoop,
                Some(json!({
                    "artifact_loop_trace": serde_json::to_value(&trace_events).unwrap_or(Value::Null),
                })),
            )
            .await;
            return Ok(artifact_loop::ArtifactLoopResult {
                failed_task_id: Some(packet.task_id.clone()),
                failure_class: Some(FailureClass::ArtifactContractInvalid),
                error: Some("artifact loop exhausted attempts without approval".to_string()),
                tokens_used: tokens_accumulated,
            });
        }
    }

    artifact_loop::save_approved_registry(
        &context.state_dir,
        &pipeline_run.pipeline_run_id,
        &approved,
    )
    .map_err(|err| -> ArchitectError { err.into() })?;
    let _ = advance_pipeline_run_with_context(
        context,
        &pipeline_run.pipeline_run_id,
        PipelineStage::ArtifactLoop,
        Some(json!({
            "artifact_loop_trace": serde_json::to_value(&trace_events).unwrap_or(Value::Null),
            "approved_artifacts": serde_json::to_value(&approved).unwrap_or(Value::Null),
        })),
    )
    .await;
    Ok(artifact_loop::ArtifactLoopResult {
        failed_task_id: None,
        failure_class: None,
        error: None,
        tokens_used: tokens_accumulated,
    })
}

async fn handle_pipeline_plan_compile(
    state: &ArchitectState,
    session: &ChatSessionRecord,
    pipeline_run: PipelineRunRecord,
) -> ChatResponse {
    let run_state = pipeline_state_from_run(&pipeline_run);
    let Some(delta_report_value) = run_state.get("delta_report").cloned() else {
        let _ = block_pipeline_run(
            state,
            &pipeline_run.pipeline_run_id,
            "pipeline plan compile requires delta_report in pipeline state_json",
            &FailureClass::PlanInvalid,
        )
        .await;
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": "Pipeline blocked: missing delta_report for plan compilation.",
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    };
    let artifact_loop_trace = run_state
        .get("artifact_loop_trace")
        .cloned()
        .unwrap_or(Value::Null);
    let delta_report: DeltaReport = match serde_json::from_value(delta_report_value) {
        Ok(value) => value,
        Err(err) => {
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                &format!("pipeline delta_report parse failed: {err}"),
                &FailureClass::PlanInvalid,
            )
            .await;
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline blocked: invalid delta_report in pipeline state: {err}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "blocked",
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
    };

    let approved_artifacts = run_state.get("approved_artifacts").and_then(|value| {
        if value.is_null() {
            None
        } else {
            Some(value.clone())
        }
    });
    let solution_id = pipeline_run.solution_id.as_deref().or_else(|| {
        run_state
            .get("solution_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    });
    let task = pipeline_task_summary(&run_state, solution_id, &delta_report);
    let tool_ctx = admin_tool_context(state, Some(&session.session_id));

    let expanded_approved_artifacts = match approved_artifacts {
        Some(value) => {
            let raw = match serde_json::from_value::<Vec<artifact_loop::ApprovedArtifact>>(value) {
                Ok(items) => items,
                Err(err) => {
                    let _ = block_pipeline_run(
                        state,
                        &pipeline_run.pipeline_run_id,
                        &format!("pipeline approved_artifacts parse failed: {err}"),
                        &FailureClass::ArtifactContractInvalid,
                    )
                    .await;
                    return ChatResponse {
                        status: "error".to_string(),
                        mode: "pipeline".to_string(),
                        output: json!({
                            "message": format!("Pipeline blocked: invalid approved_artifacts in pipeline state: {err}"),
                            "pipeline_run_id": pipeline_run.pipeline_run_id,
                            "stage": "blocked",
                            "artifact_loop_trace": artifact_loop_trace,
                        }),
                        session_id: Some(session.session_id.clone()),
                        session_title: Some(session.title.clone()),
                    };
                }
            };
            match expand_approved_artifacts_for_plan_compile(
                &state.state_dir,
                &pipeline_run.pipeline_run_id,
                &raw,
            ) {
                Ok(value) => Some(value),
                Err(err) => {
                    let _ = block_pipeline_run(
                        state,
                        &pipeline_run.pipeline_run_id,
                        &format!("approved_artifacts expansion failed: {err}"),
                        &FailureClass::ArtifactContractInvalid,
                    )
                    .await;
                    return ChatResponse {
                        status: "error".to_string(),
                        mode: "pipeline".to_string(),
                        output: json!({
                            "message": format!("Pipeline blocked: approved artifact context could not be expanded for plan compilation: {err}"),
                            "pipeline_run_id": pipeline_run.pipeline_run_id,
                            "stage": "blocked",
                            "artifact_loop_trace": artifact_loop_trace,
                        }),
                        session_id: Some(session.session_id.clone()),
                        session_title: Some(session.title.clone()),
                    };
                }
            }
        }
        None => None,
    };

    let artifact_tokens_used = run_state
        .get("artifact_tokens_used")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let design_tokens_used = run_state
        .get("design_tokens_used")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let plan_initial_tokens = design_tokens_used.saturating_add(artifact_tokens_used);
    let execution = match run_plan_compiler_transaction(
        &tool_ctx,
        &task,
        &state.hive_id,
        "",
        Some(&delta_report),
        expanded_approved_artifacts.as_ref(),
        plan_initial_tokens,
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            let err_text = err.to_string();
            let ctx = FailureContext {
                stage: PipelineStage::PlanValidation,
                error_code: None,
            };
            let failure_class = match classify_failure_deterministic(&err_text, &ctx) {
                Some(c) => c,
                None => classify_failure_with_ai(state, &err_text, &ctx).await,
            };
            let route = route_failure(
                &failure_class,
                pipeline_run.current_loop as usize,
                0,
                pipeline_run.current_attempt.saturating_add(2),
            );
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                &format!("plan compiler failed: {err_text}"),
                &failure_class,
            )
            .await;
            let _ = advance_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                PipelineStage::Blocked,
                Some(json!({
                    "plan_compile_error": err_text,
                    "failure_route": route.operator_message(&failure_class),
                    "operator_options": route.operator_options(),
                })),
            )
            .await;
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline blocked during plan compilation: {err_text}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "blocked",
                    "failure_class": failure_class,
                    "failure_route": route.operator_message(&failure_class),
                    "operator_options": route.operator_options(),
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
    };

    let output = execution.output;
    let trace = execution.trace;
    let plan = output.plan.clone();
    let human_summary = output.human_summary.clone();
    let cookbook_entry = output.cookbook_entry.clone();
    let plan_compile_tokens = output.tokens_used;
    let trace_value = plan_compile_trace_to_value(&trace);
    let confirm2_payload = build_confirm2_payload(
        &pipeline_run.pipeline_run_id,
        &delta_report,
        &human_summary,
        &trace,
    );
    let total_tokens = design_tokens_used
        .saturating_add(artifact_tokens_used)
        .saturating_add(plan_compile_tokens);
    let mut confirm2_payload = confirm2_payload;
    if let Some(payload_obj) = confirm2_payload.as_object_mut() {
        payload_obj.insert("plan_compile_trace".to_string(), trace_value.clone());
        payload_obj.insert("artifact_loop_trace".to_string(), artifact_loop_trace);
        payload_obj.insert(
            "token_usage".to_string(),
            json!({
                "design": design_tokens_used,
                "artifact": artifact_tokens_used,
                "plan_compile": plan_compile_tokens,
                "total": total_tokens,
                "budget": TASK_AGENT_TOKEN_BUDGET,
            }),
        );
    }

    if let Err(err) = advance_pipeline_run(
        state,
        &pipeline_run.pipeline_run_id,
        PipelineStage::PlanValidation,
        Some(json!({
            "executor_plan": plan.clone(),
            "plan_compile_human_summary": human_summary,
            "plan_compile_trace": trace_value,
            "plan_compile_cookbook_entry": cookbook_entry,
            "plan_compile_tokens_used": plan_compile_tokens,
        })),
    )
    .await
    {
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": format!("Pipeline plan compilation succeeded but plan validation state persistence failed: {err}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "plan_validation",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    }

    if let Err(err) = advance_pipeline_run(
        state,
        &pipeline_run.pipeline_run_id,
        PipelineStage::Confirm2,
        Some(json!({
            "confirm2_payload": confirm2_payload.clone(),
        })),
    )
    .await
    {
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": format!("Pipeline plan validation succeeded but confirm2 transition failed: {err}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "plan_validation",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    }

    ChatResponse {
        status: "ok".to_string(),
        mode: "pipeline".to_string(),
        output: confirm2_payload,
        session_id: Some(session.session_id.clone()),
        session_title: Some(session.title.clone()),
    }
}

async fn compile_pipeline_plan_with_context(
    context: &ArchitectAdminToolContext,
    pipeline_run: PipelineRunRecord,
) -> Result<Value, ArchitectError> {
    let run_state = pipeline_state_from_run(&pipeline_run);
    let Some(delta_report_value) = run_state.get("delta_report").cloned() else {
        let _ = block_pipeline_run_with_context(
            context,
            &pipeline_run.pipeline_run_id,
            "pipeline plan compile requires delta_report in pipeline state_json",
            &FailureClass::PlanInvalid,
        )
        .await;
        return Ok(json!({
            "status": "error",
            "message": "Pipeline blocked: missing delta_report for plan compilation.",
            "pipeline_run_id": pipeline_run.pipeline_run_id,
            "stage": "blocked",
        }));
    };
    let artifact_loop_trace = run_state
        .get("artifact_loop_trace")
        .cloned()
        .unwrap_or(Value::Null);
    let delta_report: DeltaReport = match serde_json::from_value(delta_report_value) {
        Ok(value) => value,
        Err(err) => {
            let _ = block_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                &format!("pipeline delta_report parse failed: {err}"),
                &FailureClass::PlanInvalid,
            )
            .await;
            return Ok(json!({
                "status": "error",
                "message": format!("Pipeline blocked: invalid delta_report in pipeline state: {err}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
            }));
        }
    };

    let approved_artifacts = run_state.get("approved_artifacts").and_then(|value| {
        if value.is_null() {
            None
        } else {
            Some(value.clone())
        }
    });
    let solution_id = pipeline_run.solution_id.as_deref().or_else(|| {
        run_state
            .get("solution_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    });
    let task = pipeline_task_summary(&run_state, solution_id, &delta_report);

    let expanded_approved_artifacts = match approved_artifacts {
        Some(value) => {
            let raw = match serde_json::from_value::<Vec<artifact_loop::ApprovedArtifact>>(value) {
                Ok(items) => items,
                Err(err) => {
                    let _ = block_pipeline_run_with_context(
                        context,
                        &pipeline_run.pipeline_run_id,
                        &format!("pipeline approved_artifacts parse failed: {err}"),
                        &FailureClass::ArtifactContractInvalid,
                    )
                    .await;
                    return Ok(json!({
                        "status": "error",
                        "message": format!("Pipeline blocked: invalid approved_artifacts in pipeline state: {err}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "blocked",
                        "artifact_loop_trace": artifact_loop_trace,
                    }));
                }
            };
            match expand_approved_artifacts_for_plan_compile(
                &context.state_dir,
                &pipeline_run.pipeline_run_id,
                &raw,
            ) {
                Ok(value) => Some(value),
                Err(err) => {
                    let _ = block_pipeline_run_with_context(
                        context,
                        &pipeline_run.pipeline_run_id,
                        &format!("approved_artifacts expansion failed: {err}"),
                        &FailureClass::ArtifactContractInvalid,
                    )
                    .await;
                    return Ok(json!({
                        "status": "error",
                        "message": format!("Pipeline blocked: approved artifact context could not be expanded for plan compilation: {err}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "blocked",
                        "artifact_loop_trace": artifact_loop_trace,
                    }));
                }
            }
        }
        None => None,
    };

    let artifact_tokens_used = run_state
        .get("artifact_tokens_used")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let design_tokens_used = run_state
        .get("design_tokens_used")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let execution = match run_plan_compiler_transaction(
        context,
        &task,
        &context.hive_id,
        "",
        Some(&delta_report),
        expanded_approved_artifacts.as_ref(),
        design_tokens_used.saturating_add(artifact_tokens_used),
    )
    .await
    {
        Ok(result) => result,
        Err(err) => {
            let err_text = err.to_string();
            let failure_class = classify_failure_deterministic(
                &err_text,
                &FailureContext {
                    stage: PipelineStage::PlanValidation,
                    error_code: None,
                },
            )
            .unwrap_or(FailureClass::PlanInvalid);
            let route = route_failure(
                &failure_class,
                pipeline_run.current_loop as usize,
                0,
                pipeline_run.current_attempt.saturating_add(2),
            );
            let _ = block_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                &format!("plan compiler failed: {err_text}"),
                &failure_class,
            )
            .await;
            let _ = advance_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                PipelineStage::Blocked,
                Some(json!({
                    "plan_compile_error": err_text,
                    "failure_route": route.operator_message(&failure_class),
                    "operator_options": route.operator_options(),
                })),
            )
            .await;
            return Ok(json!({
                "status": "error",
                "message": format!("Pipeline blocked during plan compilation: {err_text}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
                "failure_class": failure_class,
                "failure_route": route.operator_message(&failure_class),
                "operator_options": route.operator_options(),
            }));
        }
    };

    let output = execution.output;
    let trace = execution.trace;
    let plan = output.plan.clone();
    let human_summary = output.human_summary.clone();
    let cookbook_entry = output.cookbook_entry.clone();
    let plan_compile_tokens = output.tokens_used;
    let trace_value = plan_compile_trace_to_value(&trace);
    let total_tokens = design_tokens_used
        .saturating_add(artifact_tokens_used)
        .saturating_add(plan_compile_tokens);
    let mut confirm2_payload = build_confirm2_payload(
        &pipeline_run.pipeline_run_id,
        &delta_report,
        &human_summary,
        &trace,
    );
    if let Some(payload_obj) = confirm2_payload.as_object_mut() {
        payload_obj.insert("status".to_string(), json!("ok"));
        payload_obj.insert("plan_compile_trace".to_string(), trace_value.clone());
        payload_obj.insert("artifact_loop_trace".to_string(), artifact_loop_trace);
        payload_obj.insert(
            "token_usage".to_string(),
            json!({
                "design": design_tokens_used,
                "artifact": artifact_tokens_used,
                "plan_compile": plan_compile_tokens,
                "total": total_tokens,
                "budget": TASK_AGENT_TOKEN_BUDGET,
            }),
        );
    }

    advance_pipeline_run_with_context(
        context,
        &pipeline_run.pipeline_run_id,
        PipelineStage::PlanValidation,
        Some(json!({
            "executor_plan": plan,
            "plan_compile_human_summary": human_summary,
            "plan_compile_trace": trace_value,
            "plan_compile_cookbook_entry": cookbook_entry,
            "plan_compile_tokens_used": plan_compile_tokens,
        })),
    )
    .await?;
    advance_pipeline_run_with_context(
        context,
        &pipeline_run.pipeline_run_id,
        PipelineStage::Confirm2,
        Some(json!({
            "confirm2_payload": confirm2_payload.clone(),
        })),
    )
    .await?;

    Ok(confirm2_payload)
}

fn pipeline_target_hives_from_manifest(
    manifest: &SolutionManifestV2,
    fallback_hive: &str,
) -> Vec<String> {
    let mut hives = manifest
        .desired_state
        .topology
        .as_ref()
        .and_then(|topology| topology.get("hives"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("hive_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if hives.is_empty() {
        hives.push(fallback_hive.to_string());
    }
    hives
}

async fn continue_pipeline_after_design_with_context(
    context: &ArchitectAdminToolContext,
    pipeline_run: PipelineRunRecord,
) -> Result<Value, ArchitectError> {
    let solution_id = pipeline_run.solution_id.clone().or_else(|| {
        pipeline_state_from_run(&pipeline_run)
            .get("solution_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });

    let Some(solution_id) = solution_id else {
        let _ = block_pipeline_run_with_context(
            context,
            &pipeline_run.pipeline_run_id,
            "pipeline auto-continue requires a solution_id to load the manifest",
            &FailureClass::DesignIncomplete,
        )
        .await;
        return Ok(json!({
            "status": "error",
            "message": "Pipeline blocked: missing solution_id for auto-continue.",
            "pipeline_run_id": pipeline_run.pipeline_run_id,
            "stage": "blocked",
        }));
    };

    let manifest = match load_manifest_from_state_dir(&context.state_dir, &solution_id, None).await
    {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            let _ = block_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                "pipeline auto-continue could not find a saved manifest for this solution",
                &FailureClass::DesignIncomplete,
            )
            .await;
            return Ok(json!({
                "status": "error",
                "message": format!("Pipeline blocked: no saved manifest found for solution '{}'.", solution_id),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
            }));
        }
        Err(err) => return Err(err),
    };

    advance_pipeline_run_with_context(
        context,
        &pipeline_run.pipeline_run_id,
        PipelineStage::Reconcile,
        None,
    )
    .await?;

    let target_hives = pipeline_target_hives_from_manifest(&manifest, &context.hive_id);
    let include_wf = manifest
        .desired_state
        .wf_deployments
        .as_ref()
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let include_opa = manifest
        .desired_state
        .opa_deployments
        .as_ref()
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let snapshot =
        match build_actual_state_snapshot(context, &target_hives, include_wf, include_opa).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let _ = block_pipeline_run_with_context(
                    context,
                    &pipeline_run.pipeline_run_id,
                    &format!("snapshot build failed: {err}"),
                    &FailureClass::SnapshotPartialBlocking,
                )
                .await;
                return Ok(json!({
                    "status": "error",
                    "message": format!("Pipeline blocked during snapshot build: {err}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "blocked",
                }));
            }
        };

    let manifest_ref = format!("manifest://{solution_id}/current");
    let reconciler_output = match reconciler::run_reconciler(&manifest, &snapshot, &manifest_ref) {
        Ok(output) => output,
        Err(err) => {
            let _ = block_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                &format!("reconciler failed: {err}"),
                &FailureClass::DeltaUnsupported,
            )
            .await;
            return Ok(json!({
                "status": "error",
                "message": format!("Pipeline blocked during reconcile: {err}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
            }));
        }
    };

    let next_stage = if reconciler_output.delta_report.status != DeltaReportStatus::Ready {
        PipelineStage::Blocked
    } else if !reconciler_output.build_task_packets.is_empty() {
        PipelineStage::ArtifactLoop
    } else {
        PipelineStage::PlanCompile
    };

    let state_update = json!({
        "solution_id": solution_id,
        "manifest_ref": manifest_ref,
        "snapshot": snapshot,
        "delta_report": reconciler_output.delta_report,
        "build_task_packets": reconciler_output.build_task_packets,
        "reconciler_diagnostics": reconciler_output.diagnostics,
    });
    let mut current_run = advance_pipeline_run_with_context(
        context,
        &pipeline_run.pipeline_run_id,
        next_stage.clone(),
        Some(state_update),
    )
    .await?;

    if matches!(next_stage, PipelineStage::Blocked) {
        return Ok(json!({
            "status": "blocked",
            "message": "Pipeline blocked: reconcile produced a blocked delta report.",
            "pipeline_run_id": pipeline_run.pipeline_run_id,
            "stage": "blocked",
            "target_hives": target_hives,
        }));
    }

    if matches!(next_stage, PipelineStage::ArtifactLoop) {
        let build_task_packets = pipeline_state_from_run(&current_run)
            .get("build_task_packets")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<BuildTaskPacket>>(value).ok())
            .unwrap_or_default();
        let artifact_initial_tokens = pipeline_state_from_run(&current_run)
            .get("design_tokens_used")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let artifact_result = run_pipeline_artifact_loop_with_context(
            context,
            &current_run,
            &build_task_packets,
            artifact_initial_tokens,
        )
        .await?;
        if let Some(error) = artifact_result.error {
            let failure_class = artifact_result
                .failure_class
                .unwrap_or(FailureClass::ArtifactContractInvalid);
            let _ = block_pipeline_run_with_context(
                context,
                &pipeline_run.pipeline_run_id,
                &format!("artifact loop failed: {error}"),
                &failure_class,
            )
            .await;
            let artifact_loop_trace =
                load_pipeline_run_with_context(context, &pipeline_run.pipeline_run_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|run| {
                        pipeline_state_from_run(&run)
                            .get("artifact_loop_trace")
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .unwrap_or(Value::Null);
            return Ok(json!({
                "status": "error",
                "message": format!("Pipeline blocked during artifact loop: {error}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
                "failed_task_id": artifact_result.failed_task_id,
                "failure_class": failure_class,
                "artifact_loop_trace": artifact_loop_trace,
            }));
        }
        current_run = advance_pipeline_run_with_context(
            context,
            &pipeline_run.pipeline_run_id,
            PipelineStage::PlanCompile,
            Some(json!({ "artifact_tokens_used": artifact_result.tokens_used })),
        )
        .await?;
    }

    compile_pipeline_plan_with_context(context, current_run).await
}

async fn handle_pipeline_confirm1(
    state: &ArchitectState,
    session: &ChatSessionRecord,
    pipeline_run: PipelineRunRecord,
) -> ChatResponse {
    let solution_id = pipeline_run.solution_id.clone().or_else(|| {
        pipeline_state_from_run(&pipeline_run)
            .get("solution_id")
            .and_then(Value::as_str)
            .map(str::to_string)
    });

    let Some(solution_id) = solution_id else {
        let _ = block_pipeline_run(
            state,
            &pipeline_run.pipeline_run_id,
            "pipeline CONFIRM 1 requires a solution_id to load the manifest",
            &FailureClass::DesignIncomplete,
        )
        .await;
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": "Pipeline blocked: missing solution_id for CONFIRM 1.",
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    };

    let manifest = match load_manifest(state, &solution_id, None).await {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                "pipeline CONFIRM 1 could not find a saved manifest for this solution",
                &FailureClass::DesignIncomplete,
            )
            .await;
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline blocked: no saved manifest found for solution '{}'.", solution_id),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "blocked",
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
        Err(err) => {
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline failed while loading manifest: {err}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "confirm1",
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
    };

    if let Err(err) = advance_pipeline_run(
        state,
        &pipeline_run.pipeline_run_id,
        PipelineStage::Reconcile,
        None,
    )
    .await
    {
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": format!("Pipeline failed to enter reconcile stage: {err}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "confirm1",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    }

    let target_hives = pipeline_target_hives_from_manifest(&manifest, &state.hive_id);
    let include_wf = manifest
        .desired_state
        .wf_deployments
        .as_ref()
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let include_opa = manifest
        .desired_state
        .opa_deployments
        .as_ref()
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    let tool_ctx = admin_tool_context(state, Some(&session.session_id));
    let snapshot = match build_actual_state_snapshot(
        &tool_ctx,
        &target_hives,
        include_wf,
        include_opa,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                &format!("snapshot build failed: {err}"),
                &FailureClass::SnapshotPartialBlocking,
            )
            .await;
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline blocked during snapshot build: {err}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "reconcile",
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
    };

    let manifest_ref = format!("manifest://{solution_id}/current");
    let reconciler_output = match reconciler::run_reconciler(&manifest, &snapshot, &manifest_ref) {
        Ok(output) => output,
        Err(err) => {
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                &format!("reconciler failed: {err}"),
                &FailureClass::DeltaUnsupported,
            )
            .await;
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline blocked during reconcile: {err}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "reconcile",
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
    };

    let next_stage = if reconciler_output.delta_report.status != DeltaReportStatus::Ready {
        PipelineStage::Blocked
    } else if !reconciler_output.build_task_packets.is_empty() {
        PipelineStage::ArtifactLoop
    } else {
        PipelineStage::PlanCompile
    };

    let state_update = json!({
        "solution_id": solution_id,
        "manifest_ref": manifest_ref,
        "snapshot": snapshot,
        "delta_report": reconciler_output.delta_report,
        "build_task_packets": reconciler_output.build_task_packets,
        "reconciler_diagnostics": reconciler_output.diagnostics,
    });
    if let Err(err) = advance_pipeline_run(
        state,
        &pipeline_run.pipeline_run_id,
        next_stage.clone(),
        Some(state_update),
    )
    .await
    {
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": format!("Pipeline reconcile succeeded but state persistence failed: {err}"),
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "reconcile",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    }

    if matches!(next_stage, PipelineStage::ArtifactLoop) {
        let refreshed_run = match load_pipeline_run(state, &pipeline_run.pipeline_run_id).await {
            Ok(Some(run)) => run,
            Ok(None) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": "Pipeline reconcile succeeded but the artifact-loop run could not be reloaded.",
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "artifact_loop",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
            Err(err) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": format!("Pipeline reconcile succeeded but reloading the artifact-loop run failed: {err}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "artifact_loop",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
        };
        let build_task_packets = pipeline_state_from_run(&refreshed_run)
            .get("build_task_packets")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<BuildTaskPacket>>(value).ok())
            .unwrap_or_default();
        let artifact_initial_tokens = pipeline_state_from_run(&refreshed_run)
            .get("design_tokens_used")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let artifact_result = match run_pipeline_artifact_loop(
            state,
            session,
            &refreshed_run,
            &build_task_packets,
            artifact_initial_tokens,
        )
        .await
        {
            Ok(result) => result,
            Err(err) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": format!("Pipeline artifact loop failed: {err}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "artifact_loop",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
        };
        if let Some(error) = artifact_result.error {
            let failure_class = artifact_result
                .failure_class
                .unwrap_or(FailureClass::ArtifactContractInvalid);
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                &format!("artifact loop failed: {error}"),
                &failure_class,
            )
            .await;
            let artifact_loop_trace = load_pipeline_run(state, &pipeline_run.pipeline_run_id)
                .await
                .ok()
                .flatten()
                .map(|run| {
                    pipeline_state_from_run(&run)
                        .get("artifact_loop_trace")
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .unwrap_or(Value::Null);
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline blocked during artifact loop: {error}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "blocked",
                    "failed_task_id": artifact_result.failed_task_id,
                    "failure_class": failure_class,
                    "artifact_loop_trace": artifact_loop_trace,
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
        if let Err(err) = advance_pipeline_run(
            state,
            &pipeline_run.pipeline_run_id,
            PipelineStage::PlanCompile,
            Some(json!({ "artifact_tokens_used": artifact_result.tokens_used })),
        )
        .await
        {
            return ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Artifact loop succeeded but plan-compile transition failed: {err}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "artifact_loop",
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            };
        }
        let plan_run = match load_pipeline_run(state, &pipeline_run.pipeline_run_id).await {
            Ok(Some(run)) => run,
            Ok(None) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": "Artifact loop succeeded but the plan-compile run could not be reloaded.",
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "plan_compile",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
            Err(err) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": format!("Artifact loop succeeded but reloading the plan-compile run failed: {err}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "plan_compile",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
        };
        return handle_pipeline_plan_compile(state, session, plan_run).await;
    }

    if matches!(next_stage, PipelineStage::PlanCompile) {
        let refreshed_run = match load_pipeline_run(state, &pipeline_run.pipeline_run_id).await {
            Ok(Some(run)) => run,
            Ok(None) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": "Pipeline reconcile succeeded but the plan-compile run could not be reloaded.",
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "plan_compile",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
            Err(err) => {
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": format!("Pipeline reconcile succeeded but reloading the plan-compile run failed: {err}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "plan_compile",
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }
        };
        return handle_pipeline_plan_compile(state, session, refreshed_run).await;
    }

    ChatResponse {
        status: "ok".to_string(),
        mode: "pipeline".to_string(),
        output: json!({
            "message": match next_stage {
                PipelineStage::ArtifactLoop => "Design confirmed. Snapshot and reconcile completed; pipeline now requires artifact generation.".to_string(),
                PipelineStage::Blocked => "Design confirmed, but reconcile produced a blocked delta report that needs operator attention.".to_string(),
                _ => "Design confirmed. Pipeline advanced.".to_string(),
            },
            "pipeline_run_id": pipeline_run.pipeline_run_id,
            "stage": format!("{next_stage}"),
            "target_hives": target_hives,
            "has_build_tasks": !matches!(next_stage, PipelineStage::Blocked),
        }),
        session_id: Some(session.session_id.clone()),
        session_title: Some(session.title.clone()),
    }
}

async fn handle_pipeline_confirm2(
    state: &ArchitectState,
    session: &ChatSessionRecord,
    pipeline_run: PipelineRunRecord,
) -> ChatResponse {
    let run_state = pipeline_state_from_run(&pipeline_run);
    let Some(plan) = run_state.get("executor_plan").cloned() else {
        let _ = block_pipeline_run(
            state,
            &pipeline_run.pipeline_run_id,
            "pipeline CONFIRM 2 requires executor_plan in pipeline state_json",
            &FailureClass::PlanInvalid,
        )
        .await;
        return ChatResponse {
            status: "error".to_string(),
            mode: "pipeline".to_string(),
            output: json!({
                "message": "Pipeline blocked: missing executor_plan for CONFIRM 2.",
                "pipeline_run_id": pipeline_run.pipeline_run_id,
                "stage": "blocked",
            }),
            session_id: Some(session.session_id.clone()),
            session_title: Some(session.title.clone()),
        };
    };

    let _ = advance_pipeline_run(
        state,
        &pipeline_run.pipeline_run_id,
        PipelineStage::Execute,
        None,
    )
    .await;

    let tool_ctx = admin_tool_context(state, Some(&session.session_id));
    let execution_id = uuid::Uuid::new_v4().to_string();
    match execute_executor_plan_with_context(
        &tool_ctx,
        execution_id,
        plan.clone(),
        json!({}),
        json!({ "origin": "pipeline_confirm2" }),
    )
    .await
    {
        Ok(output) => {
            if !executor_output_is_successful(&output) {
                let failure_text = executor_failure_text(&output);
                let failure_class = classify_failure_deterministic(
                    &failure_text,
                    &FailureContext {
                        stage: PipelineStage::Execute,
                        error_code: output
                            .get("error_code")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    },
                )
                .unwrap_or(FailureClass::ExecutionActionFailed);
                let route = route_failure(
                    &failure_class,
                    pipeline_run.current_loop as usize,
                    0,
                    pipeline_run.current_attempt as u32,
                );
                let _ = block_pipeline_run(
                    state,
                    &pipeline_run.pipeline_run_id,
                    &format!("executor plan failed: {failure_text}"),
                    &failure_class,
                )
                .await;
                let _ = advance_pipeline_run(
                    state,
                    &pipeline_run.pipeline_run_id,
                    PipelineStage::Blocked,
                    Some(json!({
                        "execution_report": output.clone(),
                        "executor_plan": plan,
                        "execution_failure_class": failure_class,
                        "execution_failure_route": route.operator_message(&failure_class),
                        "operator_options": route.operator_options(),
                    })),
                )
                .await;
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": format!("Pipeline execution failed: {failure_text}"),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "blocked",
                        "failure_class": failure_class,
                        "failure_route": route.operator_message(&failure_class),
                        "operator_options": route.operator_options(),
                        "execution_report": output,
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }

            let verification = verify_execution_result(&output);
            let _ = advance_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                PipelineStage::Verify,
                Some(json!({
                    "execution_report": output.clone(),
                    "executor_plan": plan.clone(),
                    "verification_verdict": verification,
                })),
            )
            .await;

            if !verification.verified_success {
                let _ = advance_pipeline_run(
                    state,
                    &pipeline_run.pipeline_run_id,
                    PipelineStage::Failed,
                    Some(json!({
                        "execution_report": output.clone(),
                        "executor_plan": plan,
                        "verification_verdict": verification.clone(),
                    })),
                )
                .await;
                return ChatResponse {
                    status: "error".to_string(),
                    mode: "pipeline".to_string(),
                    output: json!({
                        "message": format!("Pipeline execution completed but verification failed: {}", verification.reason),
                        "pipeline_run_id": pipeline_run.pipeline_run_id,
                        "stage": "failed",
                        "verification_verdict": verification,
                        "execution_report": output,
                    }),
                    session_id: Some(session.session_id.clone()),
                    session_title: Some(session.title.clone()),
                };
            }

            if verification.eligible_for_cookbook {
                if let Some(entry) = observed_design_cookbook_entry_from_run_state(
                    &run_state,
                    &pipeline_run.pipeline_run_id,
                ) {
                    append_design_cookbook_entry(&state.state_dir, entry);
                }
                if let Some(entry) = pipeline_cookbook_entry_from_run_state(&run_state) {
                    append_plan_compile_cookbook_entry(
                        &state.state_dir,
                        observed_plan_compile_cookbook_entry(&entry, &pipeline_run.pipeline_run_id),
                    );
                }
                for approved in pipeline_approved_artifacts_from_run_state(&run_state) {
                    if let Some(entry) = observed_artifact_cookbook_entry(
                        &state.state_dir,
                        &pipeline_run.pipeline_run_id,
                        &approved,
                    ) {
                        append_artifact_cookbook_entry(&state.state_dir, entry);
                    }
                }
            }
            let _ = advance_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                PipelineStage::Completed,
                Some(json!({
                    "execution_report": output.clone(),
                    "executor_plan": plan,
                    "verification_verdict": verification.clone(),
                })),
            )
            .await;
            let pipeline_token_usage = {
                let rs = load_pipeline_run(state, &pipeline_run.pipeline_run_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|run| pipeline_state_from_run(&run))
                    .unwrap_or_else(|| pipeline_state_from_run(&pipeline_run));
                let d = rs
                    .get("design_tokens_used")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let a = rs
                    .get("artifact_tokens_used")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let p = rs
                    .get("plan_compile_tokens_used")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                json!({ "design": d, "artifact": a, "plan_compile": p, "total": d.saturating_add(a).saturating_add(p), "budget": TASK_AGENT_TOKEN_BUDGET })
            };
            ChatResponse {
                status: "ok".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "execution_report": output,
                    "verification_verdict": verification,
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "completed",
                    "token_usage": pipeline_token_usage,
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            }
        }
        Err(err) => {
            let err_text = err.to_string();
            let failure_class = classify_failure_deterministic(
                &err_text,
                &FailureContext {
                    stage: PipelineStage::Execute,
                    error_code: None,
                },
            )
            .unwrap_or(FailureClass::ExecutionActionFailed);
            let route = route_failure(
                &failure_class,
                pipeline_run.current_loop as usize,
                0,
                pipeline_run.current_attempt as u32,
            );
            let _ = block_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                &format!("executor plan failed: {err_text}"),
                &failure_class,
            )
            .await;
            let _ = advance_pipeline_run(
                state,
                &pipeline_run.pipeline_run_id,
                PipelineStage::Blocked,
                Some(json!({
                    "executor_plan": plan,
                    "execution_failure_class": failure_class,
                    "execution_failure_route": route.operator_message(&failure_class),
                    "operator_options": route.operator_options(),
                    "execution_failure_error": err_text,
                })),
            )
            .await;
            ChatResponse {
                status: "error".to_string(),
                mode: "pipeline".to_string(),
                output: json!({
                    "message": format!("Pipeline execution failed: {err_text}"),
                    "pipeline_run_id": pipeline_run.pipeline_run_id,
                    "stage": "blocked",
                    "failure_class": failure_class,
                    "failure_route": route.operator_message(&failure_class),
                    "operator_options": route.operator_options(),
                }),
                session_id: Some(session.session_id.clone()),
                session_title: Some(session.title.clone()),
            }
        }
    }
}

// ── TB-1/TB-2: snapshot builder ───────────────────────────────────────────

async fn build_actual_state_snapshot(
    context: &ArchitectAdminToolContext,
    hives: &[String],
    include_wf: bool,
    include_opa: bool,
) -> Result<ActualStateSnapshot, ArchitectError> {
    use pipeline_types::{
        HiveResources, HiveSnapshotStatus, SnapshotAtomicity, SnapshotCompleteness, SnapshotScope,
    };

    let start_ms = now_epoch_ms();
    let snapshot_id = format!("snap-{}", uuid::Uuid::new_v4().simple());
    let mut hive_status: HashMap<String, HiveSnapshotStatus> = HashMap::new();
    let mut resources: HashMap<String, HiveResources> = HashMap::new();
    let mut missing_sections: Vec<String> = Vec::new();

    match execute_admin_action_with_context(
        context,
        &format!("SY.admin@{}", context.hive_id),
        "inventory",
        Some(&context.hive_id),
        serde_json::json!({}),
        "snapshot.inventory",
    )
    .await
    {
        Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {}
        _ => missing_sections.push("global.inventory".to_string()),
    }

    let _ = execute_admin_action_with_context(
        context,
        &format!("SY.admin@{}", context.hive_id),
        "inventory",
        Some(&context.hive_id),
        serde_json::json!({ "summary_only": true }),
        "snapshot.inventory_summary",
    )
    .await;

    for hive in hives {
        let admin_target = format!("SY.admin@{hive}");
        let mut hive_res = HiveResources::default();
        let mut section_failed = false;

        // list_runtimes (required)
        match execute_admin_action_with_context(
            context,
            &admin_target,
            "list_runtimes",
            Some(hive),
            serde_json::json!({}),
            "snapshot.runtimes",
        )
        .await
        {
            Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                hive_res.runtimes = v.get("payload").cloned();
            }
            _ => {
                missing_sections.push(format!("{hive}.runtimes"));
                section_failed = true;
            }
        }

        // list_nodes (required)
        match execute_admin_action_with_context(
            context,
            &admin_target,
            "list_nodes",
            Some(hive),
            serde_json::json!({}),
            "snapshot.nodes",
        )
        .await
        {
            Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                hive_res.nodes = v.get("payload").cloned();
            }
            _ => {
                missing_sections.push(format!("{hive}.nodes"));
                section_failed = true;
            }
        }

        // list_routes (required)
        match execute_admin_action_with_context(
            context,
            &admin_target,
            "list_routes",
            Some(hive),
            serde_json::json!({}),
            "snapshot.routes",
        )
        .await
        {
            Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                hive_res.routes = v.get("payload").cloned();
            }
            _ => {
                missing_sections.push(format!("{hive}.routes"));
                section_failed = true;
            }
        }

        // list_vpns (required for current snapshot scope)
        match execute_admin_action_with_context(
            context,
            &admin_target,
            "list_vpns",
            Some(hive),
            serde_json::json!({}),
            "snapshot.vpns",
        )
        .await
        {
            Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                hive_res.vpns = v.get("payload").cloned();
            }
            _ => {
                missing_sections.push(format!("{hive}.vpns"));
                section_failed = true;
            }
        }

        // wf_rules_list_workflows (required when wf is in scope)
        if include_wf {
            match execute_admin_action_with_context(
                context,
                &admin_target,
                "wf_rules_list_workflows",
                Some(hive),
                serde_json::json!({}),
                "snapshot.wf",
            )
            .await
            {
                Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                    hive_res.wf_state = v.get("payload").cloned();
                }
                _ => {
                    missing_sections.push(format!("{hive}.wf_state"));
                    section_failed = true;
                }
            }
        }

        // opa_get_status (required when opa is in scope)
        if include_opa {
            match execute_admin_action_with_context(
                context,
                &admin_target,
                "opa_get_status",
                Some(hive),
                serde_json::json!({}),
                "snapshot.opa",
            )
            .await
            {
                Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                    hive_res.opa_state = v.get("payload").cloned();
                }
                _ => {
                    missing_sections.push(format!("{hive}.opa_state"));
                    section_failed = true;
                }
            }
        }

        hive_status.insert(
            hive.clone(),
            HiveSnapshotStatus {
                reachable: !section_failed,
                error: if section_failed {
                    Some("one or more required sections failed to load".to_string())
                } else {
                    None
                },
            },
        );
        resources.insert(hive.clone(), hive_res);
    }

    let is_partial = !missing_sections.is_empty();
    let blocking = !missing_sections.is_empty();
    let end_ms = now_epoch_ms();

    Ok(ActualStateSnapshot {
        snapshot_version: "0.1".to_string(),
        snapshot_id,
        captured_at_start: format!("{start_ms}"),
        captured_at_end: format!("{end_ms}"),
        scope: SnapshotScope {
            hives: hives.to_vec(),
            resources: {
                let mut s = vec!["runtimes", "nodes", "routes", "vpns"];
                if include_wf {
                    s.push("wf");
                }
                if include_opa {
                    s.push("opa");
                }
                s.iter().map(|r| r.to_string()).collect()
            },
        },
        atomicity: SnapshotAtomicity {
            mode: "best_effort_multi_call".to_string(),
            is_atomic: false,
        },
        hive_status,
        resources,
        completeness: SnapshotCompleteness {
            is_partial,
            missing_sections,
            blocking,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::pipeline_types::{
        BuildTaskKnownContext, CompilerClass, DeltaOperation, DeltaSummary,
    };
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    fn test_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fluxbee-{prefix}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn resolve_runtime_name_from_binary_filename_defaults_ai_and_wf_common() {
        assert_eq!(
            resolve_runtime_name_from_binary_filename("ai").expect("ai runtime"),
            "ai.common"
        );
        assert_eq!(
            resolve_runtime_name_from_binary_filename("wf-common").expect("wf runtime"),
            "wf.common"
        );
    }

    #[test]
    fn resolve_runtime_name_from_binary_filename_maps_io_segments() {
        assert_eq!(
            resolve_runtime_name_from_binary_filename("io-api-support").expect("io runtime"),
            "io.api.support"
        );
    }

    #[test]
    fn suggest_binary_runtime_version_bumps_patch_with_archi_suffix() {
        assert_eq!(
            suggest_binary_runtime_version(Some("0.1.3"), 1776809030),
            "0.1.4-archi.1776809030"
        );
        assert_eq!(suggest_binary_runtime_version(None, 1776809030), "1.0.0");
    }

    #[test]
    fn runtime_name_from_node_config_output_extracts_nested_system_runtime() {
        let output = json!({
            "payload": {
                "config": {
                    "_system": {
                        "runtime": "io.api"
                    }
                }
            }
        });
        assert_eq!(
            runtime_name_from_node_config_output(&output),
            Some("io.api")
        );
    }

    #[test]
    fn validate_manifest_v2_rejects_unknown_desired_state_sections() {
        let manifest: SolutionManifestV2 = serde_json::from_value(json!({
            "manifest_version": "1.0",
            "solution": { "name": "demo" },
            "desired_state": {
                "nodes": [],
                "policy": { "rules": [] }
            },
            "advisory": {}
        }))
        .expect("manifest parses");
        let err = validate_manifest_v2(&manifest).expect_err("policy must be rejected");
        assert!(err.contains("UNSUPPORTED_DESIRED_STATE_SECTION"));
    }

    #[test]
    fn validate_manifest_v2_rejects_missing_required_node_fields() {
        let manifest: SolutionManifestV2 = serde_json::from_value(json!({
            "manifest_version": "1.0",
            "solution": { "name": "demo" },
            "desired_state": {
                "nodes": [{
                    "node_name": "AI.demo@motherbee",
                    "hive": "motherbee",
                    "runtime": ""
                }]
            },
            "advisory": {}
        }))
        .expect("manifest parses");
        let err = validate_manifest_v2(&manifest).expect_err("runtime is required");
        assert!(err.contains("requires non-empty node_name, hive, and runtime"));
    }

    #[test]
    fn design_audit_verdict_round_trips_and_persists() {
        let dir = test_temp_dir("design-audit");
        let verdict = DesignAuditVerdict {
            verdict_id: "verdict-1".to_string(),
            manifest_version: "2026-04-24-01".to_string(),
            status: DesignAuditStatus::Revise,
            score: 6,
            blocking_issues: vec!["NODES_MISSING".to_string()],
            findings: vec![DesignFinding {
                code: "NODES_MISSING".to_string(),
                section: "nodes".to_string(),
                message: "Node definitions are incomplete".to_string(),
                severity: AuditSeverity::Warning,
            }],
            summary: "Needs more node detail".to_string(),
            produced_at_ms: 42,
        };

        let path =
            save_design_audit_verdict(&dir, "run-1", 1, &verdict).expect("save design audit");
        assert!(path.exists());

        let loaded = load_design_audit_verdict(&dir, "run-1", 1)
            .expect("load design audit")
            .expect("design audit exists");
        assert_eq!(loaded, verdict);
    }

    #[test]
    fn build_confirm1_summary_counts_resources_and_surfaces_warnings() {
        let manifest: SolutionManifestV2 = serde_json::from_value(json!({
            "manifest_version": "1.0",
            "solution": { "name": "acme support" },
            "desired_state": {
                "topology": {
                    "hives": [
                        { "hive_id": "motherbee" },
                        { "hive_id": "worker-220" }
                    ]
                },
                "runtimes": [
                    { "name": "ai.support.demo", "version": "1.0.0" }
                ],
                "nodes": [
                    { "node_name": "AI.support@worker-220", "hive": "worker-220", "runtime": "ai.support.demo" },
                    { "node_name": "IO.slack@motherbee", "hive": "motherbee", "runtime": "io.slack" }
                ],
                "routing": [
                    { "hive": "motherbee", "prefix": "acme", "action": "route" }
                ]
            },
            "advisory": {
                "warnings": ["Needs Slack credentials"],
                "notes": ["Tenant routing depends on acme prefix"]
            }
        }))
        .expect("manifest parses");
        let verdict = DesignAuditVerdict {
            verdict_id: "verdict-2".to_string(),
            manifest_version: "2026-04-24-02".to_string(),
            status: DesignAuditStatus::Revise,
            score: 8,
            blocking_issues: vec!["SLACK_CREDENTIALS_PENDING".to_string()],
            findings: vec![DesignFinding {
                code: "SLACK_CREDENTIALS_PENDING".to_string(),
                section: "advisory".to_string(),
                message: "Slack credentials still need to be configured".to_string(),
                severity: AuditSeverity::Warning,
            }],
            summary: "Ready pending credentials".to_string(),
            produced_at_ms: 43,
        };

        let summary = build_confirm1_summary(&manifest, &verdict);
        assert_eq!(summary.solution_name, "acme support");
        assert_eq!(summary.hive_count, 2);
        assert_eq!(summary.node_count, 2);
        assert_eq!(summary.route_count, 1);
        assert_eq!(summary.audit_status, DesignAuditStatus::Revise);
        assert_eq!(summary.audit_score, 8);
        assert!(summary
            .main_runtimes
            .iter()
            .any(|value| value == "ai.support.demo"));
        assert!(summary
            .advisory_highlights
            .iter()
            .any(|value| value == "Needs Slack credentials"));
        assert!(summary
            .warnings
            .iter()
            .any(|value| value.contains("Slack credentials")));
        assert!(summary.message.contains("2 hive(s)"));
        assert!(summary.message.contains("audit score 8/10"));
    }

    #[test]
    fn design_feedback_from_verdict_includes_blockers_and_findings() {
        let verdict = DesignAuditVerdict {
            verdict_id: "verdict-3".to_string(),
            manifest_version: "2026-04-24-03".to_string(),
            status: DesignAuditStatus::Revise,
            score: 4,
            blocking_issues: vec!["TOPOLOGY_INCOMPLETE".to_string()],
            findings: vec![DesignFinding {
                code: "ROUTES_MISSING".to_string(),
                section: "routing".to_string(),
                message: "routing section is missing".to_string(),
                severity: AuditSeverity::Warning,
            }],
            summary: "Add routing before approval.".to_string(),
            produced_at_ms: 44,
        };

        let feedback = design_feedback_from_verdict(&verdict);
        assert!(feedback.contains("TOPOLOGY_INCOMPLETE"));
        assert!(feedback.contains("ROUTES_MISSING [routing] routing section is missing"));
        assert!(feedback.contains("Add routing before approval."));
    }

    #[test]
    fn canonical_plan_compile_cookbook_path_uses_shared_contract_path() {
        let dir = test_temp_dir("plan-compile-path");
        let path = plan_compile_cookbook_path(&dir);
        assert_eq!(path, dir.join(cookbook_paths::PLAN_COMPILE));
    }

    #[test]
    fn pipeline_recovery_info_from_record_marks_interrupted_runs_resumable() {
        let run = PipelineRunRecord {
            pipeline_run_id: "run-1".to_string(),
            session_id: "sess-1".to_string(),
            solution_id: Some("sol.demo".to_string()),
            status: PipelineRunStatus::Interrupted,
            current_stage: PipelineStage::PlanCompile,
            current_loop: 1,
            current_attempt: 2,
            state_json: "{}".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            interrupted_at_ms: Some(3),
        };
        let recovery = pipeline_recovery_info_from_record(&run);
        assert_eq!(recovery.pipeline_run_id, "run-1");
        assert_eq!(recovery.solution_id.as_deref(), Some("sol.demo"));
        assert_eq!(recovery.status, "interrupted");
        assert_eq!(recovery.current_stage, "plan_compile");
        assert!(recovery.can_resume);
        assert!(recovery.can_discard);
        assert_eq!(recovery.interrupted_at_ms, Some(3));
    }

    #[test]
    fn node_config_from_output_returns_only_object_payloads() {
        let output = json!({
            "payload": {
                "config": {
                    "_system": {
                        "runtime": "io.api",
                        "runtime_version": "1.0.0"
                    },
                    "mode": "support"
                }
            }
        });
        let config = node_config_from_output(&output).expect("object config");
        assert_eq!(config.get("mode").and_then(Value::as_str), Some("support"));
    }

    #[test]
    fn session_revision_string_formats_count_and_timestamp() {
        assert_eq!(
            session_revision_string(42, 1777000000000),
            "42:1777000000000"
        );
    }

    #[test]
    fn session_meta_id_from_path_extracts_id() {
        assert_eq!(
            session_meta_id_from_path("/api/session-meta/sess_123"),
            Some("sess_123")
        );
    }

    #[test]
    fn session_pipeline_recovery_id_from_path_extracts_id() {
        assert_eq!(
            session_pipeline_recovery_id_from_path("/api/sessions/sess_123/pipeline-recovery"),
            Some("sess_123")
        );
        assert!(is_session_pipeline_recovery_path(
            "/api/sessions/sess_123/pipeline-recovery"
        ));
        assert!(!is_session_detail_path(
            "/api/sessions/sess_123/pipeline-recovery"
        ));
    }

    #[test]
    fn pipeline_run_after_recovery_action_resumes_and_discards_run() {
        let run = PipelineRunRecord {
            pipeline_run_id: "run-1".to_string(),
            session_id: "sess-1".to_string(),
            solution_id: Some("sol.demo".to_string()),
            status: PipelineRunStatus::Interrupted,
            current_stage: PipelineStage::PlanCompile,
            current_loop: 0,
            current_attempt: 0,
            state_json: "{}".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            interrupted_at_ms: Some(3),
        };
        let resumed_run =
            pipeline_run_after_recovery_action(&run, "resume").expect("resume interrupted run");
        assert_eq!(resumed_run.status, PipelineRunStatus::InProgress);
        assert_eq!(resumed_run.interrupted_at_ms, None);
        assert_eq!(resumed_run.current_stage, PipelineStage::PlanCompile);

        let discarded_run =
            pipeline_run_after_recovery_action(&run, "discard").expect("discard interrupted run");
        assert_eq!(discarded_run.status, PipelineRunStatus::Failed);
        assert_eq!(discarded_run.current_stage, PipelineStage::Failed);
    }

    #[test]
    fn validate_plan_against_delta_accepts_expected_sequence() {
        let delta = DeltaReport {
            delta_report_version: "0.1".to_string(),
            status: DeltaReportStatus::Ready,
            manifest_ref: "manifest://sol.demo/current".to_string(),
            snapshot_ref: "snapshot://snap-1".to_string(),
            summary: DeltaSummary {
                creates: 0,
                updates: 1,
                deletes: 0,
                noops: 0,
                blocked: 0,
            },
            operations: vec![DeltaOperation {
                op_id: "op-1".to_string(),
                resource_type: "node".to_string(),
                resource_id: "WF.demo@motherbee".to_string(),
                change_type: ChangeType::Update,
                ownership: "solution".to_string(),
                compiler_class: CompilerClass::NodeConfigApplyRestart,
                desired_ref: Some("desired://node/1".to_string()),
                actual_ref: Some("actual://node/1".to_string()),
                blocking: false,
                payload_ref: None,
                notes: vec![],
            }],
        };
        let plan = json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {
                "name": "reconfigure_wf_demo",
                "target_hive": "motherbee"
            },
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "steps": [
                    { "id": "s1", "action": "node_control_config_set", "args": {} },
                    { "id": "s2", "action": "restart_node", "args": {} }
                ]
            }
        });
        validate_plan_against_delta(&plan, &delta).expect("sequence should validate");
    }

    #[test]
    fn validate_plan_against_delta_rejects_wrong_sequence() {
        let delta = DeltaReport {
            delta_report_version: "0.1".to_string(),
            status: DeltaReportStatus::Ready,
            manifest_ref: "manifest://sol.demo/current".to_string(),
            snapshot_ref: "snapshot://snap-1".to_string(),
            summary: DeltaSummary {
                creates: 0,
                updates: 1,
                deletes: 0,
                noops: 0,
                blocked: 0,
            },
            operations: vec![DeltaOperation {
                op_id: "op-1".to_string(),
                resource_type: "node".to_string(),
                resource_id: "WF.demo@motherbee".to_string(),
                change_type: ChangeType::Update,
                ownership: "solution".to_string(),
                compiler_class: CompilerClass::NodeConfigApplyRestart,
                desired_ref: Some("desired://node/1".to_string()),
                actual_ref: Some("actual://node/1".to_string()),
                blocking: false,
                payload_ref: None,
                notes: vec![],
            }],
        };
        let wrong_plan = json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {
                "name": "wrong_restart_order",
                "target_hive": "motherbee"
            },
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "steps": [
                    { "id": "s1", "action": "kill_node", "args": {} },
                    { "id": "s2", "action": "run_node", "args": {} }
                ]
            }
        });
        let err = validate_plan_against_delta(&wrong_plan, &delta)
            .expect_err("wrong sequence must be rejected");
        assert!(err.to_string().contains("does not match expected"));
    }

    #[test]
    fn summarize_delta_operations_by_risk_groups_destructive_and_restarting() {
        let delta = DeltaReport {
            delta_report_version: "0.1".to_string(),
            status: DeltaReportStatus::Ready,
            manifest_ref: "manifest://sol.demo/current".to_string(),
            snapshot_ref: "snapshot://snap-1".to_string(),
            summary: DeltaSummary {
                creates: 0,
                updates: 2,
                deletes: 1,
                noops: 0,
                blocked: 1,
            },
            operations: vec![
                DeltaOperation {
                    op_id: "op-1".to_string(),
                    resource_type: "node".to_string(),
                    resource_id: "WF.demo@motherbee".to_string(),
                    change_type: ChangeType::Update,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::NodeConfigApplyRestart,
                    desired_ref: None,
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec!["restart required".to_string()],
                },
                DeltaOperation {
                    op_id: "op-2".to_string(),
                    resource_type: "route".to_string(),
                    resource_id: "default".to_string(),
                    change_type: ChangeType::Delete,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::RouteDelete,
                    desired_ref: None,
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                },
                DeltaOperation {
                    op_id: "op-3".to_string(),
                    resource_type: "node".to_string(),
                    resource_id: "AI.demo@motherbee".to_string(),
                    change_type: ChangeType::Blocked,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::BlockedImmutableChange,
                    desired_ref: None,
                    actual_ref: None,
                    blocking: true,
                    payload_ref: None,
                    notes: vec!["immutable".to_string()],
                },
            ],
        };

        let (destructive, restarting, blocked) = summarize_delta_operations_by_risk(&delta);
        assert_eq!(destructive.len(), 1);
        assert_eq!(restarting.len(), 1);
        assert_eq!(blocked.len(), 1);
        assert_eq!(
            destructive[0].get("resource_id").and_then(Value::as_str),
            Some("default")
        );
        assert_eq!(
            restarting[0].get("resource_id").and_then(Value::as_str),
            Some("WF.demo@motherbee")
        );
    }

    #[test]
    fn build_confirm2_payload_highlights_risk_sections() {
        let delta = DeltaReport {
            delta_report_version: "0.1".to_string(),
            status: DeltaReportStatus::Ready,
            manifest_ref: "manifest://sol.demo/current".to_string(),
            snapshot_ref: "snapshot://snap-1".to_string(),
            summary: DeltaSummary {
                creates: 1,
                updates: 1,
                deletes: 1,
                noops: 0,
                blocked: 0,
            },
            operations: vec![
                DeltaOperation {
                    op_id: "op-1".to_string(),
                    resource_type: "route".to_string(),
                    resource_id: "default".to_string(),
                    change_type: ChangeType::Delete,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::RouteDelete,
                    desired_ref: None,
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                },
                DeltaOperation {
                    op_id: "op-2".to_string(),
                    resource_type: "node".to_string(),
                    resource_id: "WF.demo@motherbee".to_string(),
                    change_type: ChangeType::Update,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::NodeConfigApplyRestart,
                    desired_ref: None,
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                },
            ],
        };
        let trace = PlanCompileTrace {
            task: "compile".to_string(),
            hive: "motherbee".to_string(),
            steps: vec![],
            step_count: 2,
            validation: "ok".to_string(),
            first_validation_error: None,
            tokens_used: 13_225,
            token_budget: TASK_AGENT_TOKEN_BUDGET,
        };

        let payload = build_confirm2_payload("run-1", &delta, "2 steps", &trace);
        assert_eq!(
            payload.get("stage").and_then(Value::as_str),
            Some("confirm2")
        );
        assert_eq!(
            payload
                .get("destructive_actions")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            payload
                .get("restarting_actions")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn executor_output_is_successful_rejects_failed_summary_status() {
        let ok = json!({
            "status": "ok",
            "payload": {
                "summary": {
                    "status": "done"
                }
            }
        });
        let failed = json!({
            "status": "ok",
            "payload": {
                "summary": {
                    "status": "failed"
                }
            }
        });
        assert!(executor_output_is_successful(&ok));
        assert!(!executor_output_is_successful(&failed));
    }

    #[test]
    fn observed_plan_compile_cookbook_entry_marks_run_and_seed_kind() {
        let entry = CookbookEntryV2 {
            layer: CookbookLayer::PlanCompile,
            pattern_key: "route_delete_then_add".to_string(),
            trigger: "When replacing a route".to_string(),
            inputs_signature: json!({"compiler_classes":["ROUTE_REPLACE"]}),
            successful_shape: json!({"steps_pattern":["delete_route","add_route"]}),
            constraints: vec![],
            do_not_repeat: vec![],
            failure_class: None,
            recorded_from_run: None,
            seed_kind: "manual".to_string(),
        };

        let observed = observed_plan_compile_cookbook_entry(&entry, "run-123");
        assert_eq!(observed.recorded_from_run.as_deref(), Some("run-123"));
        assert_eq!(observed.seed_kind, "observed");
        assert_eq!(observed.layer, CookbookLayer::PlanCompile);
    }

    #[test]
    fn observed_design_cookbook_entry_from_run_state_uses_confirm1_summary() {
        let run_state = json!({
            "solution_id": "support-acme",
            "design_iterations_used": 2,
            "design_stop_reason": "pass",
            "confirm1_summary": {
                "solution_name": "Support Acme",
                "hive_count": 2,
                "node_count": 3,
                "route_count": 1,
                "main_runtimes": ["ai.common", "io.slack"],
                "audit_status": "pass",
                "audit_score": 9,
                "blocking_issues": [],
                "advisory_highlights": ["slack token required"],
                "warnings": ["verify tenant mapping"],
                "message": "Support topology ready."
            },
            "design_audit_verdict": {
                "verdict_id": "design-audit-1",
                "manifest_version": "2",
                "status": "pass",
                "score": 9,
                "blocking_issues": [],
                "findings": [],
                "summary": "ok",
                "produced_at_ms": 1
            }
        });

        let entry = observed_design_cookbook_entry_from_run_state(&run_state, "run-42")
            .expect("design cookbook entry");
        assert_eq!(entry.layer, CookbookLayer::Design);
        assert_eq!(entry.recorded_from_run.as_deref(), Some("run-42"));
        assert_eq!(entry.seed_kind, "observed");
        assert_eq!(
            entry
                .inputs_signature
                .get("solution_id")
                .and_then(Value::as_str),
            Some("support-acme")
        );
        assert_eq!(
            entry
                .successful_shape
                .get("audit_score")
                .and_then(Value::as_u64),
            Some(9)
        );
        assert_eq!(entry.constraints, vec!["verify tenant mapping".to_string()]);
    }

    #[test]
    fn observed_artifact_cookbook_entry_uses_saved_bundle_and_task_packet() {
        let dir = test_temp_dir("artifact-cookbook-entry");
        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "task-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-1".to_string(),
            runtime_name: Some("ai.support.demo".to_string()),
            target_kind: "inline_package".to_string(),
            requirements: json!({}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext::default(),
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };
        artifact_loop::save_task_packet(&dir, "run-1", &packet).expect("save task packet");

        let bundle = ArtifactBundle {
            bundle_version: "0.1".to_string(),
            bundle_id: "bundle-1".to_string(),
            source_task_id: "task-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            status: pipeline_types::ArtifactBundleStatus::Approved,
            summary: "Prepared runtime package".to_string(),
            artifact: json!({
                "files": {
                    "package.json": "{}",
                    "config/default-config.json": "{}"
                }
            }),
            assumptions: vec![],
            verification_hints: vec![],
            content_digest: "sha256:demo".to_string(),
            generated_at_ms: 1,
            generator_model: "gpt-test".to_string(),
        };
        artifact_loop::save_artifact_bundle(&dir, "run-1", &bundle).expect("save artifact bundle");

        let approved = artifact_loop::ApprovedArtifact {
            bundle_id: "bundle-1".to_string(),
            task_id: "task-1".to_string(),
            source_delta_op: "op-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            content_digest: "sha256:demo".to_string(),
            path: dir.display().to_string(),
        };

        let entry = observed_artifact_cookbook_entry(&dir, "run-1", &approved).expect("entry");
        assert_eq!(entry.layer, CookbookLayer::Artifact);
        assert_eq!(entry.recorded_from_run.as_deref(), Some("run-1"));
        assert_eq!(entry.seed_kind, "observed");
        assert_eq!(
            entry
                .inputs_signature
                .get("runtime_name")
                .and_then(Value::as_str),
            Some("ai.support.demo")
        );
    }

    #[test]
    fn verify_execution_result_requires_done_and_no_failed_step() {
        let report = json!({
            "status": "ok",
            "payload": {
                "summary": {
                    "status": "done",
                    "failed_step_id": null,
                    "completed_steps": 2,
                    "total_steps": 2
                }
            }
        });
        let verdict = verify_execution_result(&report);
        assert!(verdict.verified_success);
        assert!(verdict.eligible_for_cookbook);
    }

    #[test]
    fn verify_execution_result_rejects_incomplete_or_unverified_reports() {
        let partial = json!({
            "status": "ok",
            "payload": {
                "summary": {
                    "status": "done",
                    "failed_step_id": null,
                    "completed_steps": 1,
                    "total_steps": 2
                }
            }
        });
        let failed = json!({
            "status": "ok",
            "payload": {
                "summary": {
                    "status": "failed",
                    "failed_step_id": "step-2",
                    "completed_steps": 1,
                    "total_steps": 2
                }
            }
        });

        let partial_verdict = verify_execution_result(&partial);
        assert!(partial_verdict.verified_success);
        assert!(!partial_verdict.eligible_for_cookbook);

        let failed_verdict = verify_execution_result(&failed);
        assert!(!failed_verdict.verified_success);
        assert!(!failed_verdict.eligible_for_cookbook);
    }

    #[test]
    fn pipeline_run_status_for_stage_marks_terminal_states() {
        assert_eq!(
            pipeline_run_status_for_stage(&PipelineStage::Completed),
            PipelineRunStatus::Completed
        );
        assert_eq!(
            pipeline_run_status_for_stage(&PipelineStage::Blocked),
            PipelineRunStatus::Blocked
        );
        assert_eq!(
            pipeline_run_status_for_stage(&PipelineStage::Failed),
            PipelineRunStatus::Failed
        );
        assert_eq!(
            pipeline_run_status_for_stage(&PipelineStage::Confirm2),
            PipelineRunStatus::InProgress
        );
    }

    #[test]
    fn artifact_bundle_from_real_programmer_submission_extracts_inline_files() {
        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "art-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-1".to_string(),
            runtime_name: Some("ai.common.demo".to_string()),
            target_kind: "inline_package".to_string(),
            requirements: json!({}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext::default(),
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };
        let submitted = json!({
            "summary": "Prepared package",
            "artifact": {
                "files": {
                    "package.json": "{\"name\":\"ai.common.demo\"}",
                    "config/default-config.json": "{}"
                }
            },
            "assumptions": ["used default version"],
            "verification_hints": ["verify runtime_base"]
        });

        let bundle =
            artifact_bundle_from_real_programmer_submission(&packet, &submitted).expect("bundle");
        assert_eq!(bundle.artifact_kind, "runtime_package");
        assert_eq!(bundle.generator_model, "real_programmer");
        assert_eq!(
            bundle
                .artifact
                .get("files")
                .and_then(Value::as_object)
                .and_then(|value| value.get("package.json"))
                .and_then(Value::as_str),
            Some("{\"name\":\"ai.common.demo\"}")
        );
        assert!(bundle
            .assumptions
            .iter()
            .any(|value| value == "used default version"));
        assert!(bundle.content_digest.starts_with("sha256:"));
    }

    #[test]
    fn artifact_bundle_from_real_programmer_submission_accepts_bundle_upload_target() {
        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "art-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-1".to_string(),
            runtime_name: Some("ai.common.demo".to_string()),
            target_kind: "bundle_upload".to_string(),
            requirements: json!({}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext::default(),
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };
        let submitted = json!({
            "summary": "Prepared package",
            "artifact": {
                "files": {
                    "package.json": "{\"name\":\"ai.common.demo\"}",
                    "config/default-config.json": "{}"
                }
            }
        });

        let bundle = artifact_bundle_from_real_programmer_submission(&packet, &submitted)
            .expect("bundle_upload target should be accepted as a logical file map");
        assert_eq!(bundle.artifact_kind, "runtime_package");
        assert_eq!(bundle.artifact["publish_source"], Value::Null);
        assert_eq!(
            bundle
                .artifact
                .get("files")
                .and_then(Value::as_object)
                .and_then(|value| value.get("package.json"))
                .and_then(Value::as_str),
            Some("{\"name\":\"ai.common.demo\"}")
        );
    }

    #[test]
    fn finalize_runtime_package_bundle_for_target_inline_adds_publish_source() {
        let blob_root = test_temp_dir("artifact-inline-publish-source");
        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "art-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-1".to_string(),
            runtime_name: Some("ai.common.demo".to_string()),
            target_kind: "inline_package".to_string(),
            requirements: json!({"version": "1.2.3"}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext::default(),
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };
        let mut bundle = ArtifactBundle {
            bundle_version: "0.1".to_string(),
            bundle_id: "bundle-1".to_string(),
            source_task_id: "art-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            status: pipeline_types::ArtifactBundleStatus::Approved,
            summary: "Prepared package".to_string(),
            artifact: json!({
                "files": {
                    "package.json": "{\"name\":\"ai.common.demo\",\"version\":\"1.2.3\",\"type\":\"config_only\",\"runtime_base\":\"ai.common\"}",
                    "config/default-config.json": "{}"
                }
            }),
            assumptions: vec![],
            verification_hints: vec![],
            content_digest: "sha256:old".to_string(),
            generated_at_ms: 1,
            generator_model: "real_programmer".to_string(),
        };

        finalize_runtime_package_bundle_for_target(&blob_root, &packet, &mut bundle)
            .expect("finalize inline publish source");
        assert_eq!(
            bundle.artifact["publish_source"]["kind"],
            json!("inline_package")
        );
        assert!(bundle.artifact["publish_source"]["files"]
            .get("package.json")
            .and_then(Value::as_str)
            .is_some());
        assert!(bundle.content_digest.starts_with("sha256:"));

        let _ = fs::remove_dir_all(blob_root);
    }

    #[test]
    fn finalize_runtime_package_bundle_for_target_bundle_upload_adds_blob_path() {
        let blob_root = test_temp_dir("artifact-bundle-upload-publish-source");
        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "art-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-1".to_string(),
            runtime_name: Some("ai.common.demo".to_string()),
            target_kind: "bundle_upload".to_string(),
            requirements: json!({"version": "1.2.3"}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext::default(),
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };
        let mut bundle = ArtifactBundle {
            bundle_version: "0.1".to_string(),
            bundle_id: "bundle-1".to_string(),
            source_task_id: "art-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            status: pipeline_types::ArtifactBundleStatus::Approved,
            summary: "Prepared package".to_string(),
            artifact: json!({
                "files": {
                    "package.json": "{\"name\":\"ai.common.demo\",\"version\":\"1.2.3\",\"type\":\"config_only\",\"runtime_base\":\"ai.common\"}",
                    "config/default-config.json": "{}"
                }
            }),
            assumptions: vec![],
            verification_hints: vec![],
            content_digest: "sha256:old".to_string(),
            generated_at_ms: 1,
            generator_model: "real_programmer".to_string(),
        };

        finalize_runtime_package_bundle_for_target(&blob_root, &packet, &mut bundle)
            .expect("finalize bundle_upload publish source");
        let blob_path = bundle.artifact["publish_source"]["blob_path"]
            .as_str()
            .expect("blob_path");
        assert_eq!(
            bundle.artifact["publish_source"]["kind"],
            json!("bundle_upload")
        );
        assert!(blob_path.starts_with("active/"));
        assert!(
            blob_root.join(blob_path).exists(),
            "bundle blob should exist"
        );
        assert!(bundle.content_digest.starts_with("sha256:"));

        let _ = fs::remove_dir_all(blob_root);
    }

    #[test]
    fn expand_approved_artifacts_for_plan_compile_includes_publish_source() {
        let dir = test_temp_dir("expand-approved-artifacts");
        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "task-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-1".to_string(),
            runtime_name: Some("ai.support.demo".to_string()),
            target_kind: "inline_package".to_string(),
            requirements: json!({}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext::default(),
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };
        artifact_loop::save_task_packet(&dir, "run-1", &packet).expect("save task packet");

        let bundle = ArtifactBundle {
            bundle_version: "0.1".to_string(),
            bundle_id: "bundle-1".to_string(),
            source_task_id: "task-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            status: pipeline_types::ArtifactBundleStatus::Approved,
            summary: "Prepared runtime package".to_string(),
            artifact: json!({
                "files": {
                    "package.json": "{\"name\":\"ai.support.demo\"}",
                    "config/default-config.json": "{}"
                },
                "publish_source": {
                    "kind": "inline_package",
                    "files": {
                        "package.json": "{\"name\":\"ai.support.demo\"}",
                        "config/default-config.json": "{}"
                    }
                }
            }),
            assumptions: vec![],
            verification_hints: vec!["publish runtime".to_string()],
            content_digest: "sha256:demo".to_string(),
            generated_at_ms: 1,
            generator_model: "gpt-test".to_string(),
        };
        artifact_loop::save_artifact_bundle(&dir, "run-1", &bundle).expect("save artifact bundle");

        let approved = artifact_loop::ApprovedArtifact {
            bundle_id: "bundle-1".to_string(),
            task_id: "task-1".to_string(),
            source_delta_op: "op-1".to_string(),
            artifact_kind: "runtime_package".to_string(),
            content_digest: "sha256:demo".to_string(),
            path: dir.display().to_string(),
        };

        let expanded = expand_approved_artifacts_for_plan_compile(&dir, "run-1", &[approved])
            .expect("expand approved artifacts");
        let first = expanded
            .as_array()
            .and_then(|items| items.first())
            .expect("first expanded artifact");
        assert_eq!(first["publish_source"]["kind"], json!("inline_package"));
        assert_eq!(first["target_kind"], json!("inline_package"));
        assert_eq!(first["runtime_name"], json!("ai.support.demo"));
    }

    #[test]
    fn runtime_case_variants_detects_legacy_conflicts() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "IO.api".to_string(),
            RuntimeManifestEntry {
                available: vec!["0.1.5".to_string()],
                current: Some("0.1.5".to_string()),
                package_type: Some("full_runtime".to_string()),
                runtime_base: None,
                extra: BTreeMap::new(),
            },
        );
        entries.insert(
            "io.api".to_string(),
            RuntimeManifestEntry {
                available: vec!["1.0.0".to_string()],
                current: Some("1.0.0".to_string()),
                package_type: Some("full_runtime".to_string()),
                runtime_base: None,
                extra: BTreeMap::new(),
            },
        );
        let mut variants = runtime_case_variants(&entries, "io.api");
        variants.sort();
        assert_eq!(variants, vec!["IO.api".to_string(), "io.api".to_string()]);
    }

    #[test]
    fn parse_json_value_from_text_accepts_json_code_fence() {
        let parsed = parse_json_value_from_text(
            "```json\n{\"summary\":\"ok\",\"publish_request\":{\"source\":{\"kind\":\"bundle_upload\",\"blob_path\":\"packages/incoming/demo.zip\"}},\"notes\":[]}\n```",
        )
        .expect("parse fenced json");

        assert_eq!(
            parsed,
            json!({
                "summary": "ok",
                "publish_request": {
                    "source": {
                        "kind": "bundle_upload",
                        "blob_path": "packages/incoming/demo.zip"
                    }
                },
                "notes": []
            })
        );
    }

    #[test]
    fn resolve_json_object_param_accepts_json_string() {
        let value = json!({
            "source_json": "{\"kind\":\"bundle_upload\",\"blob_path\":\"packages/incoming/demo.zip\"}"
        });

        let resolved =
            resolve_json_object_param(&value, "source", "source_json").expect("resolve source");
        assert_eq!(
            resolved,
            json!({
                "kind": "bundle_upload",
                "blob_path": "packages/incoming/demo.zip"
            })
        );
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
    fn translate_publish_runtime_package_scmd() {
        let translated = translate_scmd(
            "motherbee",
            parse(
                r#"curl -X POST /admin/runtime-packages/publish -d '{"source":{"kind":"bundle_upload","blob_path":"packages/incoming/ai-support-demo-0.1.0.zip"},"sync_to":["worker-220"],"update_to":["worker-220"]}'"#,
            ),
        )
        .expect("translation should succeed");

        assert_eq!(translated.action, "publish_runtime_package");
        assert_eq!(translated.target_hive, "motherbee");
        assert_eq!(
            translated.params,
            json!({
                "source": {
                    "kind": "bundle_upload",
                    "blob_path": "packages/incoming/ai-support-demo-0.1.0.zip"
                },
                "sync_to": ["worker-220"],
                "update_to": ["worker-220"]
            })
        );
        assert!(admin_action_allows_ai_write("publish_runtime_package"));
    }

    #[test]
    fn translate_restart_node_scmd() {
        let translated = translate_scmd(
            "motherbee",
            parse(r#"curl -X POST /hives/worker-220/nodes/AI.chat@worker-220/restart -d '{}'"#),
        )
        .expect("translation should succeed");

        assert_eq!(translated.action, "restart_node");
        assert_eq!(translated.target_hive, "worker-220");
        assert_eq!(
            translated.params,
            json!({
                "node_name": "AI.chat@worker-220"
            })
        );
        assert!(admin_action_allows_ai_write("restart_node"));
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

    // ── TG-4: Pipeline state machine progresses Design → Completed ────────────
    // Verifies that pipeline_run_with_state_update correctly transitions every
    // stage and that only Completed/Failed/Blocked/Interrupted set terminal status.
    #[test]
    fn tg4_pipeline_state_machine_progresses_design_to_completed() {
        let base = PipelineRunRecord {
            pipeline_run_id: "run-tg4-001".to_string(),
            session_id: "sess-tg4".to_string(),
            solution_id: Some("sol-tg4".to_string()),
            status: PipelineRunStatus::InProgress,
            current_stage: PipelineStage::Design,
            current_loop: 0,
            current_attempt: 0,
            state_json: "{}".to_string(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
            interrupted_at_ms: None,
        };

        let stages_in_order = [
            PipelineStage::DesignAudit,
            PipelineStage::Confirm1,
            PipelineStage::Reconcile,
            PipelineStage::ArtifactLoop,
            PipelineStage::PlanCompile,
            PipelineStage::PlanValidation,
            PipelineStage::Confirm2,
            PipelineStage::Execute,
            PipelineStage::Verify,
        ];

        let mut run = base;
        for stage in &stages_in_order {
            let next = pipeline_run_with_state_update(&run, stage.clone(), None, None)
                .expect("state transition must not fail");
            assert_eq!(
                next.current_stage, *stage,
                "stage must advance to {stage:?}"
            );
            assert_eq!(
                next.status,
                PipelineRunStatus::InProgress,
                "status must remain InProgress through {stage:?}"
            );
            assert!(
                next.interrupted_at_ms.is_none(),
                "interrupted_at_ms must be None mid-run"
            );
            run = next;
        }

        // Advance to Completed — status must flip to Completed
        let completed = pipeline_run_with_state_update(&run, PipelineStage::Completed, None, None)
            .expect("Completed transition must not fail");
        assert_eq!(completed.current_stage, PipelineStage::Completed);
        assert_eq!(completed.status, PipelineRunStatus::Completed);
        assert!(completed.interrupted_at_ms.is_none());

        // State payloads survive each merge
        let with_state = pipeline_run_with_state_update(
            &completed,
            PipelineStage::Completed,
            Some(json!({ "executor_plan_ref": "plan-001" })),
            None,
        )
        .unwrap();
        let state = pipeline_state_from_run(&with_state);
        assert_eq!(state["executor_plan_ref"], json!("plan-001"));
    }

    // ── TG-5: Artifact repair loop — bad bundle → repair → approved ──────────
    // Tests the two-attempt artifact auditor path:
    // attempt 1: missing package.json → Repairable + repair hints
    // attempt 2: complete bundle → Approved
    #[test]
    fn tg5_artifact_repair_loop_recovers_on_second_attempt() {
        use super::artifact_loop::{audit_artifact, build_repair_packet};
        use super::pipeline_types::{
            ArtifactBundle, ArtifactBundleStatus, AuditStatus, BuildTaskKnownContext,
            BuildTaskPacket,
        };

        let packet = BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "tg5-task-001".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-tg5".to_string(),
            runtime_name: Some("ai.support.tg5".to_string()),
            target_kind: "inline_package".to_string(),
            requirements: json!({}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext {
                available_runtimes: vec!["AI.common".to_string()],
                running_nodes: vec![],
            },
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        };

        // Attempt 1: bundle missing package.json — auditor must return Repairable
        let bad_bundle = ArtifactBundle {
            bundle_version: "0.1".to_string(),
            bundle_id: "bundle-tg5-bad".to_string(),
            source_task_id: "tg5-task-001".to_string(),
            artifact_kind: "runtime_package".to_string(),
            status: ArtifactBundleStatus::Pending,
            summary: "attempt 1 — missing package.json".to_string(),
            artifact: json!({ "files": { "config/default-config.json": "{}" } }),
            assumptions: vec![],
            verification_hints: vec![],
            content_digest: "sha256:bad".to_string(),
            generated_at_ms: 1_000,
            generator_model: "test".to_string(),
        };

        let verdict1 = audit_artifact(&packet, &bad_bundle);
        assert_eq!(
            verdict1.status,
            AuditStatus::Repairable,
            "attempt 1 must be Repairable"
        );
        assert!(
            verdict1.blocking_issues.iter().any(|c| c == "MISSING_FILE"),
            "must flag MISSING_FILE"
        );
        assert!(
            !verdict1.repair_hints.is_empty(),
            "repair hints must be non-empty for attempt 1"
        );
        assert!(
            verdict1
                .repair_hints
                .iter()
                .any(|h| h.instruction.contains("package.json")),
            "repair hint must mention package.json"
        );

        // Build repair packet — simulates what the loop controller does between attempts
        let repair = build_repair_packet(&verdict1, &bad_bundle, 1);
        assert_eq!(repair.attempt, 1);
        assert!(
            repair.retry_allowed,
            "retry must be allowed after Repairable verdict"
        );
        assert!(
            repair
                .required_corrections
                .iter()
                .any(|c| c.contains("package.json")),
            "repair packet must carry correction about package.json"
        );

        // Attempt 2: programmer follows the repair packet — complete bundle
        let mut fixed_packet = packet.clone();
        fixed_packet.attempt = 1;
        let good_bundle = ArtifactBundle {
            bundle_id: "bundle-tg5-good".to_string(),
            summary: "attempt 2 — all required files present".to_string(),
            artifact: json!({
                "files": {
                    "package.json": r#"{"name":"ai.support.tg5","version":"0.1.0","type":"config_only","runtime_base":"AI.common"}"#,
                    "config/default-config.json": r#"{"tenant_id":"tnt:tg5"}"#
                }
            }),
            content_digest: "sha256:good".to_string(),
            ..bad_bundle.clone()
        };

        let verdict2 = audit_artifact(&fixed_packet, &good_bundle);
        assert_eq!(
            verdict2.status,
            AuditStatus::Approved,
            "attempt 2 must be Approved"
        );
        assert!(
            verdict2.findings.is_empty(),
            "no findings on approved bundle"
        );
        assert!(
            verdict2.repair_hints.is_empty(),
            "no repair hints on approved bundle"
        );
    }

    // ── TG-6: Interrupted run — state and timestamp preserved ────────────────
    // Verifies that transitioning to PipelineStage::Interrupted:
    // - sets status to Interrupted
    // - sets interrupted_at_ms (first transition)
    // - preserves interrupted_at_ms on subsequent updates (idempotent)
    #[test]
    fn tg6_interrupted_run_sets_and_preserves_interrupted_at_ms() {
        let run = PipelineRunRecord {
            pipeline_run_id: "run-tg6-001".to_string(),
            session_id: "sess-tg6".to_string(),
            solution_id: None,
            status: PipelineRunStatus::InProgress,
            current_stage: PipelineStage::ArtifactLoop,
            current_loop: 0,
            current_attempt: 1,
            state_json: r#"{"build_task_packets":[{"task_id":"art-tg6"}]}"#.to_string(),
            created_at_ms: 1_000,
            updated_at_ms: 2_000,
            interrupted_at_ms: None,
        };

        // Simulate process kill mid-artifact-loop → mark interrupted
        let interrupted =
            pipeline_run_with_state_update(&run, PipelineStage::Interrupted, None, None)
                .expect("Interrupted transition must succeed");

        assert_eq!(interrupted.status, PipelineRunStatus::Interrupted);
        assert_eq!(interrupted.current_stage, PipelineStage::Interrupted);
        let ts = interrupted
            .interrupted_at_ms
            .expect("interrupted_at_ms must be set");
        assert!(ts >= 1_000, "interrupted_at_ms must be a real timestamp");

        // State payload must survive the transition (needed for recovery UI)
        let state = pipeline_state_from_run(&interrupted);
        assert!(
            state["build_task_packets"].is_array(),
            "build_task_packets must be preserved after interruption"
        );

        // Second mark-interrupted call must not overwrite the original timestamp
        let interrupted2 =
            pipeline_run_with_state_update(&interrupted, PipelineStage::Interrupted, None, None)
                .unwrap();
        assert_eq!(
            interrupted2.interrupted_at_ms,
            Some(ts),
            "interrupted_at_ms must not be overwritten on repeated Interrupted transitions"
        );
    }

    // ── TG-7: Partial snapshot blocking — no destructive ops emitted ─────────
    // Verifies the full reconciler output when the snapshot is partial-blocking:
    // - delta report status is BlockedPartialSnapshot
    // - zero operations with destructive compiler_class are present
    // - at least one BLOCKED_PARTIAL_SNAPSHOT op is present for the unreachable hive
    #[test]
    fn tg7_partial_snapshot_blocks_all_destructive_ops() {
        use super::pipeline_types::{
            compiler_class_risk, ActualStateSnapshot, ChangeType, DesiredStateV2, HiveResources,
            HiveSnapshotStatus, RiskClass, SnapshotAtomicity, SnapshotCompleteness, SnapshotScope,
            SolutionManifestV2,
        };
        use super::reconciler::run_reconciler;
        use std::collections::HashMap;

        // Manifest: wants to delete an existing node (ownership=solution) on worker-220
        let manifest = SolutionManifestV2 {
            manifest_version: "2.0".to_string(),
            solution: json!({ "name": "tg7-test" }),
            desired_state: DesiredStateV2 {
                nodes: Some(vec![]), // desired: no nodes → reconciler would normally emit NODE_KILL
                ..Default::default()
            },
            advisory: json!({}),
        };

        // Snapshot: worker-220 is unreachable → blocking = true
        let mut hive_status = HashMap::new();
        hive_status.insert(
            "worker-220".to_string(),
            HiveSnapshotStatus {
                reachable: false,
                error: Some("connection refused".to_string()),
            },
        );
        let mut resources = HashMap::new();
        resources.insert("worker-220".to_string(), HiveResources::default());

        let snapshot = ActualStateSnapshot {
            snapshot_version: "1.0".to_string(),
            snapshot_id: "snap-tg7".to_string(),
            captured_at_start: "2026-04-24T00:00:00Z".to_string(),
            captured_at_end: "2026-04-24T00:00:01Z".to_string(),
            scope: SnapshotScope {
                hives: vec!["worker-220".to_string()],
                resources: vec![],
            },
            atomicity: SnapshotAtomicity {
                mode: "best_effort_multi_call".to_string(),
                is_atomic: false,
            },
            hive_status,
            resources,
            completeness: SnapshotCompleteness {
                is_partial: true,
                missing_sections: vec!["worker-220".to_string()],
                blocking: true,
            },
        };

        let output = run_reconciler(&manifest, &snapshot, "manifest-tg7")
            .expect("reconciler must not error on partial snapshot");

        assert_eq!(
            output.delta_report.status,
            super::pipeline_types::DeltaReportStatus::BlockedPartialSnapshot,
            "delta report must be BlockedPartialSnapshot when snapshot is blocking"
        );

        // No destructive or restarting ops must be emitted
        let destructive_ops: Vec<_> = output
            .delta_report
            .operations
            .iter()
            .filter(|op| {
                matches!(
                    compiler_class_risk(&op.compiler_class),
                    RiskClass::Destructive | RiskClass::Restarting
                ) && op.change_type != ChangeType::Blocked
            })
            .collect();
        assert!(
            destructive_ops.is_empty(),
            "must emit zero destructive/restarting ops when snapshot is partial-blocking; got: {:?}",
            destructive_ops.iter().map(|o| &o.compiler_class).collect::<Vec<_>>()
        );
    }

    #[test]
    fn retry_target_stage_pre_block_inner_stages_route_to_confirm1() {
        use super::pipeline_types::PipelineStage;

        for stage in [
            PipelineStage::Reconcile,
            PipelineStage::ArtifactLoop,
            PipelineStage::PlanCompile,
            PipelineStage::PlanValidation,
            PipelineStage::Design,
            PipelineStage::DesignAudit,
            PipelineStage::Confirm1,
        ] {
            assert_eq!(
                super::retry_target_stage_for(Some(&stage)).unwrap(),
                PipelineStage::Confirm1,
                "stage {stage:?} should retry at Confirm1",
            );
        }
    }

    #[test]
    fn retry_target_stage_pre_block_execute_or_verify_routes_to_confirm2() {
        use super::pipeline_types::PipelineStage;

        for stage in [
            PipelineStage::Execute,
            PipelineStage::Verify,
            PipelineStage::Confirm2,
        ] {
            assert_eq!(
                super::retry_target_stage_for(Some(&stage)).unwrap(),
                PipelineStage::Confirm2,
                "stage {stage:?} should retry at Confirm2",
            );
        }
    }

    #[test]
    fn retry_target_stage_terminal_or_missing_stage_errors() {
        use super::pipeline_types::PipelineStage;

        assert!(super::retry_target_stage_for(None).is_err());
        assert!(super::retry_target_stage_for(Some(&PipelineStage::Failed)).is_err());
        assert!(super::retry_target_stage_for(Some(&PipelineStage::Completed)).is_err());
        assert!(super::retry_target_stage_for(Some(&PipelineStage::Blocked)).is_err());
    }
}
