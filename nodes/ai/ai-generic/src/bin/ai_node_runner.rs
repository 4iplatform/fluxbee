use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use fluxbee_ai_sdk::{
    build_ai_behavior_response, build_openai_user_content_parts,
    build_output_schema_fallback_instruction, build_reply_message_runtime_src, extract_text,
    resolve_model_input_from_payload_with_options, resolve_response_envelope_output_schema,
    AiBehaviorOutput, AiFinalOutput, AiNode, AiUserArtifact, FunctionCallingConfig,
    FunctionCallingRunner, FunctionLoopItem, FunctionLoopRunResult, FunctionRunInput, FunctionTool,
    FunctionToolDefinition, FunctionToolProvider, FunctionToolRegistry,
    ImmediateConversationMemory, LanceDbThreadStateStore, Message, ModelInputOptions,
    ModelSettings, NodeRuntime, OpenAiResponsesClient, ResolvedModelInput, RetryPolicy,
    RuntimeConfig, ThreadStateStore, ThreadStateToolsProvider,
};
use fluxbee_sdk::identity::{find_ilk_by_handler_node_from_hive_config, IdentityIlkOption};
use fluxbee_sdk::protocol::{
    Destination, MemoryPackage, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{
    managed_node_config_path, managed_node_name, NodeConfig, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, VaultCallerOwned, VaultClient,
};
use fluxbee_sdk::{MSG_ILK_REGISTER, MSG_TNT_CREATE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs as tokio_fs;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tokio::task::JoinSet;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const MSG_NODE_STATUS_GET: &str = "NODE_STATUS_GET";
const MSG_NODE_STATUS_GET_RESPONSE: &str = "NODE_STATUS_GET_RESPONSE";
const NODE_STATUS_DEFAULT_HANDLER_ENABLED: &str = "NODE_STATUS_DEFAULT_HANDLER_ENABLED";
const NODE_STATUS_DEFAULT_HEALTH_STATE: &str = "NODE_STATUS_DEFAULT_HEALTH_STATE";
const GOV_IDENTITY_TARGET_ENV: &str = "GOV_IDENTITY_TARGET";
const GOV_IDENTITY_TIMEOUT_MS_ENV: &str = "GOV_IDENTITY_TIMEOUT_MS";
const GOV_IDENTITY_TENANT_ID_ENV: &str = "GOV_IDENTITY_TENANT_ID";
const IMMEDIATE_INTERACTION_MAX_CHARS: usize = 1_200;
const AI_RUNTIME_KIND: &str = "ai.generic";
const DEFAULT_AGENT_ASSET_BLOB_ROOT: &str = "/var/lib/fluxbee/blob";
const AGENT_ASSET_MAX_BYTES: u64 = 256 * 1024;
const COMPOSED_PROMPT_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_COGNITIVE_POLL_INTERVAL_SECS: u64 = 10;
const DEFAULT_UNCONFIGURED_AGENT_PROMPT: &str = r#"You are an AI agent that has not yet received its operational configuration.

If you receive a message, respond with this exact structure:
{
  "status": "unconfigured",
  "message": "This agent has not been configured yet. Please contact the system administrator to assign a role, skills, and handbooks."
}

Do not interpret messages beyond confirming receipt.
Do not perform any action."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiApiKeySource {
    /// Model D' — secret resolved via `fluxbee_sdk::resolve_resource(Openai)`
    /// (pool match against SY.vault). This is the only supported source.
    Vault,
    Missing,
}

impl OpenAiApiKeySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Vault => "vault",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RunnerConfig {
    node: NodeSection,
    #[serde(default)]
    runtime: RuntimeSection,
    behavior: BehaviorSection,
}

#[derive(Debug, Deserialize)]
struct NodeSection {
    name: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default = "default_router_socket")]
    router_socket: String,
    #[serde(default = "default_state_dir")]
    uuid_persistence_dir: String,
    #[serde(default = "default_config_dir")]
    config_dir: String,
    #[serde(default = "default_dynamic_config_dir")]
    dynamic_config_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RuntimeSection {
    read_timeout_ms: u64,
    handler_timeout_ms: u64,
    write_timeout_ms: u64,
    queue_capacity: usize,
    worker_pool_size: usize,
    retry_max_attempts: usize,
    retry_initial_backoff_ms: u64,
    retry_max_backoff_ms: u64,
    metrics_log_interval_ms: u64,
    #[serde(default)]
    immediate_memory: ImmediateMemorySection,
    #[serde(default)]
    cognitive_definition: CognitiveDefinitionSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ImmediateMemorySection {
    enabled: bool,
    recent_interactions_max: usize,
    active_operations_max: usize,
    summary_max_chars: usize,
    summary_refresh_every_turns: usize,
    trim_noise_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct CognitiveDefinitionSection {
    enabled: bool,
    poll_interval_secs: u64,
    #[serde(default)]
    blob_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BehaviorSection {
    Echo,
    OpenaiChat(OpenAiChatSection),
}

#[derive(Debug, Deserialize)]
struct OpenAiChatSection {
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    instructions: Option<InstructionsSourceConfig>,
    #[serde(default)]
    model_settings: Option<RunnerModelSettings>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    capabilities: Option<BehaviorCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BehaviorCapabilities {
    #[serde(default)]
    multimodal: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RunnerModelSettings {
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InstructionsSourceConfig {
    // Backward-compatible short form:
    // instructions: "You are concise"
    Inline(String),
    // Structured strategy:
    // instructions:
    //   source: file|env|inline|none
    //   value: /path/file.txt | ENV_VAR | inline text
    //   trim: true|false (default true)
    Strategy(InstructionsStrategy),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstructionsStrategy {
    source: InstructionsSourceKind,
    #[serde(default)]
    value: Option<String>,
    #[serde(default = "default_trim_true")]
    trim: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InstructionsSourceKind {
    Inline,
    File,
    Env,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EffectiveStateFile {
    schema_version: u32,
    config_version: u64,
    node_name: String,
    config: EffectiveConfigDocument,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveConfigDocument {
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    node: Option<EffectiveNodeSection>,
    #[serde(default)]
    behavior: EffectiveBehaviorSection,
    #[serde(default)]
    runtime: Option<EffectiveRuntimeSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveNodeSection {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    router_socket: Option<String>,
    #[serde(default)]
    uuid_persistence_dir: Option<String>,
    #[serde(default)]
    config_dir: Option<String>,
    #[serde(default)]
    dynamic_config_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveBehaviorSection {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    params: Option<EffectiveBehaviorParams>,
    #[serde(default)]
    instructions: Option<Value>,
    #[serde(default)]
    model_settings: Option<RunnerModelSettings>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    capabilities: Option<BehaviorCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveBehaviorParams {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveRuntimeSection {
    #[serde(default)]
    read_timeout_ms: Option<u64>,
    #[serde(default)]
    handler_timeout_ms: Option<u64>,
    #[serde(default)]
    write_timeout_ms: Option<u64>,
    #[serde(default)]
    queue_capacity: Option<usize>,
    #[serde(default)]
    worker_pool_size: Option<usize>,
    #[serde(default)]
    retry_max_attempts: Option<usize>,
    #[serde(default)]
    retry_initial_backoff_ms: Option<u64>,
    #[serde(default)]
    retry_max_backoff_ms: Option<u64>,
    #[serde(default)]
    metrics_log_interval_ms: Option<u64>,
    #[serde(default)]
    immediate_memory: Option<ImmediateMemorySection>,
    #[serde(default)]
    cognitive_definition: Option<CognitiveDefinitionSection>,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            read_timeout_ms: 30_000,
            handler_timeout_ms: 60_000,
            write_timeout_ms: 10_000,
            queue_capacity: 128,
            worker_pool_size: 4,
            retry_max_attempts: 3,
            retry_initial_backoff_ms: 200,
            retry_max_backoff_ms: 2_000,
            metrics_log_interval_ms: 30_000,
            immediate_memory: ImmediateMemorySection::default(),
            cognitive_definition: CognitiveDefinitionSection::default(),
        }
    }
}

impl Default for ImmediateMemorySection {
    fn default() -> Self {
        Self {
            enabled: false,
            recent_interactions_max: 10,
            active_operations_max: 8,
            summary_max_chars: 1_600,
            summary_refresh_every_turns: 3,
            trim_noise_enabled: true,
        }
    }
}

impl Default for CognitiveDefinitionSection {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: DEFAULT_COGNITIVE_POLL_INTERVAL_SECS,
            blob_root: None,
        }
    }
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_router_socket() -> String {
    "/var/run/fluxbee/routers".to_string()
}

fn default_state_dir() -> String {
    "/var/lib/fluxbee/state/nodes".to_string()
}

fn default_config_dir() -> String {
    "/etc/fluxbee".to_string()
}

fn default_dynamic_config_dir() -> String {
    "/var/lib/fluxbee/state/ai-nodes".to_string()
}

fn default_model() -> String {
    "gpt-4.1-mini".to_string()
}

fn default_multimodal_for_runtime() -> bool {
    true
}

fn default_trim_true() -> bool {
    true
}

#[derive(Debug, Clone)]
enum NodeBehavior {
    Echo,
    OpenAiChat(OpenAiChatRuntime),
}

#[derive(Debug, Clone)]
struct OpenAiChatRuntime {
    model: String,
    instructions: Option<String>,
    model_settings: ModelSettings,
    base_url: Option<String>,
    immediate_memory: ImmediateMemorySection,
    multimodal: bool,
}

struct GenericAiNode {
    mode: RunnerMode,
    node_name: String,
    /// Self ILK, read from `FLUXBEE_NODE_ILK_ID` env injected by orchestrator
    /// at spawn (after `ILK_REGISTER` to SY.identity). `None` only when the
    /// node was started manually outside the orchestrator pipeline; in that
    /// case vault and any identity-bearing call will be rejected by auth.
    self_ilk_id: Option<String>,
    /// Self tenant, read from `FLUXBEE_NODE_TENANT_ID` env injected by
    /// orchestrator at spawn. Same lifecycle as `self_ilk_id`.
    self_tenant_id: Option<String>,
    behavior: Arc<RwLock<Option<NodeBehavior>>>,
    config_dir: PathBuf,
    dynamic_config_dir: PathBuf,
    /// Router socket directory — kept for tracing/debug purposes only.
    router_socket: PathBuf,
    /// UUID persistence root — kept for tracing/debug purposes only.
    state_dir: PathBuf,
    thread_state_store: Option<Arc<dyn ThreadStateStore>>,
    immediate_memory_store: Option<Arc<ImmediateMemoryStore>>,
    gov_identity: GovIdentityConfig,
    /// Vault accessor over the canonical `Arc<RouterDispatcher>`. `None` when
    /// `self_ilk_id` / hive suffix is missing — the node still boots in a
    /// degraded state.
    vault: Option<VaultClient>,
    control_plane: Arc<RwLock<ControlPlaneState>>,
    cognitive_definition: Arc<RwLock<CognitiveDefinitionRuntimeState>>,
    cognitive_definition_config: CognitiveDefinitionRuntimeConfig,
}

#[derive(Debug, Clone)]
struct GovIdentityConfig {
    target: String,
    fallback_target: Option<String>,
    timeout: Duration,
}

impl Default for GovIdentityConfig {
    fn default() -> Self {
        Self {
            target: "SY.identity@motherbee".to_string(),
            fallback_target: None,
            timeout: Duration::from_secs(10),
        }
    }
}

// `SharedRouterConnection` and `GovIdentityBridge` were home-grown
// trace_id multiplexers built on top of `RouterClient` from
// `fluxbee_ai_sdk`. Both are eliminated by the global `RouterDispatcher`
// unification: the dispatcher carries the canonical pending-matcher table,
// and identity calls go through `send_with_matcher`. The ai-generic node
// never actually wired a `gov_identity_bridge: Some(...)` in practice
// (always `None`) — the gov-mode path lived on `ai-frontdesk-gov`. The
// associated dead code is removed.
struct GovIdentityBridge {
    dispatcher: Arc<RouterDispatcher>,
}

impl GovIdentityBridge {
    #[allow(dead_code)]
    fn new(dispatcher: Arc<RouterDispatcher>) -> Self {
        Self { dispatcher }
    }

    async fn call_ok(
        &self,
        identity: &GovIdentityConfig,
        action: &str,
        payload: Value,
    ) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, String> {
        let first = self
            .send_action_once(&identity.target, action, payload.clone(), identity.timeout)
            .await;

        match first {
            Ok(out) => {
                let status = out.payload.get("status").and_then(Value::as_str);
                let error_code = out.payload.get("error_code").and_then(Value::as_str);
                if status == Some("error") && error_code == Some("NOT_PRIMARY") {
                    if let Some(fallback) = identity.fallback_target.as_deref() {
                        if !fallback.trim().is_empty() && fallback != identity.target {
                            return self
                                .send_action_once(fallback, action, payload, identity.timeout)
                                .await;
                        }
                    }
                }
                if status == Some("ok") {
                    Ok(out)
                } else {
                    Err(format!(
                        "identity action rejected: action={action}, error_code={}, message={}",
                        error_code.unwrap_or("UNKNOWN"),
                        out.payload
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("identity returned non-ok status")
                    ))
                }
            }
            Err(err) => {
                let use_fallback = err.contains("original_dst=") && err.contains("NODE_NOT_FOUND");
                if use_fallback {
                    if let Some(fallback) = identity.fallback_target.as_deref() {
                        if !fallback.trim().is_empty() && fallback != identity.target {
                            return self
                                .send_action_once(fallback, action, payload, identity.timeout)
                                .await;
                        }
                    }
                }
                Err(err)
            }
        }
    }

    async fn send_action_once(
        &self,
        target: &str,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, String> {
        let trace_id = Uuid::new_v4().to_string();
        let req = Message {
            routing: Routing {
                src: String::new(),
                src_l2_name: None,
                dst: Destination::Unicast(target.to_string()),
                ttl: 16,
                trace_id: trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(action.to_string()),
                ..Meta::default()
            },
            payload,
        };
        let expected_msg = format!("{action}_RESPONSE");
        let matcher = fluxbee_sdk::PendingMatcher::new(
            vec![fluxbee_sdk::RouteMatch::exact(SYSTEM_KIND, &expected_msg)],
            vec![
                fluxbee_sdk::RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
                fluxbee_sdk::RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
            ],
            vec![fluxbee_sdk::RouteMatch::any_msg_type(SYSTEM_KIND)],
        );
        let labels = fluxbee_sdk::RpcRequestLabels::new(target, action, expected_msg.clone());
        let msg = self
            .dispatcher
            .send_with_matcher(req, matcher, labels, timeout)
            .await
            .map_err(|err| format!("identity send failed: {err}"))?;
        Self::parse_identity_reply(msg, &expected_msg, target, trace_id)
    }

    fn parse_identity_reply(
        msg: Message,
        expected_msg: &str,
        target: &str,
        trace_id: String,
    ) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, String> {
        if msg.meta.msg.as_deref() == Some(expected_msg) {
            return Ok(fluxbee_sdk::IdentitySystemResult {
                payload: msg.payload,
                effective_target: target.to_string(),
                trace_id,
            });
        }
        if msg.meta.msg.as_deref() == Some(MSG_UNREACHABLE) {
            let original_dst = msg
                .payload
                .get("original_dst")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reason = msg
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(format!(
                "identity transport unreachable: reason={reason}, original_dst={original_dst}"
            ));
        }
        if msg.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED) {
            let original_dst = msg
                .payload
                .get("original_dst")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let last_hop = msg
                .payload
                .get("last_hop")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(format!(
                "identity transport ttl exceeded: original_dst={original_dst}, last_hop={last_hop}"
            ));
        }
        Err(format!(
            "invalid identity response: expected {expected_msg} trace_id={trace_id}, got msg={:?}",
            msg.meta.msg
        ))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IlkRegisterIdentityCandidate {
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    tenant_hint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IlkRegisterArgs {
    src_ilk: String,
    identity_candidate: IlkRegisterIdentityCandidate,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
}

#[derive(Clone)]
struct IlkRegisterTool {
    scoped_src_ilk: Option<String>,
    default_tenant_id: Option<String>,
    identity: GovIdentityConfig,
    bridge: Option<Arc<GovIdentityBridge>>,
}

#[derive(Clone)]
struct GenerateCsvArtifactTool;

#[derive(Clone)]
struct GenerateTextArtifactTool;

#[derive(Clone)]
struct GenerateJsonArtifactTool;

#[derive(Clone)]
struct GenerateMarkdownArtifactTool;

#[derive(Clone)]
struct GenerateHtmlArtifactTool;

#[derive(Clone)]
struct GeneratePdfArtifactTool;

#[derive(Clone)]
struct GenerateXlsxArtifactTool;

#[derive(Clone)]
struct GenerateDocxArtifactTool;

#[derive(Clone)]
struct GeneratePngArtifactTool;

#[derive(Clone)]
struct GenerateJpegArtifactTool;

#[derive(Debug, Clone)]
struct BehaviorContext {
    thread_id: Option<String>,
    src_ilk: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateCsvArtifactArgs {
    filename: String,
    rows: Vec<Vec<String>>,
    #[serde(default)]
    headers: Option<Vec<String>>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateTextArtifactArgs {
    filename: String,
    content: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateJsonArtifactArgs {
    filename: String,
    data: Value,
    #[serde(default = "default_true")]
    pretty: bool,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateMarkdownArtifactArgs {
    filename: String,
    content: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateHtmlArtifactArgs {
    filename: String,
    content: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GeneratePdfArtifactArgs {
    filename: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    lines: Vec<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateXlsxArtifactArgs {
    filename: String,
    rows: Vec<Vec<String>>,
    #[serde(default)]
    headers: Option<Vec<String>>,
    #[serde(default)]
    sheet_name: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateDocxArtifactArgs {
    filename: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    paragraphs: Vec<String>,
    #[serde(default)]
    bullets: Vec<String>,
    #[serde(default)]
    table_headers: Option<Vec<String>>,
    #[serde(default)]
    table_rows: Vec<Vec<String>>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateRasterArtifactArgs {
    filename: String,
    #[serde(default)]
    bands: Vec<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolFinalArtifactEnvelope {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    artifacts: Vec<ToolFinalArtifactItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ToolFinalArtifactItem {
    filename: String,
    mime: String,
    bytes_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedImmediateMemoryRecord {
    #[serde(default)]
    summary: Option<fluxbee_ai_sdk::ConversationSummary>,
    #[serde(default)]
    recent_interactions: Vec<fluxbee_ai_sdk::ImmediateInteraction>,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct ImmediateMemoryStore {
    root_dir: PathBuf,
    key_gates: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ImmediateMemoryStore {
    fn path_for_node(state_dir: &std::path::Path, node_name: &str) -> PathBuf {
        state_dir
            .join("ai-nodes")
            .join(sanitize_storage_key(node_name))
            .join("immediate-memory")
    }

    fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            key_gates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn root_dir(&self) -> &std::path::Path {
        &self.root_dir
    }

    fn records_dir(&self) -> PathBuf {
        self.root_dir.join("threads")
    }

    fn key_file_path(&self, key: &str) -> PathBuf {
        self.records_dir()
            .join(format!("{}.json", sanitize_storage_key(key)))
    }

    async fn ensure_ready(&self) -> fluxbee_ai_sdk::Result<()> {
        tokio_fs::create_dir_all(self.records_dir())
            .await
            .map_err(|err| {
                fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                    "immediate memory init failed: {err}"
                ))
            })?;
        Ok(())
    }

    async fn lock_key(&self, key: &str) -> OwnedMutexGuard<()> {
        let safe = sanitize_storage_key(key);
        let gate = {
            let mut gates = self.key_gates.lock().await;
            gates
                .entry(safe)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        gate.lock_owned().await
    }

    async fn get(
        &self,
        key: &str,
    ) -> fluxbee_ai_sdk::Result<Option<PersistedImmediateMemoryRecord>> {
        if key.trim().is_empty() {
            return Ok(None);
        }
        let _guard = self.lock_key(key).await;
        let path = self.key_file_path(key);
        let raw = match tokio_fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                    "immediate memory read failed: {err}"
                )))
            }
        };
        let parsed = serde_json::from_str::<PersistedImmediateMemoryRecord>(&raw)?;
        Ok(Some(parsed))
    }

    async fn put(
        &self,
        key: &str,
        record: &PersistedImmediateMemoryRecord,
    ) -> fluxbee_ai_sdk::Result<()> {
        if key.trim().is_empty() {
            return Ok(());
        }
        let _guard = self.lock_key(key).await;
        self.ensure_ready().await?;
        let path = self.key_file_path(key);
        let raw = serde_json::to_string_pretty(record)?;
        tokio_fs::write(path, raw).await.map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "immediate memory write failed: {err}"
            ))
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeLifecycleState {
    Unconfigured,
    Configured,
    FailedConfig,
}

impl NodeLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "UNCONFIGURED",
            Self::Configured => "CONFIGURED",
            Self::FailedConfig => "FAILED_CONFIG",
        }
    }
}

#[derive(Debug)]
struct ControlPlaneState {
    current_state: NodeLifecycleState,
    config_source: &'static str,
    effective_config: Option<Value>,
    schema_version: u32,
    config_version: u64,
}

impl Default for ControlPlaneState {
    fn default() -> Self {
        Self {
            current_state: NodeLifecycleState::Unconfigured,
            config_source: "none",
            effective_config: None,
            schema_version: 0,
            config_version: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct CognitiveDefinitionRuntimeConfig {
    enabled: bool,
    poll_interval: Duration,
    blob_root: PathBuf,
}

impl From<CognitiveDefinitionSection> for CognitiveDefinitionRuntimeConfig {
    fn from(value: CognitiveDefinitionSection) -> Self {
        let poll_secs = value.poll_interval_secs.max(1).min(300);
        Self {
            enabled: value.enabled,
            poll_interval: Duration::from_secs(poll_secs),
            blob_root: value
                .blob_root
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_AGENT_ASSET_BLOB_ROOT)),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CognitiveDefinitionHashes {
    role_hash: Option<String>,
    skill_hashes: Vec<String>,
    handbook_hashes: Vec<String>,
    personality_hash: Option<String>,
}

impl CognitiveDefinitionHashes {
    fn is_empty(&self) -> bool {
        self.role_hash.is_none()
            && self.skill_hashes.is_empty()
            && self.handbook_hashes.is_empty()
            && self.personality_hash.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CognitiveAssetFailure {
    hash: String,
    asset_type: String,
    error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CognitiveDefinitionRuntimeState {
    enabled: bool,
    definition_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ilk_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role_hash_loaded: Option<String>,
    #[serde(default)]
    skill_hashes_loaded: Vec<String>,
    #[serde(default)]
    handbook_hashes_loaded: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    personality_hash_loaded: Option<String>,
    #[serde(default)]
    failed_hashes: Vec<CognitiveAssetFailure>,
    prompt_truncated: bool,
    active_prompt_chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_recompose_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_identity_seq: Option<u64>,
    #[serde(skip)]
    last_hashes: CognitiveDefinitionHashes,
    #[serde(skip)]
    active_prompt: Option<String>,
}

impl CognitiveDefinitionRuntimeState {
    fn disabled() -> Self {
        Self {
            enabled: false,
            definition_state: "disabled".to_string(),
            ilk_id: None,
            role_hash_loaded: None,
            skill_hashes_loaded: Vec::new(),
            handbook_hashes_loaded: Vec::new(),
            personality_hash_loaded: None,
            failed_hashes: Vec::new(),
            prompt_truncated: false,
            active_prompt_chars: 0,
            last_recompose_at: None,
            last_identity_seq: None,
            last_hashes: CognitiveDefinitionHashes::default(),
            active_prompt: None,
        }
    }

    fn unresolved(enabled: bool) -> Self {
        Self {
            enabled,
            definition_state: if enabled { "unresolved" } else { "disabled" }.to_string(),
            ..Self::disabled()
        }
    }

    fn has_failures(&self) -> bool {
        !self.failed_hashes.is_empty()
    }
}

fn should_reuse_cognitive_state(
    current: &CognitiveDefinitionRuntimeState,
    ilk_id: &str,
    hashes: &CognitiveDefinitionHashes,
) -> bool {
    current.enabled
        && current.last_identity_seq.is_some()
        && current.ilk_id.as_deref() == Some(ilk_id)
        && current.last_hashes == *hashes
        && current.active_prompt.is_some()
        && !current.has_failures()
}

#[async_trait]
impl AiNode for GenericAiNode {
    async fn on_message(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        if is_control_plane(&msg) {
            return self.handle_control_plane(msg).await;
        }
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            let state = self.control_plane.read().await.current_state;
            if state != NodeLifecycleState::Configured {
                let payload = node_not_configured_payload(state);
                return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
            }
            if extract_thread_id(&msg).is_none() {
                let payload = invalid_payload_missing_thread_id();
                return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
            }
        }
        let behavior_ctx = BehaviorContext {
            thread_id: extract_thread_id(&msg),
            src_ilk: extract_src_ilk(&msg),
        };
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            let src_ilk_source = src_ilk_source(&msg);
            if behavior_ctx.src_ilk.is_none() {
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    src_ilk_source = src_ilk_source,
                    "missing src_ilk in incoming user message"
                );
            } else {
                tracing::debug!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    src_ilk_source = src_ilk_source,
                    "resolved src_ilk in incoming user message"
                );
            }
        }

        let behavior = self.behavior.read().await.clone();
        let Some(behavior) = behavior else {
            let payload = node_runtime_not_ready_payload();
            return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
        };

        let (input, resolved_user_input): (String, Option<ResolvedModelInput>) = if msg
            .meta
            .msg_type
            .eq_ignore_ascii_case("user")
        {
            let options = ModelInputOptions {
                multimodal: matches!(&behavior, NodeBehavior::OpenAiChat(openai) if openai.multimodal),
                ..ModelInputOptions::default()
            };
            match resolve_model_input_from_payload_with_options(&msg.payload, &options).await {
                Ok(value) => (value.prompt_text.clone(), Some(value)),
                Err(err) => {
                    return Ok(Some(build_reply_message_runtime_src(
                        &msg,
                        err.to_error_payload(),
                    )))
                }
            }
        } else {
            (extract_text(&msg.payload).unwrap_or_default(), None)
        };
        let cognition_block = render_memory_package_prompt_block(msg.meta.memory_package.as_ref());
        let input = inject_memory_package_into_text_input(&input, cognition_block.as_deref());
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                src_ilk = ?behavior_ctx.src_ilk,
                sender = ?incoming_sender_hint(&msg),
                thread_id = ?behavior_ctx.thread_id,
                memory_package = msg.meta.memory_package.is_some(),
                input_len = input.len(),
                input_preview = %text_preview(&input, 240),
                "incoming user message"
            );
        }
        let output = match &behavior {
            NodeBehavior::Echo => AiBehaviorOutput::text(format!("Echo: {input}")),
            NodeBehavior::OpenAiChat(openai) => {
                let input_parts = if openai.multimodal {
                    if let Some(resolved) = resolved_user_input.as_ref() {
                        match build_openai_user_content_parts(resolved).await {
                            Ok(parts) => Some(inject_memory_package_into_input_parts(
                                parts,
                                cognition_block.as_deref(),
                            )),
                            Err(err) => {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    error = %err,
                                    "failed to build structured user input parts; replying with canonical attachment error payload"
                                );
                                return Ok(Some(build_reply_message_runtime_src(
                                    &msg,
                                    err.to_error_payload(),
                                )));
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match self
                    .run_openai_chat(openai, input, input_parts, &behavior_ctx, &msg.meta)
                    .await
                {
                    Ok(output) => output,
                    Err(err) if err.to_string().contains("missing OpenAI api key") => {
                        tracing::warn!(
                            node_name = %self.node_name,
                            trace_id = %msg.routing.trace_id,
                            error = %err,
                            "openai runtime missing api key; replying with runtime-not-ready payload"
                        );
                        let payload = missing_openai_api_key_payload();
                        return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
                    }
                    Err(err) => {
                        let attachment_summary =
                            attachment_summary_for_observability(resolved_user_input.as_ref());
                        if let fluxbee_ai_sdk::errors::AiSdkError::Protocol(msg_text) = &err {
                            if let Some((status, detail)) = parse_openai_status_error(msg_text) {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    model = %openai.model,
                                    provider_status = status,
                                    provider_param = ?extract_openai_error_param(&detail),
                                    provider_detail = %trim_chars(&detail, 280),
                                    attachment_count = attachment_summary.count,
                                    attachment_total_bytes = attachment_summary.total_bytes,
                                    attachment_mimes = ?attachment_summary.mimes,
                                    error = %err,
                                    "openai runtime request failed with structured provider status; replying with provider error payload"
                                );
                            } else {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    model = %openai.model,
                                    attachment_count = attachment_summary.count,
                                    attachment_total_bytes = attachment_summary.total_bytes,
                                    attachment_mimes = ?attachment_summary.mimes,
                                    error = %err,
                                    "openai runtime request failed; replying with provider error payload"
                                );
                            }
                        } else {
                            tracing::warn!(
                                node_name = %self.node_name,
                                trace_id = %msg.routing.trace_id,
                                model = %openai.model,
                                attachment_count = attachment_summary.count,
                                attachment_total_bytes = attachment_summary.total_bytes,
                                attachment_mimes = ?attachment_summary.mimes,
                                error = %err,
                                "openai runtime request failed; replying with provider error payload"
                            );
                        }
                        let payload = openai_runtime_error_payload(&err);
                        return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
                    }
                }
            }
        };

        let payload = match build_ai_behavior_response(output) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    error = %err,
                    "failed to build final AI response with user-facing artifacts; replying with artifact generation error payload"
                );
                ai_final_output_error_payload(&err)
            }
        };
        Ok(Some(build_reply_message_runtime_src(&msg, payload)))
    }
}

impl GenericAiNode {
    async fn run_openai_chat(
        &self,
        openai: &OpenAiChatRuntime,
        input: String,
        input_parts: Option<Vec<Value>>,
        ctx: &BehaviorContext,
        meta: &Meta,
    ) -> fluxbee_ai_sdk::Result<AiBehaviorOutput> {
        let api_key = self.resolve_openai_api_key(openai).await.ok_or_else(|| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(
                "missing OpenAI api key in SY.vault resource_type=openai".to_string(),
            )
        })?;
        let mut client = OpenAiResponsesClient::new(api_key);
        if let Some(base_url) = &openai.base_url {
            client = client.with_base_url(base_url.clone());
        }
        let output_schema = resolve_response_envelope_output_schema(meta)?;
        let tool_registry = self.build_tool_registry(ctx)?;
        let tool_count = tool_registry.definitions().len();
        let output_contract_mode = match (output_schema.as_ref(), tool_count > 0) {
            (Some(_), true) => "fallback_instruction",
            (Some(_), false) => "structured_output",
            (None, _) => "free_text",
        };
        tracing::info!(
            node_name = %self.node_name,
            thread_id = ?ctx.thread_id,
            src_ilk = ?ctx.src_ilk,
            tool_count,
            output_contract_mode,
            output_schema_name = ?output_schema.as_ref().map(|schema| schema.name()),
            "openai chat request prepared"
        );
        if !tool_registry.definitions().is_empty() {
            let base_instructions = self.effective_openai_instructions(openai).await;
            let system = match (&base_instructions, &output_schema) {
                (Some(base), Some(schema)) => Some(format!(
                    "{base}\n\n{}",
                    build_output_schema_fallback_instruction(schema)?
                )),
                (None, Some(schema)) => Some(build_output_schema_fallback_instruction(schema)?),
                (base, None) => base.clone(),
            };
            let model = client.clone().function_model(
                openai.model.clone(),
                system,
                openai.model_settings.clone(),
            );
            let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
            let immediate_memory = self.load_immediate_memory_for_input(openai, ctx).await;
            let run_input = self.build_function_run_input(
                input.clone(),
                input_parts.clone(),
                ctx,
                openai,
                immediate_memory,
            );
            let result = runner
                .run_with_input(&model, &tool_registry, run_input)
                .await?;
            if let Some(output) = extract_final_output_from_tool_results(&result)? {
                tracing::info!(
                    node_name = %self.node_name,
                    thread_id = ?ctx.thread_id,
                    src_ilk = ?ctx.src_ilk,
                    output_summary = %summarize_behavior_output(&output),
                    "tool run produced explicit final_output with user-facing artifacts"
                );
                let summary_text = summarize_behavior_output(&output);
                self.persist_immediate_turn(openai, ctx, &input, &summary_text)
                    .await;
                return Ok(output);
            }
            if let Some(text) = result.final_assistant_text {
                self.persist_immediate_turn(openai, ctx, &input, &text)
                    .await;
                return Ok(AiBehaviorOutput::text(text));
            }
        }

        let current_user_input = input.clone();
        let system = self.effective_openai_instructions(openai).await;
        let req = fluxbee_ai_sdk::llm::LlmRequest {
            model: openai.model.clone(),
            system,
            input,
            input_parts,
            output_schema,
            max_output_tokens: None,
            model_settings: Some(openai.model_settings.clone()),
        };
        let response = fluxbee_ai_sdk::llm::LlmClient::generate(&client, req).await?;
        self.persist_immediate_turn(openai, ctx, &current_user_input, &response.content)
            .await;
        Ok(AiBehaviorOutput::text(response.content))
    }

    async fn effective_openai_instructions(&self, openai: &OpenAiChatRuntime) -> Option<String> {
        let state = self.cognitive_definition.read().await;
        state
            .active_prompt
            .clone()
            .or_else(|| openai.instructions.clone())
    }

    fn build_function_run_input(
        &self,
        input: String,
        input_parts: Option<Vec<Value>>,
        ctx: &BehaviorContext,
        openai: &OpenAiChatRuntime,
        immediate_memory: Option<ImmediateConversationMemory>,
    ) -> FunctionRunInput {
        if !openai.immediate_memory.enabled {
            return FunctionRunInput {
                current_user_message: input,
                current_user_parts: input_parts,
                immediate_memory: None,
            };
        }
        FunctionRunInput {
            current_user_message: input,
            current_user_parts: input_parts,
            immediate_memory: immediate_memory.or_else(|| {
                Some(ImmediateConversationMemory {
                    thread_id: ctx.thread_id.clone(),
                    scope_id: ctx.src_ilk.clone(),
                    summary: None,
                    recent_interactions: Vec::new(),
                    active_operations: Vec::new(),
                })
            }),
        }
    }

    async fn load_immediate_memory_for_input(
        &self,
        openai: &OpenAiChatRuntime,
        ctx: &BehaviorContext,
    ) -> Option<ImmediateConversationMemory> {
        if !openai.immediate_memory.enabled {
            return None;
        }
        let src_ilk = ctx.src_ilk.as_deref()?;
        let store = self.immediate_memory_store.as_ref()?;
        let record = match store.get(src_ilk).await {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    src_ilk = %src_ilk,
                    thread_id = ?ctx.thread_id,
                    error = %err,
                    "immediate memory get failed; continuing without persisted context"
                );
                None
            }
        };

        let (summary, recent_interactions) = if let Some(mut record) = record {
            record.summary = record
                .summary
                .map(|summary| trim_summary(summary, openai.immediate_memory.summary_max_chars));
            record.recent_interactions = prune_recent_interactions(
                record.recent_interactions,
                openai.immediate_memory.recent_interactions_max,
            );
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                memory_hit = true,
                recent_interactions = record.recent_interactions.len(),
                recent_interactions_max = openai.immediate_memory.recent_interactions_max,
                active_operations_max = openai.immediate_memory.active_operations_max,
                summary_max_chars = openai.immediate_memory.summary_max_chars,
                summary_refresh_status = "not_implemented_v1",
                "immediate memory loaded"
            );
            (record.summary, record.recent_interactions)
        } else {
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                memory_hit = false,
                recent_interactions_max = openai.immediate_memory.recent_interactions_max,
                active_operations_max = openai.immediate_memory.active_operations_max,
                summary_max_chars = openai.immediate_memory.summary_max_chars,
                summary_refresh_status = "not_implemented_v1",
                "immediate memory loaded"
            );
            (None, Vec::new())
        };

        Some(ImmediateConversationMemory {
            thread_id: ctx.thread_id.clone(),
            scope_id: ctx.src_ilk.clone(),
            summary,
            recent_interactions,
            active_operations: Vec::new(),
        })
    }

    async fn persist_immediate_turn(
        &self,
        openai: &OpenAiChatRuntime,
        ctx: &BehaviorContext,
        user_input: &str,
        assistant_output: &str,
    ) {
        if !openai.immediate_memory.enabled {
            return;
        }
        let Some(src_ilk) = ctx.src_ilk.as_deref() else {
            return;
        };
        let Some(store) = self.immediate_memory_store.as_ref() else {
            return;
        };

        let mut record = match store.get(src_ilk).await {
            Ok(Some(record)) => record,
            Ok(None) => PersistedImmediateMemoryRecord::default(),
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    src_ilk = %src_ilk,
                    thread_id = ?ctx.thread_id,
                    error = %err,
                    "immediate memory get-before-put failed; skipping persistence"
                );
                return;
            }
        };
        record.summary = record
            .summary
            .map(|summary| trim_summary(summary, openai.immediate_memory.summary_max_chars));
        record
            .recent_interactions
            .push(fluxbee_ai_sdk::ImmediateInteraction {
                role: fluxbee_ai_sdk::ImmediateRole::User,
                kind: fluxbee_ai_sdk::ImmediateInteractionKind::Text,
                content: trim_chars(user_input, IMMEDIATE_INTERACTION_MAX_CHARS),
            });
        record
            .recent_interactions
            .push(fluxbee_ai_sdk::ImmediateInteraction {
                role: fluxbee_ai_sdk::ImmediateRole::Assistant,
                kind: fluxbee_ai_sdk::ImmediateInteractionKind::Text,
                content: trim_chars(assistant_output, IMMEDIATE_INTERACTION_MAX_CHARS),
            });
        record.recent_interactions = prune_recent_interactions(
            record.recent_interactions,
            openai.immediate_memory.recent_interactions_max,
        );
        record.updated_at = chrono::Utc::now().to_rfc3339();

        if let Err(err) = store.put(src_ilk, &record).await {
            tracing::warn!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                error = %err,
                "immediate memory put failed; continuing without persistence"
            );
        } else {
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                persisted_recent_interactions = record.recent_interactions.len(),
                recent_interactions_max = openai.immediate_memory.recent_interactions_max,
                summary_refresh_status = "not_implemented_v1",
                "immediate memory persisted"
            );
        }
    }

    fn build_tool_registry(
        &self,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<FunctionToolRegistry> {
        let mut registry = FunctionToolRegistry::new();
        self.register_common_tools(&mut registry, ctx)?;
        Ok(registry)
    }

    fn register_common_tools(
        &self,
        registry: &mut FunctionToolRegistry,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<()> {
        // Thread state tools remain the source for node-level "hard state".
        // In scoped AI runtimes the canonical key is src_ilk; thread_id is
        // conversational metadata only.
        // Immediate memory is managed separately by the runner as short-horizon context.
        if let (Some(store), Some(src_ilk)) = (&self.thread_state_store, &ctx.src_ilk) {
            let provider = ThreadStateToolsProvider::with_get_put_delete_scoped(
                store.clone(),
                src_ilk.clone(),
            );
            provider.register_tools(registry)?;
        }
        registry.register(Arc::new(GenerateCsvArtifactTool))?;
        registry.register(Arc::new(GenerateTextArtifactTool))?;
        registry.register(Arc::new(GenerateJsonArtifactTool))?;
        registry.register(Arc::new(GenerateMarkdownArtifactTool))?;
        registry.register(Arc::new(GenerateHtmlArtifactTool))?;
        registry.register(Arc::new(GeneratePdfArtifactTool))?;
        registry.register(Arc::new(GenerateXlsxArtifactTool))?;
        registry.register(Arc::new(GenerateDocxArtifactTool))?;
        registry.register(Arc::new(GeneratePngArtifactTool))?;
        registry.register(Arc::new(GenerateJpegArtifactTool))?;
        Ok(())
    }

    fn register_gov_tools(
        &self,
        registry: &mut FunctionToolRegistry,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<()> {
        // ai-generic never wires a gov identity bridge — that lives in
        // ai-frontdesk-gov. The tool registers in disabled mode here.
        let tool = IlkRegisterTool {
            scoped_src_ilk: ctx.src_ilk.clone(),
            default_tenant_id: self.resolve_effective_tenant_id(),
            identity: self.gov_identity.clone(),
            bridge: None,
        };
        registry.register(Arc::new(tool))?;
        Ok(())
    }

    async fn resolve_openai_api_key(&self, openai: &OpenAiChatRuntime) -> Option<String> {
        self.resolve_openai_api_key_with_source(openai).await.0
    }

    /// Model D' — resolve the OpenAI api_key by discovering the `openai`
    /// resource in SY.vault (pool match: dedicated to caller ILK → caller
    /// tenant pool → root tenant pool, i.e.
    /// `tnt:00000000-0000-0000-0000-000000000001`). This is the **only**
    /// supported source. Plaintext config fields, env-var references, YAML
    /// inline secrets, and local `secrets.json` persistence are not accepted.
    ///
    /// Requires `self_ilk_id` and `self_tenant_id` to be present (set by
    /// the orchestrator via FLUXBEE_NODE_ILK_ID + FLUXBEE_NODE_TENANT_ID
    /// envs). If either is missing the node runs degraded and replies
    /// with a runtime-not-ready payload on chat requests.
    async fn resolve_openai_api_key_with_source(
        &self,
        _openai: &OpenAiChatRuntime,
    ) -> (Option<String>, OpenAiApiKeySource) {
        let Some(self_tenant_id) = self.self_tenant_id.as_deref().filter(|v| !v.is_empty()) else {
            tracing::warn!(
                node_name = %self.node_name,
                "FLUXBEE_NODE_TENANT_ID not set; vault lookup skipped (node running degraded)"
            );
            return (None, OpenAiApiKeySource::Missing);
        };
        let Some(vault) = self.vault.as_ref() else {
            tracing::warn!(
                node_name = %self.node_name,
                "vault client unavailable (missing self_ilk_id / hive suffix); lookup skipped"
            );
            return (None, OpenAiApiKeySource::Missing);
        };
        let result = vault
            .resolve_resource(
                fluxbee_sdk::ResourceType::Openai,
                self_tenant_id,
                Duration::from_secs(5),
            )
            .await;
        match result {
            Ok(Some(value)) => {
                let api_key = extract_openai_api_key_from_value(&value);
                if api_key.is_some() {
                    (api_key, OpenAiApiKeySource::Vault)
                } else {
                    tracing::warn!(
                        node_name = %self.node_name,
                        "vault openai secret did not carry a usable api_key"
                    );
                    (None, OpenAiApiKeySource::Missing)
                }
            }
            Ok(None) => (None, OpenAiApiKeySource::Missing),
            Err(err) => {
                tracing::warn!(error = %err, "ai-generic vault resource lookup failed");
                (None, OpenAiApiKeySource::Missing)
            }
        }
    }

    fn resolve_effective_tenant_id(&self) -> Option<String> {
        let Ok(state) = self.control_plane.try_read() else {
            return None;
        };
        state
            .effective_config
            .as_ref()
            .and_then(|v| v.get("tenant_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| looks_like_tenant_id(v))
            .map(ToString::to_string)
    }

    async fn handle_control_plane(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        let Some(command) = msg.meta.msg.as_deref() else {
            return Ok(None);
        };
        let (response_msg, response_payload) = if command.eq_ignore_ascii_case(MSG_NODE_STATUS_GET)
        {
            if !env_bool(NODE_STATUS_DEFAULT_HANDLER_ENABLED, true) {
                return Ok(None);
            }
            (
                MSG_NODE_STATUS_GET_RESPONSE,
                self.build_node_status_get_response().await,
            )
        } else if command.eq_ignore_ascii_case("CONFIG_SET") {
            ("CONFIG_RESPONSE", self.apply_config_set(&msg).await)
        } else if command.eq_ignore_ascii_case("CONFIG_GET") {
            ("CONFIG_RESPONSE", self.build_config_get_response().await)
        } else if command.eq_ignore_ascii_case("PING") {
            ("PONG", self.build_ping_response().await)
        } else if command.eq_ignore_ascii_case("STATUS") {
            ("STATUS_RESPONSE", self.build_status_response().await)
        } else {
            let state = self.control_plane.read().await.current_state;
            (
                "CONFIG_RESPONSE",
                self.error_response(
                    "unknown_system_msg",
                    format!("Unsupported control-plane command: {command}"),
                    1,
                    0,
                    state.as_str(),
                ),
            )
        };
        Ok(Some(build_control_plane_response(
            &msg,
            response_msg,
            response_payload,
        )))
    }

    async fn apply_config_set(&self, msg: &Message) -> Value {
        let subsystem = match msg.payload.get("subsystem").and_then(Value::as_str) {
            Some(value) if value == "ai_node" => value,
            Some(value) => {
                return self.invalid_config_response(
                    None,
                    None,
                    format!("Invalid payload.subsystem: expected 'ai_node', got '{value}'"),
                );
            }
            None => {
                return self.invalid_config_response(
                    None,
                    None,
                    "Missing required field: payload.subsystem".to_string(),
                );
            }
        };

        let requested_node_name = match msg.payload.get("node_name").and_then(Value::as_str) {
            Some(value) => value,
            None => {
                return self.invalid_config_response(
                    None,
                    None,
                    "Missing required field: payload.node_name".to_string(),
                );
            }
        };
        if !self.node_name_matches(requested_node_name) {
            return self.invalid_config_response(
                None,
                None,
                format!(
                    "Invalid payload.node_name: expected '{}', got '{}'",
                    self.node_name, requested_node_name
                ),
            );
        }

        let schema_version = match msg.payload.get("schema_version").and_then(Value::as_u64) {
            Some(raw) => match u32::try_from(raw) {
                Ok(value) => value,
                Err(_) => {
                    return self.invalid_config_response(
                        None,
                        None,
                        "Invalid payload.schema_version: must fit u32".to_string(),
                    );
                }
            },
            None => {
                return self.invalid_config_response(
                    None,
                    None,
                    "Missing required field: payload.schema_version".to_string(),
                );
            }
        };

        let config_version = match msg.payload.get("config_version").and_then(Value::as_u64) {
            Some(value) => value,
            None => {
                return self.invalid_config_response(
                    Some(schema_version),
                    None,
                    "Missing required field: payload.config_version".to_string(),
                );
            }
        };
        let apply_mode = match msg.payload.get("apply_mode").and_then(Value::as_str) {
            Some(value) => value,
            None => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    "Missing required field: payload.apply_mode".to_string(),
                );
            }
        };
        if apply_mode != "replace" {
            return self.error_response(
                "unsupported_apply_mode",
                format!("Unsupported payload.apply_mode='{apply_mode}' (only 'replace' is supported in current phase)"),
                schema_version,
                config_version,
                self.control_plane.read().await.current_state.as_str(),
            );
        }

        let config = match msg.payload.get("config") {
            Some(Value::Object(_)) => msg.payload.get("config").cloned().unwrap_or(Value::Null),
            Some(_) => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    "Invalid payload.config: must be an object".to_string(),
                );
            }
            None => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    "Missing required field: payload.config".to_string(),
                );
            }
        };
        if let Some(field) = find_openai_secret_contract_field(&config) {
            return self.invalid_config_response(
                Some(schema_version),
                Some(config_version),
                format!(
                    "Invalid payload.config: field '{field}' is not accepted by ai.generic; load OpenAI credentials through SY.vault with resource_type=openai"
                ),
            );
        }
        if let Some(field) = find_unsupported_ai_config_field(&config) {
            return self.invalid_config_response(
                Some(schema_version),
                Some(config_version),
                format!(
                    "Invalid payload.config: field '{field}' is not accepted by ai.generic; cognitive role/skill/handbook/personality hashes must be applied to the ILK with set_ilk_definition"
                ),
            );
        }
        let mut config_doc = match parse_effective_config_doc(&config) {
            Ok(v) => v,
            Err(err) => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    format!("Invalid payload.config schema: {err}"),
                );
            }
        };
        config_doc = materialize_effective_defaults(&self.node_name, config_doc);
        let next_behavior = match build_behavior_from_effective_config(&config_doc) {
            Ok(v) => v,
            Err(err) => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    format!("Invalid payload.config behavior: {err}"),
                );
            }
        };
        let materialized_config = match serde_json::to_value(&config_doc) {
            Ok(v) => v,
            Err(err) => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    format!("Failed to serialize effective config: {err}"),
                );
            }
        };

        let mut state = self.control_plane.write().await;
        if config_version < state.config_version {
            return self.error_response(
                "stale_config_version",
                format!(
                    "Stale config_version: received {}, current {}",
                    config_version, state.config_version
                ),
                state.schema_version,
                state.config_version,
                state.current_state.as_str(),
            );
        }
        if config_version == state.config_version && state.effective_config.is_some() {
            return self.ok_response(
                subsystem,
                state.schema_version,
                state.config_version,
                state.current_state.as_str(),
                state.effective_config.as_ref(),
            );
        }

        let prev_state = state.current_state;
        let prev_source = state.config_source;
        let prev_effective = state.effective_config.clone();
        let prev_schema = state.schema_version;
        let prev_version = state.config_version;

        state.current_state = NodeLifecycleState::Configured;
        state.config_source = "persisted";
        state.effective_config = Some(materialized_config);
        state.schema_version = schema_version;
        state.config_version = config_version;
        if let Err(err) = persist_dynamic_config(
            &self.dynamic_config_dir,
            &self.node_name,
            state.schema_version,
            state.config_version,
            &config_doc,
        ) {
            state.current_state = prev_state;
            state.config_source = prev_source;
            state.effective_config = prev_effective;
            state.schema_version = prev_schema;
            state.config_version = prev_version;
            return self.error_response(
                "config_persist_error",
                format!("Failed to persist dynamic config: {err}"),
                prev_schema,
                prev_version,
                prev_state.as_str(),
            );
        }
        *self.behavior.write().await = Some(next_behavior);

        self.ok_response(
            subsystem,
            state.schema_version,
            state.config_version,
            state.current_state.as_str(),
            state.effective_config.as_ref(),
        )
    }

    fn invalid_config_response(
        &self,
        schema_version: Option<u32>,
        config_version: Option<u64>,
        message: String,
    ) -> Value {
        self.error_response(
            "invalid_config",
            message,
            schema_version.unwrap_or(1),
            config_version.unwrap_or(0),
            NodeLifecycleState::Unconfigured.as_str(),
        )
    }

    fn ok_response(
        &self,
        subsystem: &str,
        schema_version: u32,
        config_version: u64,
        state: &str,
        effective_config: Option<&Value>,
    ) -> Value {
        json!({
            "subsystem": subsystem,
            "node_name": self.node_name.as_str(),
            "ok": true,
            "state": state,
            "schema_version": schema_version,
            "config_version": config_version,
            "error": Value::Null,
            "effective_config": effective_config.map(redact_secrets),
        })
    }

    fn error_response(
        &self,
        code: &str,
        message: String,
        schema_version: u32,
        config_version: u64,
        state: &str,
    ) -> Value {
        json!({
            "subsystem": "ai_node",
            "node_name": self.node_name.as_str(),
            "ok": false,
            "state": state,
            "schema_version": schema_version,
            "config_version": config_version,
            "error": {
                "code": code,
                "message": message
            },
            "effective_config": Value::Null
        })
    }

    fn node_name_matches(&self, requested: &str) -> bool {
        if requested == self.node_name {
            return true;
        }
        let with_hive_prefix = format!("{}@", self.node_name);
        requested.starts_with(&with_hive_prefix)
    }

    async fn build_config_get_response(&self) -> Value {
        let state = self.control_plane.read().await;
        let cognitive_definition = self.cognitive_definition.read().await.clone();
        let definition_state = cognitive_definition.definition_state.clone();
        let active_prompt_chars = cognitive_definition.active_prompt_chars;
        let (ok, config_source) = if state.effective_config.is_some() {
            (true, state.config_source)
        } else {
            (false, "none")
        };
        // Model D' — the only source is vault. Reports `vault` when the
        // node has both `self_ilk_id` and `self_tenant_id` available
        // (orchestrator-spawned with FLUXBEE_NODE_ILK_ID +
        // FLUXBEE_NODE_TENANT_ID); reports `missing` otherwise. Live vault
        // presence is verified on each chat resolve call.
        let api_key_source = if self
            .self_ilk_id
            .as_deref()
            .filter(|v| !v.is_empty())
            .is_some()
            && self
                .self_tenant_id
                .as_deref()
                .filter(|v| !v.is_empty())
                .is_some()
        {
            OpenAiApiKeySource::Vault
        } else {
            OpenAiApiKeySource::Missing
        };
        let error = if ok {
            Value::Null
        } else {
            json!({"code":"node_not_configured","message":"No effective config available"})
        };
        json!({
            "subsystem": "ai_node",
            "node_name": self.node_name.as_str(),
            "ok": ok,
            "state": state.current_state.as_str(),
            "config_source": config_source,
            "api_key_source": api_key_source.as_str(),
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "contract": {
                "node_family": "AI",
                "node_kind": AI_RUNTIME_KIND,
                "supports": ["CONFIG_GET", "CONFIG_SET"],
                "required_fields": [
                    "config.behavior.kind",
                    "config.behavior.model"
                ],
                "field_values": {
                    "config.behavior.kind": {
                        "allowed": ["echo", "openai_chat"],
                        "notes": ["Use openai_chat for OpenAI-backed chat. Do not use openai."]
                    },
                    "config.behavior.model": {
                        "examples": ["gpt-4.1-mini"]
                    }
                },
                "optional_fields": [
                    "config.behavior.instructions",
                    "config.behavior.model_settings",
                    "config.behavior.base_url",
                    "config.behavior.capabilities.multimodal"
                ],
                "resources": [
                    {
                        "resource_type": "openai",
                        "required": true,
                        "source": api_key_source.as_str(),
                        "configured": api_key_source != OpenAiApiKeySource::Missing,
                        "provider": "SY.vault"
                    }
                ],
                "notes": [
                    "Cognitive assets are not part of CONFIG_SET. Apply role_hash, skill_hashes, handbook_hashes, and personality_hash with set_ilk_definition against the agent ILK.",
                    "OpenAI credentials are resolved only from SY.vault using resource_type=openai.",
                    "For OpenAI behavior set config.behavior.kind=openai_chat, not openai.",
                    "CONFIG_SET rejects secret-bearing fields such as config.secrets.openai.api_key, config.behavior.openai.api_key, config.behavior.api_key, and config.behavior.api_key_env.",
                    "ai.generic defaults behavior.capabilities.multimodal=true unless explicitly overridden.",
                    "Cognitive role/skill/handbook prompt definition is loaded from identity SHM and blob://agent-assets/<hash>.json."
                ]
            },
            "effective_config": state.effective_config.as_ref().map(redact_secrets),
            "runtime": AI_RUNTIME_KIND,
            "definition_state": definition_state,
            "definition": cognitive_definition,
            "active_prompt_chars": active_prompt_chars,
            "error": error,
        })
    }

    async fn build_ping_response(&self) -> Value {
        let state = self.control_plane.read().await;
        json!({
            "ok": true,
            "node_name": self.node_name.as_str(),
            "state": state.current_state.as_str(),
        })
    }

    async fn build_status_response(&self) -> Value {
        let state = self.control_plane.read().await;
        let behavior_kind = self
            .behavior
            .read()
            .await
            .as_ref()
            .map(NodeBehavior::kind)
            .unwrap_or("none");
        json!({
            "state": state.current_state.as_str(),
            "node_name": self.node_name.as_str(),
            "behavior_kind": behavior_kind,
            "config_source": state.config_source,
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": Value::Null
        })
    }

    async fn build_node_status_get_response(&self) -> Value {
        let health_state = std::env::var(NODE_STATUS_DEFAULT_HEALTH_STATE)
            .ok()
            .as_deref()
            .map(normalize_health_state)
            .unwrap_or("HEALTHY");
        json!({
            "status": "ok",
            "health_state": health_state
        })
    }
}

#[derive(Debug, Clone)]
struct LoadedCognitiveDefinition {
    role: Option<LoadedRoleAsset>,
    skills: Vec<LoadedSkillAsset>,
    handbooks: Vec<LoadedHandbookAsset>,
    personality: Option<LoadedPersonalityAsset>,
}

#[derive(Debug, Clone)]
struct LoadedRoleAsset {
    hash: String,
    name: String,
    description: String,
    tone: Option<String>,
    limits: Vec<String>,
}

#[derive(Debug, Clone)]
struct LoadedSkillAsset {
    hash: String,
    name: String,
    description: Option<String>,
    instructions: Vec<String>,
    examples: Vec<SkillExampleAsset>,
    constraints: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SkillExampleAsset {
    #[serde(default)]
    input: String,
    #[serde(default)]
    output: String,
}

#[derive(Debug, Clone)]
struct LoadedHandbookAsset {
    hash: String,
    name: String,
    sections: Vec<HandbookSectionAsset>,
}

#[derive(Debug, Clone)]
struct LoadedPersonalityAsset {
    hash: String,
    name: String,
    description: Option<String>,
    system_fields: PersonalitySystemFields,
    biographical: PersonalityBiographical,
    narrative: PersonalityNarrative,
    extensions: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PersonalitySystemFields {
    #[serde(default)]
    timezone: String,
    #[serde(default)]
    country_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region_code: Option<String>,
    #[serde(default)]
    primary_language: String,
    #[serde(default)]
    additional_languages: Vec<PersonalityLanguage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PersonalityLanguage {
    #[serde(default)]
    code: String,
    #[serde(default)]
    level: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PersonalityBiographical {
    #[serde(default)]
    nationality: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    birth_year: Option<u32>,
    #[serde(default)]
    birth_place: Option<String>,
    #[serde(default)]
    current_residence: Option<String>,
    #[serde(default)]
    education: Vec<PersonalityEducation>,
    #[serde(default)]
    professional_background: Vec<PersonalityProfessionalEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PersonalityEducation {
    #[serde(default)]
    institution: Option<String>,
    #[serde(default)]
    degree: Option<String>,
    #[serde(default)]
    year_completed: Option<u32>,
    #[serde(default)]
    field: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PersonalityProfessionalEntry {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    organization: Option<String>,
    #[serde(default)]
    years: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PersonalityNarrative {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    personality_traits: Vec<String>,
    #[serde(default)]
    communication_style: Option<String>,
    #[serde(default)]
    interests: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HandbookSectionAsset {
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    subsections: Vec<HandbookSectionAsset>,
}

#[derive(Debug, Deserialize)]
struct CognitiveAssetDocument {
    asset_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tone: Option<String>,
    #[serde(default)]
    limits: Vec<String>,
    #[serde(default)]
    instructions: Vec<String>,
    #[serde(default)]
    examples: Vec<SkillExampleAsset>,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    sections: Vec<HandbookSectionAsset>,
    #[serde(default)]
    system_fields: Option<PersonalitySystemFields>,
    #[serde(default)]
    biographical: Option<PersonalityBiographical>,
    #[serde(default)]
    narrative: Option<PersonalityNarrative>,
    #[serde(default)]
    extensions: Option<serde_json::Map<String, serde_json::Value>>,
}

async fn cognitive_definition_poll_loop(
    node_name: String,
    config_dir: PathBuf,
    config: CognitiveDefinitionRuntimeConfig,
    state: Arc<RwLock<CognitiveDefinitionRuntimeState>>,
) {
    if !config.enabled {
        return;
    }
    loop {
        if let Err(err) =
            refresh_cognitive_definition(&node_name, &config_dir, &config, &state).await
        {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "cognitive definition refresh failed"
            );
        }
        tokio::time::sleep(config.poll_interval).await;
    }
}

fn spawn_cognitive_definition_poll_if_enabled(
    node_name: String,
    config_dir: PathBuf,
    config: CognitiveDefinitionRuntimeConfig,
    state: Arc<RwLock<CognitiveDefinitionRuntimeState>>,
) {
    if !config.enabled {
        return;
    }
    tokio::spawn(cognitive_definition_poll_loop(
        node_name, config_dir, config, state,
    ));
}

async fn refresh_cognitive_definition(
    node_name: &str,
    config_dir: &PathBuf,
    config: &CognitiveDefinitionRuntimeConfig,
    state: &Arc<RwLock<CognitiveDefinitionRuntimeState>>,
) -> Result<(), String> {
    let resolved = match find_ilk_by_handler_node_from_hive_config(config_dir, node_name) {
        Ok(value) => value,
        Err(err) => {
            update_cognitive_unresolved(state, format!("identity SHM unavailable: {err}")).await;
            return Ok(());
        }
    };
    let Some((seq, ilk)) = resolved else {
        update_cognitive_unresolved(state, "agent ILK not found for handler_node".to_string())
            .await;
        return Ok(());
    };
    if !ilk.ilk_type.eq_ignore_ascii_case("agent") {
        update_cognitive_unresolved(state, format!("matched ILK is not agent: {}", ilk.ilk_type))
            .await;
        return Ok(());
    }

    let hashes = hashes_from_identity_ilk(&ilk);
    {
        let mut current = state.write().await;
        if should_reuse_cognitive_state(&current, &ilk.ilk_id, &hashes) {
            current.last_identity_seq = Some(seq);
            return Ok(());
        }
    }

    let next = compose_cognitive_state(&config.blob_root, seq, &ilk, hashes)?;
    tracing::info!(
        node_name = %node_name,
        ilk_id = %ilk.ilk_id,
        identity_seq = seq,
        definition_state = %next.definition_state,
        role_hash_loaded = ?next.role_hash_loaded,
        skill_hashes_loaded = ?next.skill_hashes_loaded,
        handbook_hashes_loaded = ?next.handbook_hashes_loaded,
        failed_hashes = ?next.failed_hashes,
        prompt_truncated = next.prompt_truncated,
        active_prompt_chars = next.active_prompt_chars,
        "cognitive definition refreshed"
    );
    *state.write().await = next;
    Ok(())
}

async fn update_cognitive_unresolved(
    state: &Arc<RwLock<CognitiveDefinitionRuntimeState>>,
    message: String,
) {
    let mut guard = state.write().await;
    guard.definition_state = "unresolved".to_string();
    guard.active_prompt = None;
    guard.active_prompt_chars = 0;
    guard.failed_hashes = vec![CognitiveAssetFailure {
        hash: String::new(),
        asset_type: "identity".to_string(),
        error: message,
    }];
}

fn hashes_from_identity_ilk(ilk: &IdentityIlkOption) -> CognitiveDefinitionHashes {
    CognitiveDefinitionHashes {
        role_hash: ilk.role_hash.clone(),
        skill_hashes: ilk.skill_hashes.clone(),
        handbook_hashes: ilk.handbook_hashes.clone(),
        personality_hash: ilk.personality_hash.clone(),
    }
}

fn compose_cognitive_state(
    blob_root: &std::path::Path,
    seq: u64,
    ilk: &IdentityIlkOption,
    hashes: CognitiveDefinitionHashes,
) -> Result<CognitiveDefinitionRuntimeState, String> {
    if hashes.is_empty() {
        return Ok(CognitiveDefinitionRuntimeState {
            enabled: true,
            definition_state: "empty".to_string(),
            ilk_id: Some(ilk.ilk_id.clone()),
            role_hash_loaded: None,
            skill_hashes_loaded: Vec::new(),
            handbook_hashes_loaded: Vec::new(),
            personality_hash_loaded: None,
            failed_hashes: Vec::new(),
            prompt_truncated: false,
            active_prompt_chars: DEFAULT_UNCONFIGURED_AGENT_PROMPT.chars().count(),
            last_recompose_at: Some(chrono::Utc::now().to_rfc3339()),
            last_identity_seq: Some(seq),
            last_hashes: hashes,
            active_prompt: Some(DEFAULT_UNCONFIGURED_AGENT_PROMPT.to_string()),
        });
    }

    let mut failures = Vec::new();
    let role = match hashes.role_hash.as_deref() {
        Some(hash) => match load_role_asset(blob_root, hash) {
            Ok(asset) => Some(asset),
            Err(err) => {
                failures.push(CognitiveAssetFailure {
                    hash: hash.to_string(),
                    asset_type: "role".to_string(),
                    error: err,
                });
                None
            }
        },
        None => None,
    };

    let mut skills = Vec::new();
    for hash in &hashes.skill_hashes {
        match load_skill_asset(blob_root, hash) {
            Ok(asset) => skills.push(asset),
            Err(err) => failures.push(CognitiveAssetFailure {
                hash: hash.clone(),
                asset_type: "skill".to_string(),
                error: err,
            }),
        }
    }

    let mut handbooks = Vec::new();
    for hash in &hashes.handbook_hashes {
        match load_handbook_asset(blob_root, hash) {
            Ok(asset) => handbooks.push(asset),
            Err(err) => failures.push(CognitiveAssetFailure {
                hash: hash.clone(),
                asset_type: "handbook".to_string(),
                error: err,
            }),
        }
    }

    let personality = match hashes.personality_hash.as_deref() {
        Some(hash) => match load_personality_asset(blob_root, hash) {
            Ok(asset) => Some(asset),
            Err(err) => {
                failures.push(CognitiveAssetFailure {
                    hash: hash.to_string(),
                    asset_type: "personality".to_string(),
                    error: err,
                });
                None
            }
        },
        None => None,
    };

    let loaded = LoadedCognitiveDefinition {
        role,
        skills,
        handbooks,
        personality,
    };
    let loaded_count = loaded.role.iter().count()
        + loaded.skills.len()
        + loaded.handbooks.len()
        + loaded.personality.iter().count();
    let (prompt, prompt_truncated) = if loaded_count == 0 {
        (DEFAULT_UNCONFIGURED_AGENT_PROMPT.to_string(), false)
    } else {
        compose_cognitive_prompt(&loaded)
    };
    let definition_state = if failures.is_empty() {
        "composed"
    } else if loaded_count > 0 {
        "partial"
    } else {
        "error"
    };

    Ok(CognitiveDefinitionRuntimeState {
        enabled: true,
        definition_state: definition_state.to_string(),
        ilk_id: Some(ilk.ilk_id.clone()),
        role_hash_loaded: loaded.role.as_ref().map(|asset| asset.hash.clone()),
        skill_hashes_loaded: loaded
            .skills
            .iter()
            .map(|asset| asset.hash.clone())
            .collect(),
        handbook_hashes_loaded: loaded
            .handbooks
            .iter()
            .map(|asset| asset.hash.clone())
            .collect(),
        personality_hash_loaded: loaded.personality.as_ref().map(|asset| asset.hash.clone()),
        failed_hashes: failures,
        prompt_truncated,
        active_prompt_chars: prompt.chars().count(),
        last_recompose_at: Some(chrono::Utc::now().to_rfc3339()),
        last_identity_seq: Some(seq),
        last_hashes: hashes,
        active_prompt: Some(prompt),
    })
}

fn load_role_asset(blob_root: &std::path::Path, hash: &str) -> Result<LoadedRoleAsset, String> {
    let doc = load_cognitive_asset(blob_root, hash, "role")?;
    let name = required_asset_string(&doc.name, "role.name")?;
    let description =
        required_asset_string(doc.description.as_deref().unwrap_or(""), "role.description")?;
    Ok(LoadedRoleAsset {
        hash: hash.to_string(),
        name,
        description,
        tone: doc.tone.filter(|value| !value.trim().is_empty()),
        limits: clean_string_vec(doc.limits),
    })
}

fn load_skill_asset(blob_root: &std::path::Path, hash: &str) -> Result<LoadedSkillAsset, String> {
    let doc = load_cognitive_asset(blob_root, hash, "skill")?;
    let name = required_asset_string(&doc.name, "skill.name")?;
    let instructions = clean_string_vec(doc.instructions);
    if instructions.is_empty() {
        return Err("skill.instructions must contain at least one item".to_string());
    }
    Ok(LoadedSkillAsset {
        hash: hash.to_string(),
        name,
        description: doc.description.filter(|value| !value.trim().is_empty()),
        instructions,
        examples: doc
            .examples
            .into_iter()
            .filter(|item| !item.input.trim().is_empty() || !item.output.trim().is_empty())
            .collect(),
        constraints: clean_string_vec(doc.constraints),
    })
}

fn load_handbook_asset(
    blob_root: &std::path::Path,
    hash: &str,
) -> Result<LoadedHandbookAsset, String> {
    let doc = load_cognitive_asset(blob_root, hash, "handbook")?;
    let name = required_asset_string(&doc.name, "handbook.name")?;
    if doc.sections.is_empty() {
        return Err("handbook.sections must contain at least one item".to_string());
    }
    Ok(LoadedHandbookAsset {
        hash: hash.to_string(),
        name,
        sections: doc.sections,
    })
}

fn load_personality_asset(
    blob_root: &std::path::Path,
    hash: &str,
) -> Result<LoadedPersonalityAsset, String> {
    let doc = load_cognitive_asset(blob_root, hash, "personality")?;
    let name = required_asset_string(&doc.name, "personality.name")?;
    let system_fields = doc
        .system_fields
        .ok_or_else(|| "personality.system_fields is required".to_string())?;
    if system_fields.timezone.trim().is_empty() {
        return Err("personality.system_fields.timezone is required".to_string());
    }
    if system_fields.country_code.trim().is_empty() {
        return Err("personality.system_fields.country_code is required".to_string());
    }
    if system_fields.primary_language.trim().is_empty() {
        return Err("personality.system_fields.primary_language is required".to_string());
    }
    Ok(LoadedPersonalityAsset {
        hash: hash.to_string(),
        name,
        description: doc.description.filter(|value| !value.trim().is_empty()),
        system_fields,
        biographical: doc.biographical.unwrap_or_default(),
        narrative: doc.narrative.unwrap_or_default(),
        extensions: doc.extensions.unwrap_or_default(),
    })
}

fn load_cognitive_asset(
    blob_root: &std::path::Path,
    hash: &str,
    expected_type: &str,
) -> Result<CognitiveAssetDocument, String> {
    validate_hash64(hash)?;
    let path = blob_root.join("agent-assets").join(format!("{hash}.json"));
    let meta = fs::metadata(&path)
        .map_err(|err| format!("asset not readable '{}': {err}", path.display()))?;
    if meta.len() > AGENT_ASSET_MAX_BYTES {
        return Err(format!(
            "asset too large: {} bytes > {}",
            meta.len(),
            AGENT_ASSET_MAX_BYTES
        ));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|err| format!("asset read failed '{}': {err}", path.display()))?;
    let doc: CognitiveAssetDocument =
        serde_json::from_str(&raw).map_err(|err| format!("asset JSON invalid: {err}"))?;
    if doc.asset_type != expected_type {
        return Err(format!(
            "asset_type mismatch: expected '{expected_type}', got '{}'",
            doc.asset_type
        ));
    }
    Ok(doc)
}

fn validate_hash64(hash: &str) -> Result<(), String> {
    let trimmed = hash.trim();
    if trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("hash must be 64 hex chars".to_string())
    }
}

fn required_asset_string(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn clean_string_vec(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn append_personality_block(out: &mut String, personality: &LoadedPersonalityAsset) {
    out.push_str(&format!("[PERSONALITY: {}]\n", personality.name));
    out.push_str(&format!("Asset hash: {}\n", personality.hash));
    if let Some(description) = &personality.description {
        out.push_str(description);
        out.push('\n');
    }

    let sys = &personality.system_fields;
    out.push_str(&format!("Timezone: {}\n", sys.timezone));
    out.push_str(&format!("Country: {}\n", sys.country_code));
    if let Some(region) = sys.region_code.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("Region: {region}\n"));
    }
    out.push_str(&format!("Primary language: {}\n", sys.primary_language));
    if !sys.additional_languages.is_empty() {
        let langs: Vec<String> = sys
            .additional_languages
            .iter()
            .filter(|lang| !lang.code.trim().is_empty())
            .map(|lang| {
                if lang.level.trim().is_empty() {
                    lang.code.clone()
                } else {
                    format!("{} ({})", lang.code, lang.level)
                }
            })
            .collect();
        if !langs.is_empty() {
            out.push_str(&format!("Additional languages: {}\n", langs.join(", ")));
        }
    }

    let bio = &personality.biographical;
    if let Some(name) = bio.display_name.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("Display name: {name}\n"));
    }
    if let Some(nat) = bio.nationality.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("Nationality: {nat}\n"));
    }
    if let Some(year) = bio.birth_year {
        out.push_str(&format!("Birth year: {year}\n"));
    }
    if let Some(place) = bio.birth_place.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("Birth place: {place}\n"));
    }
    if let Some(residence) = bio
        .current_residence
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        out.push_str(&format!("Current residence: {residence}\n"));
    }
    if !bio.education.is_empty() {
        out.push_str("Education:\n");
        for entry in &bio.education {
            let parts: Vec<String> = [
                entry.degree.as_deref(),
                entry.field.as_deref(),
                entry.institution.as_deref(),
            ]
            .iter()
            .filter_map(|opt| opt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
            .collect();
            let year = entry
                .year_completed
                .map(|y| format!(" ({y})"))
                .unwrap_or_default();
            if parts.is_empty() && year.is_empty() {
                continue;
            }
            out.push_str(&format!("- {}{year}\n", parts.join(" — ")));
        }
    }
    if !bio.professional_background.is_empty() {
        out.push_str("Professional background:\n");
        for entry in &bio.professional_background {
            let role = entry.role.as_deref().unwrap_or("").trim();
            let org = entry.organization.as_deref().unwrap_or("").trim();
            let years = entry.years.as_deref().unwrap_or("").trim();
            if role.is_empty() && org.is_empty() && years.is_empty() {
                continue;
            }
            let mut line = String::from("- ");
            if !role.is_empty() {
                line.push_str(role);
            }
            if !org.is_empty() {
                if !role.is_empty() {
                    line.push_str(" — ");
                }
                line.push_str(org);
            }
            if !years.is_empty() {
                line.push_str(&format!(" ({years})"));
            }
            line.push('\n');
            out.push_str(&line);
        }
    }

    let nar = &personality.narrative;
    if let Some(summary) = nar.summary.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push('\n');
        out.push_str(summary);
        out.push('\n');
    }
    if let Some(style) = nar
        .communication_style
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        out.push_str(&format!("Communication style: {style}\n"));
    }
    if !nar.personality_traits.is_empty() {
        let traits: Vec<&str> = nar
            .personality_traits
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !traits.is_empty() {
            out.push_str(&format!("Traits: {}\n", traits.join(", ")));
        }
    }
    if !nar.interests.is_empty() {
        let interests: Vec<&str> = nar
            .interests
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !interests.is_empty() {
            out.push_str(&format!("Interests: {}\n", interests.join(", ")));
        }
    }

    if !personality.extensions.is_empty() {
        out.push_str("Additional traits:\n");
        for (key, value) in &personality.extensions {
            let rendered = match value {
                serde_json::Value::String(s) => s.clone(),
                _ => value.to_string(),
            };
            out.push_str(&format!("- {key}: {rendered}\n"));
        }
    }

    out.push('\n');
}

fn compose_cognitive_prompt(loaded: &LoadedCognitiveDefinition) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;

    if let Some(personality) = &loaded.personality {
        append_personality_block(&mut out, personality);
    }

    if let Some(role) = &loaded.role {
        out.push_str(&format!("[ROLE: {}]\n", role.name));
        out.push_str(&role.description);
        out.push('\n');
        out.push_str(&format!("\nAsset hash: {}\n", role.hash));
        if let Some(tone) = &role.tone {
            out.push_str(&format!("\nTone: {tone}\n"));
        }
        if !role.limits.is_empty() {
            out.push_str("\nLimits:\n");
            for limit in &role.limits {
                out.push_str(&format!("- {limit}\n"));
            }
        }
    }

    if !loaded.skills.is_empty() {
        out.push_str("\n--- SKILLS ---\n");
        for skill in &loaded.skills {
            out.push_str(&format!("\n[{}]\nAsset hash: {}\n", skill.name, skill.hash));
            if let Some(description) = &skill.description {
                out.push_str(&format!("Description: {description}\n"));
            }
            out.push_str("\nInstructions:\n");
            for (idx, instruction) in skill.instructions.iter().enumerate() {
                out.push_str(&format!("{}. {instruction}\n", idx + 1));
            }
            if !skill.constraints.is_empty() {
                out.push_str("\nConstraints:\n");
                for constraint in &skill.constraints {
                    out.push_str(&format!("- {constraint}\n"));
                }
            }
            if !skill.examples.is_empty() {
                append_budgeted(&mut out, "\nExamples:\n", &mut truncated);
                for example in &skill.examples {
                    append_budgeted(
                        &mut out,
                        &format!("Input: {}\nOutput: {}\n", example.input, example.output),
                        &mut truncated,
                    );
                }
            }
        }
    }

    if !loaded.handbooks.is_empty() {
        append_budgeted(&mut out, "\n--- REFERENCE MATERIAL ---\n", &mut truncated);
        for handbook in &loaded.handbooks {
            append_budgeted(
                &mut out,
                &format!("\n[{}]\nAsset hash: {}\n", handbook.name, handbook.hash),
                &mut truncated,
            );
            for section in &handbook.sections {
                append_handbook_section(&mut out, section, 2, &mut truncated);
            }
        }
    }

    if out.trim().is_empty() {
        return (DEFAULT_UNCONFIGURED_AGENT_PROMPT.to_string(), false);
    }
    if out.len() > COMPOSED_PROMPT_MAX_BYTES {
        truncate_to_byte_budget(&mut out, COMPOSED_PROMPT_MAX_BYTES);
        truncated = true;
    }
    (out, truncated)
}

fn append_handbook_section(
    out: &mut String,
    section: &HandbookSectionAsset,
    level: usize,
    truncated: &mut bool,
) {
    let title = section.title.trim();
    if !title.is_empty() {
        append_budgeted(
            out,
            &format!("\n{} {}\n", "#".repeat(level.clamp(2, 6)), title),
            truncated,
        );
    }
    if !section.content.trim().is_empty() {
        append_budgeted(out, &format!("{}\n", section.content.trim()), truncated);
    }
    for child in &section.subsections {
        append_handbook_section(out, child, level + 1, truncated);
    }
}

fn append_budgeted(out: &mut String, text: &str, truncated: &mut bool) {
    if out.len() >= COMPOSED_PROMPT_MAX_BYTES {
        *truncated = true;
        return;
    }
    let remaining = COMPOSED_PROMPT_MAX_BYTES - out.len();
    if text.len() <= remaining {
        out.push_str(text);
        return;
    }
    let marker = "\n[TRUNCATED]\n";
    let budget = remaining.saturating_sub(marker.len()).max(0);
    let mut end = budget;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&text[..end]);
    if remaining >= marker.len() {
        out.push_str(marker);
    }
    truncate_to_byte_budget(out, COMPOSED_PROMPT_MAX_BYTES);
    *truncated = true;
}

fn truncate_to_byte_budget(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn normalize_health_state(raw: &str) -> &'static str {
    match raw.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => "HEALTHY",
        "DEGRADED" => "DEGRADED",
        "ERROR" => "ERROR",
        "UNKNOWN" => "UNKNOWN",
        _ => "HEALTHY",
    }
}

fn extract_final_output_from_tool_results(
    result: &FunctionLoopRunResult,
) -> fluxbee_ai_sdk::Result<Option<AiBehaviorOutput>> {
    for item in result.items.iter().rev() {
        let FunctionLoopItem::ToolResult { result } = item else {
            continue;
        };
        if result.is_error {
            continue;
        }
        let Some(final_output_value) = result.output.get("final_output") else {
            continue;
        };
        let envelope: ToolFinalArtifactEnvelope =
            serde_json::from_value(final_output_value.clone()).map_err(|err| {
                fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                    "tool final_output envelope is invalid: {err}"
                ))
            })?;
        let mut artifacts = Vec::with_capacity(envelope.artifacts.len());
        for artifact in envelope.artifacts {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(artifact.bytes_base64)
                .map_err(|err| {
                    fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                        "tool final_output artifact bytes_base64 is invalid: {err}"
                    ))
                })?;
            artifacts.push(AiUserArtifact::new(bytes, artifact.mime, artifact.filename));
        }
        tracing::info!(
            artifact_count = artifacts.len(),
            artifact_filenames = ?artifacts
                .iter()
                .map(|artifact| artifact.filename.clone())
                .collect::<Vec<_>>(),
            artifact_mimes = ?artifacts
                .iter()
                .map(|artifact| artifact.mime.clone())
                .collect::<Vec<_>>(),
            "parsed explicit final_output artifact envelope from tool result"
        );
        return Ok(Some(AiBehaviorOutput::final_output(AiFinalOutput::new(
            envelope.text,
            artifacts,
        ))));
    }
    Ok(None)
}

fn summarize_behavior_output(output: &AiBehaviorOutput) -> String {
    match output {
        AiBehaviorOutput::Text(text) => text.clone(),
        AiBehaviorOutput::Final(final_output) => {
            if let Some(text) = final_output
                .text
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                return text.to_string();
            }
            if final_output.artifacts.is_empty() {
                return "Generated final output without text.".to_string();
            }
            let filenames = final_output
                .artifacts
                .iter()
                .map(|artifact| artifact.filename.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("Generated user-facing artifact(s): {filenames}")
        }
    }
}

fn csv_line(cells: &[String]) -> String {
    cells
        .iter()
        .map(|cell| {
            let escaped = cell.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn default_true() -> bool {
    true
}

fn build_tool_success_artifact_response(
    tool_name: &str,
    text: Option<String>,
    artifacts: Vec<AiUserArtifact>,
) -> fluxbee_ai_sdk::Result<Value> {
    tracing::info!(
        tool = %tool_name,
        artifact_count = artifacts.len(),
        artifact_filenames = ?artifacts
            .iter()
            .map(|artifact| artifact.filename.clone())
            .collect::<Vec<_>>(),
        artifact_mimes = ?artifacts
            .iter()
            .map(|artifact| artifact.mime.clone())
            .collect::<Vec<_>>(),
        artifact_total_bytes = artifacts.iter().map(|artifact| artifact.bytes.len()).sum::<usize>(),
        "tool produced final user-facing artifacts"
    );
    let envelope = ToolFinalArtifactEnvelope {
        text,
        artifacts: artifacts
            .into_iter()
            .map(|artifact| ToolFinalArtifactItem {
                filename: artifact.filename,
                mime: artifact.mime,
                bytes_base64: base64::engine::general_purpose::STANDARD.encode(artifact.bytes),
            })
            .collect(),
    };
    Ok(json!({
        "status": "ok",
        "final_output": envelope
    }))
}

fn invalid_tool_args_error(tool_name: &str, err: serde_json::Error) -> fluxbee_ai_sdk::AiSdkError {
    fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("{tool_name}: invalid arguments: {err}"))
}

fn invalid_tool_artifact_error(tool_name: &str, err: fluxbee_ai_sdk::AiSdkError) -> Value {
    let error_code = match &err {
        fluxbee_ai_sdk::errors::AiSdkError::ArtifactMimeNotAllowed { .. } => {
            "artifact_mime_not_allowed"
        }
        fluxbee_ai_sdk::errors::AiSdkError::ArtifactFilenameInvalid { .. } => {
            "artifact_filename_invalid"
        }
        _ => "artifact_generation_failed",
    };
    json!({
        "status": "error",
        "error_code": error_code,
        "message": format!("{tool_name}: {err}"),
        "retryable": false
    })
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn pdf_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn build_pdf_bytes(title: Option<&str>, lines: &[String]) -> fluxbee_ai_sdk::Result<Vec<u8>> {
    let mut content_lines = Vec::new();
    if let Some(title) = title.map(str::trim).filter(|value| !value.is_empty()) {
        content_lines.push(title.to_string());
        content_lines.push(String::new());
    }
    if lines.is_empty() && title.is_none() {
        content_lines.push("Documento generado por AI".to_string());
    } else {
        content_lines.extend(lines.iter().cloned());
    }

    let mut content = String::from("BT\n/F1 12 Tf\n50 742 Td\n14 TL\n");
    for line in content_lines {
        content.push_str(&format!("({}) Tj\nT*\n", pdf_escape(&line)));
    }
    content.push_str("ET\n");
    let content_bytes = content.into_bytes();

    let mut pdf = Vec::new();
    pdf.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    let objects = vec![
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_vec(),
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_vec(),
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n".to_vec(),
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_vec(),
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n",
            content_bytes.len()
        )
        .into_bytes(),
    ];

    for object in objects.into_iter().take(4) {
        offsets.push(pdf.len());
        pdf.extend_from_slice(&object);
    }
    offsets.push(pdf.len());
    pdf.extend_from_slice(
        format!("5 0 obj\n<< /Length {} >>\nstream\n", content_bytes.len()).as_bytes(),
    );
    pdf.extend_from_slice(&content_bytes);
    pdf.extend_from_slice(b"endstream\nendobj\n");

    let xref_offset = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            6, xref_offset
        )
        .as_bytes(),
    );
    Ok(pdf)
}

fn sanitize_sheet_name(raw: Option<&str>) -> String {
    let candidate = raw.unwrap_or("Sheet1").trim();
    let filtered = candidate
        .chars()
        .filter(|ch| !matches!(ch, ':' | '\\' | '/' | '?' | '*' | '[' | ']'))
        .collect::<String>();
    let final_name = if filtered.is_empty() {
        "Sheet1"
    } else {
        filtered.as_str()
    };
    final_name.chars().take(31).collect()
}

fn column_name(index: usize) -> String {
    let mut value = index + 1;
    let mut out = String::new();
    while value > 0 {
        let rem = (value - 1) % 26;
        out.insert(0, (b'A' + rem as u8) as char);
        value = (value - 1) / 26;
    }
    out
}

fn build_xlsx_bytes(args: &GenerateXlsxArtifactArgs) -> fluxbee_ai_sdk::Result<Vec<u8>> {
    let mut rows = Vec::new();
    if let Some(headers) = args.headers.as_ref().filter(|headers| !headers.is_empty()) {
        rows.push(headers.clone());
    }
    rows.extend(args.rows.clone());
    if rows.is_empty() {
        rows.push(vec!["Generated by AI".to_string()]);
    }
    let sheet_name = sanitize_sheet_name(args.sheet_name.as_deref());
    let mut sheet_xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>"#,
    );
    for (row_idx, row) in rows.iter().enumerate() {
        sheet_xml.push_str(&format!(r#"<row r="{}">"#, row_idx + 1));
        for (col_idx, cell) in row.iter().enumerate() {
            let cell_ref = format!("{}{}", column_name(col_idx), row_idx + 1);
            sheet_xml.push_str(&format!(
                r#"<c r="{cell_ref}" t="inlineStr"><is><t>{}</t></is></c>"#,
                xml_escape(cell)
            ));
        }
        sheet_xml.push_str("</row>");
    }
    sheet_xml.push_str("</sheetData></worksheet>");

    let cursor = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("[Content_Types].xml", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "xlsx: start content types failed: {err}"
            ))
        })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
  <Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>
</Types>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write content types failed: {err}")))?;

    zip.start_file("_rels/.rels", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: start rels failed: {err}"))
    })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>
</Relationships>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write rels failed: {err}")))?;

    zip.start_file("docProps/core.xml", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "xlsx: start core props failed: {err}"
            ))
        })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <dc:title>AI Generated Spreadsheet</dc:title>
  <dc:creator>Fluxbee AI</dc:creator>
</cp:coreProperties>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write core props failed: {err}")))?;

    zip.start_file("docProps/app.xml", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: start app props failed: {err}"))
    })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">
  <Application>Fluxbee AI</Application>
</Properties>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write app props failed: {err}")))?;

    zip.start_file("xl/workbook.xml", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: start workbook failed: {err}"))
    })?;
    zip.write_all(
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="{}" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#,
            xml_escape(&sheet_name)
        )
        .as_bytes(),
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write workbook failed: {err}")))?;

    zip.start_file("xl/_rels/workbook.xml.rels", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "xlsx: start workbook rels failed: {err}"
            ))
        })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write workbook rels failed: {err}")))?;

    zip.start_file("xl/styles.xml", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: start styles failed: {err}"))
    })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
  <fills count="1"><fill><patternFill patternType="none"/></fill></fills>
  <borders count="1"><border/></borders>
  <cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
  <cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs>
</styleSheet>"#,
    )
    .map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write styles failed: {err}"))
    })?;

    zip.start_file("xl/worksheets/sheet1.xml", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: start sheet failed: {err}"))
        })?;
    zip.write_all(sheet_xml.as_bytes()).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: write sheet failed: {err}"))
    })?;

    let cursor = zip.finish().map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("xlsx: finish zip failed: {err}"))
    })?;
    Ok(cursor.into_inner())
}

fn build_docx_bytes(args: &GenerateDocxArtifactArgs) -> fluxbee_ai_sdk::Result<Vec<u8>> {
    let mut body = String::new();
    if let Some(title) = args
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        body.push_str(&format!(
            r#"<w:p><w:r><w:t>{}</w:t></w:r></w:p>"#,
            xml_escape(title)
        ));
    }
    if args.paragraphs.is_empty()
        && args.bullets.is_empty()
        && args.table_headers.is_none()
        && args.table_rows.is_empty()
        && args.title.is_none()
    {
        body.push_str(r#"<w:p><w:r><w:t>Documento generado por AI</w:t></w:r></w:p>"#);
    }
    for paragraph in &args.paragraphs {
        body.push_str(&format!(
            r#"<w:p><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
            xml_escape(paragraph)
        ));
    }
    for bullet in &args.bullets {
        body.push_str(&format!(
            r#"<w:p><w:r><w:t>- {}</w:t></w:r></w:p>"#,
            xml_escape(bullet)
        ));
    }
    if args.table_headers.is_some() || !args.table_rows.is_empty() {
        body.push_str(r#"<w:tbl>"#);
        if let Some(headers) = args.table_headers.as_ref() {
            body.push_str("<w:tr>");
            for cell in headers {
                body.push_str(&format!(
                    r#"<w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc>"#,
                    xml_escape(cell)
                ));
            }
            body.push_str("</w:tr>");
        }
        for row in &args.table_rows {
            body.push_str("<w:tr>");
            for cell in row {
                body.push_str(&format!(
                    r#"<w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc>"#,
                    xml_escape(cell)
                ));
            }
            body.push_str("</w:tr>");
        }
        body.push_str("</w:tbl>");
    }

    let document_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" mc:Ignorable="w14 wp14">
  <w:body>{}<w:sectPr/></w:body>
</w:document>"#,
        body
    );

    let cursor = Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(cursor);
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("[Content_Types].xml", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "docx: start content types failed: {err}"
            ))
        })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
  <Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>
</Types>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: write content types failed: {err}")))?;

    zip.start_file("_rels/.rels", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: start rels failed: {err}"))
    })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>
</Relationships>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: write rels failed: {err}")))?;

    zip.start_file("docProps/core.xml", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "docx: start core props failed: {err}"
            ))
        })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:title>AI Generated Document</dc:title>
  <dc:creator>Fluxbee AI</dc:creator>
</cp:coreProperties>"#,
    )
    .map_err(|err| fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: write core props failed: {err}")))?;

    zip.start_file("docProps/app.xml", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: start app props failed: {err}"))
    })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">
  <Application>Fluxbee AI</Application>
</Properties>"#,
    )
    .map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: write app props failed: {err}"))
    })?;

    zip.start_file("word/document.xml", options)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "docx: start document failed: {err}"
            ))
        })?;
    zip.write_all(document_xml.as_bytes()).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: write document failed: {err}"))
    })?;

    zip.start_file("word/styles.xml", options).map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: start styles failed: {err}"))
    })?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#,
    )
    .map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: write styles failed: {err}"))
    })?;

    let cursor = zip.finish().map_err(|err| {
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!("docx: finish zip failed: {err}"))
    })?;
    Ok(cursor.into_inner())
}

fn color_from_seed(seed: &str) -> [u8; 3] {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    let value = hasher.finish();
    [
        48 + ((value & 0x7f) as u8),
        48 + (((value >> 8) & 0x7f) as u8),
        48 + (((value >> 16) & 0x7f) as u8),
    ]
}

fn build_raster_bytes(
    bands: &[String],
    width: Option<u32>,
    height: Option<u32>,
    format: image::ImageFormat,
) -> fluxbee_ai_sdk::Result<Vec<u8>> {
    let width = width.unwrap_or(1200).clamp(64, 2400);
    let height = height.unwrap_or(800).clamp(64, 2400);
    let mut image = image::RgbImage::from_pixel(width, height, image::Rgb([245, 245, 245]));
    let effective_bands = if bands.is_empty() {
        vec!["generated".to_string()]
    } else {
        bands.to_vec()
    };
    let band_height = (height / effective_bands.len() as u32).max(1);
    for (idx, band) in effective_bands.iter().enumerate() {
        let y_start = idx as u32 * band_height;
        let y_end = ((idx as u32 + 1) * band_height).min(height);
        let color = color_from_seed(band);
        for y in y_start..y_end {
            for x in 0..width {
                image.put_pixel(x, y, image::Rgb(color));
            }
        }
    }
    let mut cursor = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut cursor, format)
        .map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "raster artifact encoding failed: {err}"
            ))
        })?;
    Ok(cursor.into_inner())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn gov_identity_config_from_env() -> GovIdentityConfig {
    let mut cfg = GovIdentityConfig::default();
    if let Some(target) = env_nonempty(GOV_IDENTITY_TARGET_ENV) {
        cfg.target = target;
    }
    cfg.timeout = Duration::from_millis(env_u64(
        GOV_IDENTITY_TIMEOUT_MS_ENV,
        cfg.timeout.as_millis() as u64,
    ));
    cfg
}

#[async_trait]
impl FunctionTool for GenerateCsvArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_csv_artifact".to_string(),
            description: "Generate a CSV file as a final user-facing artifact from tabular rows."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "headers": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "rows": {
                        "type": "array",
                        "items": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "text": { "type": "string" }
                },
                "required": ["filename", "rows"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateCsvArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_csv_artifact", err))?;

        let filename = args.filename.trim();
        if filename.is_empty() {
            return Ok(json!({
                "status": "error",
                "error_code": "invalid_filename",
                "message": "filename is required",
                "retryable": false
            }));
        }

        let mut lines = Vec::new();
        if let Some(headers) = args.headers.as_ref().filter(|headers| !headers.is_empty()) {
            lines.push(csv_line(headers));
        }
        for row in &args.rows {
            lines.push(csv_line(row));
        }
        let csv_content = if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        };
        tracing::info!(
            filename = %filename,
            headers = args.headers.as_ref().map(|headers| headers.len()).unwrap_or(0),
            rows = args.rows.len(),
            csv_bytes = csv_content.len(),
            "generate_csv_artifact produced CSV final artifact"
        );
        let artifact =
            match AiUserArtifact::from_bytes(filename, "text/csv", csv_content.into_bytes()) {
                Ok(artifact) => artifact,
                Err(err) => return Ok(invalid_tool_artifact_error("generate_csv_artifact", err)),
            };
        build_tool_success_artifact_response(
            "generate_csv_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el archivo solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateTextArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_text_artifact".to_string(),
            description: "Generate a plain text file as a final user-facing artifact.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "content": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["filename", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateTextArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_text_artifact", err))?;
        let filename = args.filename.trim();
        let artifact = match AiUserArtifact::from_text(filename, args.content) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_text_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_text_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el archivo solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateJsonArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_json_artifact".to_string(),
            description: "Generate a JSON file as a final user-facing artifact.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "data": {},
                    "pretty": { "type": "boolean" },
                    "text": { "type": "string" }
                },
                "required": ["filename", "data"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateJsonArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_json_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = if args.pretty {
            serde_json::to_vec_pretty(&args.data)?
        } else {
            serde_json::to_vec(&args.data)?
        };
        let artifact = match AiUserArtifact::from_bytes(filename, "application/json", bytes) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_json_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_json_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el archivo solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateMarkdownArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_markdown_artifact".to_string(),
            description: "Generate a Markdown file as a final user-facing artifact.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "content": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["filename", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateMarkdownArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_markdown_artifact", err))?;
        let filename = args.filename.trim();
        let artifact = match AiUserArtifact::from_markdown(filename, args.content) {
            Ok(artifact) => artifact,
            Err(err) => {
                return Ok(invalid_tool_artifact_error(
                    "generate_markdown_artifact",
                    err,
                ))
            }
        };
        build_tool_success_artifact_response(
            "generate_markdown_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el archivo solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateHtmlArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_html_artifact".to_string(),
            description: "Generate an HTML file as a final user-facing artifact.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "content": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["filename", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateHtmlArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_html_artifact", err))?;
        let filename = args.filename.trim();
        let artifact = match AiUserArtifact::from_html(filename, args.content) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_html_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_html_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el archivo solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GeneratePdfArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_pdf_artifact".to_string(),
            description: "Generate a PDF document as a final user-facing artifact.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "title": { "type": "string" },
                    "lines": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "text": { "type": "string" }
                },
                "required": ["filename"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GeneratePdfArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_pdf_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = build_pdf_bytes(args.title.as_deref(), &args.lines)?;
        let artifact = match AiUserArtifact::from_bytes(filename, "application/pdf", bytes) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_pdf_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_pdf_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el PDF solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateXlsxArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_xlsx_artifact".to_string(),
            description: "Generate an XLSX spreadsheet as a final user-facing artifact."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "headers": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "rows": {
                        "type": "array",
                        "items": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "sheet_name": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["filename", "rows"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateXlsxArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_xlsx_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = build_xlsx_bytes(&args)?;
        let artifact = match AiUserArtifact::from_bytes(
            filename,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            bytes,
        ) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_xlsx_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_xlsx_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta la planilla solicitada: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateDocxArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_docx_artifact".to_string(),
            description: "Generate a DOCX document as a final user-facing artifact.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "title": { "type": "string" },
                    "paragraphs": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "bullets": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "table_headers": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "table_rows": {
                        "type": "array",
                        "items": {
                            "type": "array",
                            "items": { "type": "string" }
                        }
                    },
                    "text": { "type": "string" }
                },
                "required": ["filename"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateDocxArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_docx_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = build_docx_bytes(&args)?;
        let artifact = match AiUserArtifact::from_bytes(
            filename,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            bytes,
        ) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_docx_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_docx_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta el documento solicitado: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GeneratePngArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_png_artifact".to_string(),
            description: "Generate a simple PNG raster artifact with colored bands.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "bands": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "width": { "type": "integer", "minimum": 64 },
                    "height": { "type": "integer", "minimum": 64 },
                    "text": { "type": "string" }
                },
                "required": ["filename"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateRasterArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_png_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = build_raster_bytes(
            &args.bands,
            args.width,
            args.height,
            image::ImageFormat::Png,
        )?;
        let artifact = match AiUserArtifact::from_bytes(filename, "image/png", bytes) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_png_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_png_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta la imagen solicitada: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for GenerateJpegArtifactTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "generate_jpeg_artifact".to_string(),
            description: "Generate a simple JPEG raster artifact with colored bands.".to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "bands": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "width": { "type": "integer", "minimum": 64 },
                    "height": { "type": "integer", "minimum": 64 },
                    "text": { "type": "string" }
                },
                "required": ["filename"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateRasterArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_jpeg_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = build_raster_bytes(
            &args.bands,
            args.width,
            args.height,
            image::ImageFormat::Jpeg,
        )?;
        let artifact = match AiUserArtifact::from_bytes(filename, "image/jpeg", bytes) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_jpeg_artifact", err)),
        };
        build_tool_success_artifact_response(
            "generate_jpeg_artifact",
            args.text
                .or_else(|| Some(format!("Aca esta la imagen solicitada: {}", filename))),
            vec![artifact],
        )
    }
}

#[async_trait]
impl FunctionTool for IlkRegisterTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "ilk_register".to_string(),
            description: "Register identity completion for a temporary ILK (gov mode only)."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "src_ilk": { "type": "string", "minLength": 1 },
                    "identity_candidate": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "minLength": 1 },
                            "email": { "type": "string", "minLength": 3 },
                            "phone": { "type": "string" },
                            "tenant_hint": { "type": "string" }
                        },
                        "required": ["name", "email"],
                        "additionalProperties": true
                    },
                    "tenant_id": { "type": "string" },
                    "thread_id": { "type": "string" }
                },
                "required": ["src_ilk", "identity_candidate"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: IlkRegisterArgs = serde_json::from_value(arguments).map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "ilk_register: invalid arguments: {err}"
            ))
        })?;

        let src_ilk_owned = self.scoped_src_ilk.clone().unwrap_or(args.src_ilk);
        let src_ilk = src_ilk_owned.trim();
        if src_ilk.is_empty() {
            return Ok(json!({
                "status": "error",
                "error_code": "missing_src_ilk",
                "message": "src_ilk is required",
                "retryable": false
            }));
        }

        if args.identity_candidate.name.trim().is_empty()
            || args.identity_candidate.email.trim().is_empty()
        {
            return Ok(json!({
                "status": "error",
                "error_code": "invalid_identity_candidate",
                "message": "identity_candidate.name and identity_candidate.email are required",
                "retryable": false
            }));
        }

        let explicit_tenant = args.tenant_id.as_deref().map(str::trim);
        let tenant_hint = args
            .identity_candidate
            .tenant_hint
            .as_deref()
            .map(str::trim);
        let cfg_tenant = self.default_tenant_id.as_deref().map(str::trim);
        let env_tenant = env_nonempty(GOV_IDENTITY_TENANT_ID_ENV);
        let mut tenant_source = tenant_resolution_source(explicit_tenant, tenant_hint, cfg_tenant);
        let mut resolved_tenant_id =
            resolve_tenant_id_for_register(explicit_tenant, tenant_hint, cfg_tenant);

        if resolved_tenant_id.is_none() {
            if let Some(tenant_name) = tenant_hint.filter(|value| !value.is_empty()) {
                tracing::info!(
                    op = "tenant_resolve",
                    src_ilk = %src_ilk,
                    target = %self.identity.target,
                    tenant_hint = %tenant_name,
                    "tenant_id missing; attempting TNT_CREATE from tenant_hint"
                );
                let create_payload = json!({
                    "name": tenant_name,
                    "status": "active"
                });
                tracing::info!(
                    op = "tenant_resolve",
                    target = %self.identity.target,
                    msg = %MSG_TNT_CREATE,
                    payload = %create_payload,
                    "sending TNT_CREATE to identity"
                );
                let create_result = if let Some(bridge) = &self.bridge {
                    bridge
                        .call_ok(&self.identity, MSG_TNT_CREATE, create_payload)
                        .await
                } else {
                    Err("identity bridge not initialized".to_string())
                };

                match create_result {
                    Ok(out) => {
                        let created_tenant_id = out
                            .payload
                            .get("tenant_id")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|value| looks_like_tenant_id(value))
                            .map(ToString::to_string);
                        tracing::info!(
                            op = "tenant_resolve",
                            trace_id = %out.trace_id,
                            effective_target = %out.effective_target,
                            response_payload = %out.payload,
                            "received TNT_CREATE response from identity"
                        );
                        if created_tenant_id.is_none() {
                            tracing::warn!(
                                op = "tenant_resolve",
                                target = %self.identity.target,
                                response_payload = %out.payload,
                                "TNT_CREATE response missing valid tenant_id"
                            );
                            return Ok(json!({
                                "status": "error",
                                "error_code": "invalid_tnt_create_response",
                                "message": "TNT_CREATE response did not include a valid tenant_id",
                                "retryable": false
                            }));
                        }
                        resolved_tenant_id = created_tenant_id;
                        tenant_source = "tnt_create";
                    }
                    Err(err) => {
                        tracing::warn!(
                            op = "tenant_resolve",
                            target = %self.identity.target,
                            error = %err,
                            "TNT_CREATE failed"
                        );
                        return Ok(identity_error_to_tool_payload(err));
                    }
                }
            }
        }

        let Some(tenant_id) = resolved_tenant_id else {
            tracing::warn!(
                op = "ilk_register",
                src_ilk = %src_ilk,
                target = %self.identity.target,
                explicit_tenant_id = ?explicit_tenant,
                tenant_hint = ?tenant_hint,
                effective_config_tenant_id = ?cfg_tenant,
                env_tenant_id = ?env_tenant,
                "missing tenant_id for ILK_REGISTER"
            );
            return Ok(json!({
                "status": "error",
                "error_code": "missing_tenant_id",
                "message": "tenant_id is required for ILK_REGISTER (set tenant_id as tnt:<uuid>, use identity_candidate.tenant_hint=tnt:<uuid>, or set GOV_IDENTITY_TENANT_ID)",
                "retryable": false
            }));
        };

        tracing::info!(
            op = "ilk_register",
            src_ilk = %src_ilk,
            tenant_id = %tenant_id,
            tenant_source = %tenant_source,
            target = %self.identity.target,
            has_fallback = self.identity.fallback_target.is_some(),
            "dispatching identity registration request"
        );

        let payload = json!({
            "ilk_id": src_ilk,
            "ilk_type": "human",
            "tenant_id": tenant_id,
            "identification": {
                "display_name": args.identity_candidate.name,
                "email": args.identity_candidate.email,
                "phone": args.identity_candidate.phone,
                "tenant_hint": args.identity_candidate.tenant_hint,
            }
        });
        tracing::info!(
            op = "ilk_register",
            target = %self.identity.target,
            msg = %MSG_ILK_REGISTER,
            payload = %payload,
            "sending ILK_REGISTER to identity"
        );
        let result = if let Some(bridge) = &self.bridge {
            bridge
                .call_ok(&self.identity, MSG_ILK_REGISTER, payload)
                .await
        } else {
            Err("identity bridge not initialized".to_string())
        };

        match result {
            Ok(out) => {
                tracing::info!(
                    op = "ilk_register",
                    trace_id = %out.trace_id,
                    effective_target = %out.effective_target,
                    response_payload = %out.payload,
                    "received ILK_REGISTER response from identity"
                );
                Ok(json!({
                    "status": "ok",
                    "registered": true,
                    "effective_target": out.effective_target,
                    "trace_id": out.trace_id,
                    "identity_payload": out.payload
                }))
            }
            Err(err) => {
                tracing::warn!(
                    op = "ilk_register",
                    target = %self.identity.target,
                    error = %err,
                    "ILK_REGISTER failed"
                );
                Ok(identity_error_to_tool_payload(err))
            }
        }
    }
}

fn identity_error_to_tool_payload(msg: String) -> Value {
    let upper = msg.to_ascii_uppercase();
    let (error_code, retryable) = if upper.contains("NOT_PRIMARY") {
        ("NOT_PRIMARY", true)
    } else if upper.contains("UNREACHABLE") || upper.contains("NODE_NOT_FOUND") {
        ("UNAVAILABLE", true)
    } else if upper.contains("TTL EXCEEDED") || upper.contains("TTL_EXCEEDED") {
        ("TTL_EXCEEDED", true)
    } else if upper.contains("TIMEOUT") {
        ("TIMEOUT", true)
    } else if upper.contains("INVALID_") {
        ("INVALID_REQUEST", false)
    } else if upper.contains("UNAUTHORIZED_REGISTRAR") {
        ("UNAUTHORIZED_REGISTRAR", false)
    } else {
        ("IDENTITY_ERROR", true)
    };

    json!({
        "status": "error",
        "error_code": error_code,
        "message": msg,
        "retryable": retryable
    })
}

impl NodeBehavior {
    fn kind(&self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::OpenAiChat(_) => "openai_chat",
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = parse_runner_args()?;
    let config_paths = args.config_paths;
    let mut loaded = Vec::with_capacity(config_paths.len());
    for path in &config_paths {
        let raw = fs::read_to_string(path)?;
        let cfg: RunnerConfig = serde_yaml::from_str(&raw)?;
        loaded.push((path.clone(), cfg));
    }

    ensure_unique_node_names(&loaded)?;

    if loaded.is_empty() {
        let bootstrap_node = bootstrap_node_from_args(&args.bootstrap)?;
        tracing::info!(
            node_name = %bootstrap_node.name,
            "starting ai_node_runner without YAML config (UNCONFIGURED mode)"
        );
        run_unconfigured_bootstrap(bootstrap_node).await?;
        return Ok(());
    }

    let mut runners = JoinSet::new();
    for (config_path, cfg) in loaded {
        runners.spawn(async move { run_one_config(config_path, cfg).await });
    }

    while let Some(result) = runners.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(err),
            Err(err) => return Err(format!("runner task join error: {err}").into()),
        }
    }
    Ok(())
}

fn with_jitter(base: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_factor_percent = nanos % 25;
    let jitter = base
        .as_millis()
        .saturating_mul(jitter_factor_percent as u128)
        / 100;
    let total = base.as_millis().saturating_add(jitter);
    Duration::from_millis(total as u64)
}

async fn run_one_config(
    config_path: PathBuf,
    cfg: RunnerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let startup_effective_doc = build_startup_effective_config_doc(&cfg);
    let startup_effective_doc =
        materialize_effective_defaults(&cfg.node.name, startup_effective_doc);
    let startup_effective_config = serde_json::to_value(&startup_effective_doc)?;
    let persisted_dynamic =
        load_persisted_dynamic_config(&PathBuf::from(&cfg.node.dynamic_config_dir), &cfg.node.name);
    let behavior = build_behavior(&cfg)?;
    let node_config_dir = cfg.node.config_dir.clone();
    let runner_node_name = cfg.node.name.clone();
    let runner_router_socket = PathBuf::from(cfg.node.router_socket);
    let runner_uuid_persistence_dir = PathBuf::from(cfg.node.uuid_persistence_dir);
    let runner_version = cfg.node.version.clone();
    let runner_node_config = NodeConfig {
        name: runner_node_name.clone(),
        router_socket: runner_router_socket.clone(),
        uuid_persistence_dir: runner_uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: PathBuf::from(node_config_dir.clone()),
        version: runner_version.clone(),
    };

    let runtime_config = RuntimeConfig {
        read_timeout: Duration::from_millis(cfg.runtime.read_timeout_ms),
        handler_timeout: Duration::from_millis(cfg.runtime.handler_timeout_ms),
        write_timeout: Duration::from_millis(cfg.runtime.write_timeout_ms),
        queue_capacity: cfg.runtime.queue_capacity,
        worker_pool_size: cfg.runtime.worker_pool_size,
        retry_policy: RetryPolicy {
            max_attempts: cfg.runtime.retry_max_attempts,
            initial_backoff: Duration::from_millis(cfg.runtime.retry_initial_backoff_ms),
            max_backoff: Duration::from_millis(cfg.runtime.retry_max_backoff_ms),
        },
        metrics_log_interval: Duration::from_millis(cfg.runtime.metrics_log_interval_ms),
    };

    tracing::info!(
        config = %config_path.display(),
        node_name = %runner_node_name,
        "starting ai_node_runner node instance"
    );

    let node_name = runner_node_name.clone();
    let gov_identity = gov_identity_config_from_env();
    let thread_state_store =
        init_thread_state_store(&node_name, &PathBuf::from(&cfg.node.dynamic_config_dir)).await;
    let immediate_memory_store =
        init_immediate_memory_store(&node_name, &PathBuf::from(&cfg.node.dynamic_config_dir)).await;
    let cognitive_definition_config =
        CognitiveDefinitionRuntimeConfig::from(cfg.runtime.cognitive_definition.clone());
    let cognitive_definition = Arc::new(RwLock::new(CognitiveDefinitionRuntimeState::unresolved(
        cognitive_definition_config.enabled,
    )));
    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    match (&self_ilk_id, &self_tenant_id) {
        (Some(ilk), Some(tenant)) => {
            tracing::info!(
                node_name = %node_name,
                self_ilk_id = %ilk,
                self_tenant_id = %tenant,
                "AI node self identity loaded from FLUXBEE_NODE_ILK_ID / FLUXBEE_NODE_TENANT_ID"
            );
        }
        _ => {
            tracing::warn!(
                node_name = %node_name,
                has_ilk = self_ilk_id.is_some(),
                has_tenant = self_tenant_id.is_some(),
                "AI node missing FLUXBEE_NODE_ILK_ID and/or FLUXBEE_NODE_TENANT_ID; \
                 outgoing identity-bearing calls (vault, identity) will fail until \
                 the orchestrator re-spawns this node with the env vars."
            );
        }
    }
    let profile = build_ai_generic_rpc_profile()
        .map_err(|err| format!("ai-generic rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(runner_node_config, Duration::from_secs(1), profile)
            .await?;
    let vault = vault_client_for(dispatcher.clone(), &node_name, self_ilk_id.as_deref());
    let node = GenericAiNode {
        mode: RunnerMode::Default,
        node_name,
        self_ilk_id,
        self_tenant_id,
        behavior: Arc::new(RwLock::new(Some(behavior))),
        config_dir: PathBuf::from(node_config_dir),
        dynamic_config_dir: PathBuf::from(cfg.node.dynamic_config_dir),
        router_socket: runner_router_socket,
        state_dir: runner_uuid_persistence_dir,
        thread_state_store,
        immediate_memory_store,
        gov_identity,
        vault,
        control_plane: Arc::new(RwLock::new(ControlPlaneState {
            current_state: NodeLifecycleState::Configured,
            config_source: "yaml",
            effective_config: Some(startup_effective_config),
            schema_version: persisted_dynamic
                .as_ref()
                .map(|v| v.schema_version)
                .unwrap_or(1),
            config_version: persisted_dynamic
                .as_ref()
                .map(|v| v.config_version)
                .unwrap_or(1),
            ..ControlPlaneState::default()
        })),
        cognitive_definition: cognitive_definition.clone(),
        cognitive_definition_config: cognitive_definition_config.clone(),
    };
    spawn_cognitive_definition_poll_if_enabled(
        node.node_name.clone(),
        node.config_dir.clone(),
        cognitive_definition_config,
        cognitive_definition,
    );
    let runtime = NodeRuntime::new(dispatcher, node);
    runtime.run_with_config(runtime_config).await?;
    Ok(())
}

fn build_ai_generic_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(fluxbee_ai_sdk::AI_RUNTIME_CHANNEL)
        .post_pending_rule(
            RouteMatch::Any,
            RouteTarget::Command(fluxbee_ai_sdk::AI_RUNTIME_CHANNEL),
        )
        .build()
}

fn vault_client_for(
    dispatcher: Arc<RouterDispatcher>,
    node_name: &str,
    self_ilk_id: Option<&str>,
) -> Option<VaultClient> {
    let ilk = self_ilk_id.map(str::trim).filter(|v| !v.is_empty())?;
    let hive = node_name.split('@').nth(1).filter(|v| !v.is_empty())?;
    Some(VaultClient::new(
        dispatcher,
        hive.to_string(),
        VaultCallerOwned::new(ilk.to_string(), node_name.to_string()),
    ))
}

async fn run_unconfigured_bootstrap(
    node: NodeSection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let node_name = node.name.clone();
    let dynamic_dir = PathBuf::from(node.dynamic_config_dir.clone());
    let thread_state_store = init_thread_state_store(&node_name, &dynamic_dir).await;
    let immediate_memory_store = init_immediate_memory_store(&node_name, &dynamic_dir).await;
    let cognitive_definition_config =
        CognitiveDefinitionRuntimeConfig::from(CognitiveDefinitionSection::default());
    let cognitive_definition = Arc::new(RwLock::new(CognitiveDefinitionRuntimeState::unresolved(
        cognitive_definition_config.enabled,
    )));
    let persisted_dynamic = load_persisted_dynamic_config(&dynamic_dir, &node_name);
    let spawn_effective = if persisted_dynamic.is_none() {
        load_effective_config_from_spawn(&node_name)
    } else {
        None
    };
    let (behavior, state) = match persisted_dynamic.as_ref() {
        Some(stored) => {
            let materialized = materialize_effective_defaults(&node_name, stored.config.clone());
            match build_behavior_from_effective_config(&materialized) {
                Ok(behavior) => {
                    tracing::info!(
                        node_name = %node_name,
                        config_version = stored.config_version,
                        "loaded effective JSON config at bootstrap"
                    );
                    (
                        Some(behavior),
                        ControlPlaneState {
                            current_state: NodeLifecycleState::Configured,
                            config_source: "persisted",
                            effective_config: Some(
                                serde_json::to_value(materialized).unwrap_or(Value::Null),
                            ),
                            schema_version: stored.schema_version,
                            config_version: stored.config_version,
                        },
                    )
                }
                Err(err) => {
                    tracing::warn!(
                        node_name = %node_name,
                        error = %err,
                        "persisted JSON config is invalid; booting FAILED_CONFIG"
                    );
                    (
                        None,
                        ControlPlaneState {
                            current_state: NodeLifecycleState::FailedConfig,
                            config_source: "persisted",
                            effective_config: Some(
                                serde_json::to_value(materialized).unwrap_or(Value::Null),
                            ),
                            schema_version: stored.schema_version,
                            config_version: stored.config_version,
                        },
                    )
                }
            }
        }
        None => {
            if let Some(spawn_cfg) = spawn_effective {
                let spawn_config = spawn_cfg.config.clone();
                match build_behavior_from_effective_config(&spawn_config) {
                    Ok(behavior) => {
                        tracing::info!(
                            node_name = %node_name,
                            path = %spawn_cfg.path.display(),
                            "loaded spawn config at bootstrap"
                        );
                        if let Err(err) = persist_dynamic_config(
                            &dynamic_dir,
                            &node_name,
                            spawn_cfg.schema_version,
                            spawn_cfg.config_version,
                            &spawn_config,
                        ) {
                            tracing::warn!(
                                node_name = %node_name,
                                error = %err,
                                "failed to persist bootstrap config from spawn file"
                            );
                        }
                        (
                            Some(behavior),
                            ControlPlaneState {
                                current_state: NodeLifecycleState::Configured,
                                config_source: "spawn",
                                effective_config: Some(
                                    serde_json::to_value(spawn_config).unwrap_or(Value::Null),
                                ),
                                schema_version: spawn_cfg.schema_version,
                                config_version: spawn_cfg.config_version,
                            },
                        )
                    }
                    Err(err) => {
                        tracing::warn!(
                            node_name = %node_name,
                            path = %spawn_cfg.path.display(),
                            error = %err,
                            "spawn config exists but is invalid for AI effective config"
                        );
                        (
                            None,
                            ControlPlaneState {
                                current_state: NodeLifecycleState::FailedConfig,
                                config_source: "spawn",
                                effective_config: Some(
                                    serde_json::to_value(spawn_config).unwrap_or(Value::Null),
                                ),
                                schema_version: spawn_cfg.schema_version,
                                config_version: spawn_cfg.config_version,
                            },
                        )
                    }
                }
            } else {
                (
                    None,
                    ControlPlaneState {
                        current_state: NodeLifecycleState::Unconfigured,
                        config_source: "none",
                        effective_config: None,
                        schema_version: 0,
                        config_version: 0,
                    },
                )
            }
        }
    };

    let node_config_dir = node.config_dir.clone();
    let runner_node_name = node.name.clone();
    let runner_router_socket = PathBuf::from(node.router_socket);
    let runner_uuid_persistence_dir = PathBuf::from(node.uuid_persistence_dir);
    let runner_version = node.version.clone();
    let runner_node_config = NodeConfig {
        name: runner_node_name.clone(),
        router_socket: runner_router_socket.clone(),
        uuid_persistence_dir: runner_uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: PathBuf::from(node_config_dir.clone()),
        version: runner_version.clone(),
    };
    tracing::info!(
        node_name = %node_name,
        "starting ai_node_runner bootstrap instance"
    );
    let gov_identity = gov_identity_config_from_env();
    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    let profile = build_ai_generic_rpc_profile()
        .map_err(|err| format!("ai-generic rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(runner_node_config, Duration::from_secs(1), profile)
            .await?;
    let vault = vault_client_for(dispatcher.clone(), &node_name, self_ilk_id.as_deref());
    let ai_node = GenericAiNode {
        mode: RunnerMode::Default,
        node_name,
        self_ilk_id,
        self_tenant_id,
        behavior: Arc::new(RwLock::new(behavior)),
        config_dir: PathBuf::from(node_config_dir),
        dynamic_config_dir: dynamic_dir,
        router_socket: runner_router_socket,
        state_dir: runner_uuid_persistence_dir,
        thread_state_store,
        immediate_memory_store,
        gov_identity,
        vault,
        control_plane: Arc::new(RwLock::new(state)),
        cognitive_definition: cognitive_definition.clone(),
        cognitive_definition_config: cognitive_definition_config.clone(),
    };
    spawn_cognitive_definition_poll_if_enabled(
        ai_node.node_name.clone(),
        ai_node.config_dir.clone(),
        cognitive_definition_config,
        cognitive_definition,
    );
    let runtime = NodeRuntime::new(dispatcher, ai_node);
    runtime.run_with_config(RuntimeConfig::default()).await?;
    Ok(())
}

#[derive(Debug, Default)]
struct BootstrapArgs {
    node_name: Option<String>,
    version: Option<String>,
    router_socket: Option<String>,
    uuid_persistence_dir: Option<String>,
    config_dir: Option<String>,
    dynamic_config_dir: Option<String>,
}

#[derive(Debug, Default)]
struct RunnerArgs {
    config_paths: Vec<PathBuf>,
    bootstrap: BootstrapArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RunnerMode {
    #[default]
    Default,
    Gov,
}

impl RunnerMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Gov => "gov",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "default" => Some(Self::Default),
            "gov" => Some(Self::Gov),
            _ => None,
        }
    }
}

fn parse_runner_args() -> Result<RunnerArgs, Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut parsed = RunnerArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let Some(path) = args.get(i + 1) else {
                    return Err("missing path after --config".to_string().into());
                };
                parsed.config_paths.push(PathBuf::from(path));
                i += 2;
            }
            "--node-name" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --node-name".to_string().into());
                };
                parsed.bootstrap.node_name = Some(value.clone());
                i += 2;
            }
            "--version" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --version".to_string().into());
                };
                parsed.bootstrap.version = Some(value.clone());
                i += 2;
            }
            "--router-socket" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --router-socket".to_string().into());
                };
                parsed.bootstrap.router_socket = Some(value.clone());
                i += 2;
            }
            "--uuid-persistence-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --uuid-persistence-dir"
                        .to_string()
                        .into());
                };
                parsed.bootstrap.uuid_persistence_dir = Some(value.clone());
                i += 2;
            }
            "--config-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --config-dir".to_string().into());
                };
                parsed.bootstrap.config_dir = Some(value.clone());
                i += 2;
            }
            "--dynamic-config-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --dynamic-config-dir"
                        .to_string()
                        .into());
                };
                parsed.bootstrap.dynamic_config_dir = Some(value.clone());
                i += 2;
            }
            "--mode" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --mode".to_string().into());
                };
                let normalized = value.trim().to_ascii_lowercase();
                if normalized != "default" {
                    return Err(format!(
                        "--mode={value} is not supported in ai.common runtime (only default)"
                    )
                    .into());
                }
                i += 2;
            }
            other => {
                return Err(format!("unknown argument: {other}").into());
            }
        }
    }

    Ok(parsed)
}

fn bootstrap_node_from_args(
    args: &BootstrapArgs,
) -> Result<NodeSection, Box<dyn std::error::Error + Send + Sync>> {
    let name = args.node_name.clone().or_else(|| {
        let resolved = managed_node_name("", &["AI_NODE_NAME", "NODE_NAME"]);
        if resolved.trim().is_empty() {
            None
        } else {
            Some(resolved)
        }
    }).ok_or_else(|| {
        "when no --config is provided, pass --node-name (or FLUXBEE_NODE_NAME/AI_NODE_NAME env var)".to_string()
    })?;
    Ok(NodeSection {
        name,
        version: args
            .version
            .clone()
            .or_else(|| std::env::var("AI_NODE_VERSION").ok())
            .unwrap_or_else(default_version),
        router_socket: args
            .router_socket
            .clone()
            .or_else(|| std::env::var("AI_ROUTER_SOCKET").ok())
            .unwrap_or_else(default_router_socket),
        uuid_persistence_dir: args
            .uuid_persistence_dir
            .clone()
            .or_else(|| std::env::var("AI_UUID_PERSISTENCE_DIR").ok())
            .unwrap_or_else(default_state_dir),
        config_dir: args
            .config_dir
            .clone()
            .or_else(|| std::env::var("AI_CONFIG_DIR").ok())
            .unwrap_or_else(default_config_dir),
        dynamic_config_dir: args
            .dynamic_config_dir
            .clone()
            .or_else(|| std::env::var("AI_DYNAMIC_CONFIG_DIR").ok())
            .unwrap_or_else(default_dynamic_config_dir),
    })
}

fn ensure_unique_node_names(
    configs: &[(PathBuf, RunnerConfig)],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut names = HashSet::new();
    for (path, cfg) in configs {
        if !names.insert(cfg.node.name.clone()) {
            return Err(format!(
                "duplicate node name '{}' found in config {}",
                cfg.node.name,
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn build_behavior(
    cfg: &RunnerConfig,
) -> Result<NodeBehavior, Box<dyn std::error::Error + Send + Sync>> {
    let behavior = match &cfg.behavior {
        BehaviorSection::Echo => NodeBehavior::Echo,
        BehaviorSection::OpenaiChat(openai) => {
            let instructions = resolve_instructions(&openai.instructions)?;
            let model_settings = openai
                .model_settings
                .as_ref()
                .map(|v| ModelSettings {
                    temperature: v.temperature,
                    top_p: v.top_p,
                    max_output_tokens: v.max_output_tokens,
                })
                .unwrap_or_default();
            let multimodal = openai
                .capabilities
                .as_ref()
                .and_then(|caps| caps.multimodal)
                .unwrap_or_else(default_multimodal_for_runtime);
            NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                model: openai.model.clone(),
                instructions,
                model_settings,
                base_url: openai.base_url.clone(),
                immediate_memory: cfg.runtime.immediate_memory.clone(),
                multimodal,
            })
        }
    };
    Ok(behavior)
}

fn build_behavior_from_effective_config(
    config: &EffectiveConfigDocument,
) -> Result<NodeBehavior, Box<dyn std::error::Error + Send + Sync>> {
    let behavior = &config.behavior;
    let kind = behavior.kind.as_str();
    if kind.is_empty() {
        return Err("missing behavior.kind in effective config"
            .to_string()
            .into());
    }

    match kind {
        "echo" => Ok(NodeBehavior::Echo),
        "openai_chat" => {
            let model = behavior
                .model
                .clone()
                .or_else(|| behavior.params.as_ref().and_then(|p| p.model.clone()))
                .ok_or_else(|| "missing behavior.model for openai_chat".to_string())?
                .to_string();

            let instructions = extract_instructions_from_effective_config(behavior);
            let model_settings = extract_model_settings_from_effective_config(behavior);
            let base_url = behavior.base_url.clone();
            let immediate_memory = config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.immediate_memory.clone())
                .unwrap_or_default();
            let multimodal = behavior
                .capabilities
                .as_ref()
                .and_then(|caps| caps.multimodal)
                .unwrap_or_else(default_multimodal_for_runtime);

            Ok(NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                model,
                instructions,
                model_settings,
                base_url,
                immediate_memory,
                multimodal,
            }))
        }
        other => Err(format!("unsupported behavior.kind '{other}'").into()),
    }
}

fn resolve_instructions(
    cfg: &Option<InstructionsSourceConfig>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };

    match cfg {
        InstructionsSourceConfig::Inline(value) => Ok(Some(value.clone())),
        InstructionsSourceConfig::Strategy(strategy) => match strategy.source {
            InstructionsSourceKind::Inline => {
                let Some(value) = strategy.value.clone() else {
                    return Err("instructions.source=inline requires instructions.value".into());
                };
                Ok(Some(maybe_trim(value, strategy.trim)))
            }
            InstructionsSourceKind::File => {
                let Some(path) = strategy.value.clone() else {
                    return Err(
                        "instructions.source=file requires instructions.value (path)".into(),
                    );
                };
                let content = fs::read_to_string(path)?;
                Ok(Some(maybe_trim(content, strategy.trim)))
            }
            InstructionsSourceKind::Env => {
                let Some(env_name) = strategy.value.clone() else {
                    return Err(
                        "instructions.source=env requires instructions.value (env var)".into(),
                    );
                };
                let value = std::env::var(&env_name).map_err(|_| {
                    format!("missing env var for instructions source env: {}", env_name)
                })?;
                Ok(Some(maybe_trim(value, strategy.trim)))
            }
            InstructionsSourceKind::None => Ok(None),
        },
    }
}

fn maybe_trim(value: String, trim: bool) -> String {
    if trim {
        value.trim().to_string()
    } else {
        value
    }
}

fn build_startup_effective_config_doc(cfg: &RunnerConfig) -> EffectiveConfigDocument {
    let behavior = match &cfg.behavior {
        BehaviorSection::Echo => EffectiveBehaviorSection {
            kind: "echo".to_string(),
            ..EffectiveBehaviorSection::default()
        },
        BehaviorSection::OpenaiChat(openai) => EffectiveBehaviorSection {
            kind: "openai_chat".to_string(),
            model: Some(openai.model.clone()),
            instructions: Some(format_instructions_snapshot(&openai.instructions)),
            model_settings: openai.model_settings.clone(),
            base_url: openai.base_url.clone(),
            capabilities: Some(BehaviorCapabilities {
                multimodal: Some(
                    openai
                        .capabilities
                        .as_ref()
                        .and_then(|caps| caps.multimodal)
                        .unwrap_or_else(default_multimodal_for_runtime),
                ),
            }),
            ..EffectiveBehaviorSection::default()
        },
    };

    EffectiveConfigDocument {
        tenant_id: None,
        node: Some(EffectiveNodeSection {
            name: Some(cfg.node.name.clone()),
            version: Some(cfg.node.version.clone()),
            router_socket: Some(cfg.node.router_socket.clone()),
            uuid_persistence_dir: Some(cfg.node.uuid_persistence_dir.clone()),
            config_dir: Some(cfg.node.config_dir.clone()),
            dynamic_config_dir: Some(cfg.node.dynamic_config_dir.clone()),
        }),
        behavior,
        runtime: Some(EffectiveRuntimeSection {
            read_timeout_ms: Some(cfg.runtime.read_timeout_ms),
            handler_timeout_ms: Some(cfg.runtime.handler_timeout_ms),
            write_timeout_ms: Some(cfg.runtime.write_timeout_ms),
            queue_capacity: Some(cfg.runtime.queue_capacity),
            worker_pool_size: Some(cfg.runtime.worker_pool_size),
            retry_max_attempts: Some(cfg.runtime.retry_max_attempts),
            retry_initial_backoff_ms: Some(cfg.runtime.retry_initial_backoff_ms),
            retry_max_backoff_ms: Some(cfg.runtime.retry_max_backoff_ms),
            metrics_log_interval_ms: Some(cfg.runtime.metrics_log_interval_ms),
            immediate_memory: Some(cfg.runtime.immediate_memory.clone()),
            cognitive_definition: Some(cfg.runtime.cognitive_definition.clone()),
        }),
    }
}

fn dynamic_config_path(base_dir: &std::path::Path, node_name: &str) -> PathBuf {
    let safe_name = node_name.replace(['/', '\\'], "_");
    base_dir.join(format!("{safe_name}.json"))
}

fn load_persisted_dynamic_config(
    base_dir: &std::path::Path,
    node_name: &str,
) -> Option<EffectiveStateFile> {
    let path = dynamic_config_path(base_dir, node_name);
    let raw = fs::read_to_string(path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    if let Some(field) = root
        .get("config")
        .and_then(find_openai_secret_contract_field)
    {
        tracing::warn!(
            node_name = %node_name,
            field,
            "persisted AI config contains unsupported secret-bearing field; ignoring persisted config"
        );
        return None;
    }
    serde_json::from_str::<EffectiveStateFile>(&raw).ok()
}

fn persist_dynamic_config(
    base_dir: &std::path::Path,
    node_name: &str,
    schema_version: u32,
    config_version: u64,
    config: &EffectiveConfigDocument,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    fs::create_dir_all(base_dir)?;
    let path = dynamic_config_path(base_dir, node_name);
    let payload = EffectiveStateFile {
        schema_version,
        config_version,
        node_name: node_name.to_string(),
        config: config.clone(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&payload)?;
    write_json_atomic(&path, &json)?;
    Ok(())
}

fn write_json_atomic(
    path: &std::path::Path,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let parent = path
        .parent()
        .ok_or_else(|| "target path has no parent directory".to_string())?;
    fs::create_dir_all(parent)?;

    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("state"),
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let tmp_path = parent.join(tmp_name);

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    drop(file);

    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp_path, path)?;

    if let Ok(dir_file) = OpenOptions::new().read(true).open(parent) {
        let _ = dir_file.sync_all();
    }

    Ok(())
}

fn format_instructions_snapshot(cfg: &Option<InstructionsSourceConfig>) -> Value {
    match cfg {
        None => Value::Null,
        Some(InstructionsSourceConfig::Inline(value)) => {
            json!({ "source": "inline", "value": value, "trim": true })
        }
        Some(InstructionsSourceConfig::Strategy(strategy)) => json!({
            "source": match strategy.source {
                InstructionsSourceKind::Inline => "inline",
                InstructionsSourceKind::File => "file",
                InstructionsSourceKind::Env => "env",
                InstructionsSourceKind::None => "none"
            },
            "value": strategy.value,
            "trim": strategy.trim
        }),
    }
}

/// Model D' — extract the openai api_key from a vault `value`. Vault may
/// return either a bare string or an object with `api_key` field; both are
/// accepted (matches what SY consumers do).
fn extract_openai_api_key_from_value(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(s.to_string());
    }
    value
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn find_openai_secret_contract_field(config: &Value) -> Option<&'static str> {
    let behavior = config.get("behavior");
    if config.get("secrets").is_some() {
        return Some("config.secrets");
    }
    if behavior.and_then(|v| v.get("api_key")).is_some() {
        return Some("config.behavior.api_key");
    }
    if behavior.and_then(|v| v.get("api_key_env")).is_some() {
        return Some("config.behavior.api_key_env");
    }
    if behavior.and_then(|v| v.get("openai")).is_some() {
        return Some("config.behavior.openai");
    }
    None
}

fn find_unsupported_ai_config_field(config: &Value) -> Option<&'static str> {
    if config.get("assets").is_some() {
        return Some("config.assets");
    }
    None
}

fn parse_effective_config_doc(
    config: &Value,
) -> Result<EffectiveConfigDocument, Box<dyn std::error::Error + Send + Sync>> {
    if let Some(field) = find_openai_secret_contract_field(config) {
        return Err(format!(
            "secret-bearing field '{field}' is not accepted by ai.generic; use SY.vault resource_type=openai"
        )
        .into());
    }
    if let Some(field) = find_unsupported_ai_config_field(config) {
        return Err(format!(
            "unsupported field '{field}'; cognitive assets must be applied through set_ilk_definition"
        )
        .into());
    }
    Ok(serde_json::from_value::<EffectiveConfigDocument>(
        config.clone(),
    )?)
}

#[derive(Debug)]
struct SpawnEffectiveConfig {
    path: PathBuf,
    schema_version: u32,
    config_version: u64,
    config: EffectiveConfigDocument,
}

fn load_effective_config_from_spawn(node_name: &str) -> Option<SpawnEffectiveConfig> {
    let path = managed_node_config_path(node_name).ok()?;
    let raw = fs::read_to_string(&path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    let schema_version = root
        .get("_system")
        .and_then(|v| v.get("config_version"))
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(1);
    let config_version = root
        .get("_system")
        .and_then(|v| v.get("updated_at_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let mut candidate = root.get("config").cloned().unwrap_or(root);
    if let Some(obj) = candidate.as_object_mut() {
        obj.remove("_system");
    }
    let parsed = parse_effective_config_doc(&candidate).ok()?;
    let config = materialize_effective_defaults(node_name, parsed);
    Some(SpawnEffectiveConfig {
        path,
        schema_version,
        config_version,
        config,
    })
}

fn looks_like_tenant_id(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("tnt:") else {
        return false;
    };
    Uuid::parse_str(rest.trim()).is_ok()
}

fn resolve_tenant_id_for_register(
    explicit_tenant_id: Option<&str>,
    tenant_hint: Option<&str>,
    default_tenant_id: Option<&str>,
) -> Option<String> {
    let explicit = explicit_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| looks_like_tenant_id(v))
        .map(ToString::to_string);
    if explicit.is_some() {
        return explicit;
    }

    let hint = tenant_hint
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| looks_like_tenant_id(v))
        .map(ToString::to_string);
    if hint.is_some() {
        return hint;
    }

    let cfg_default = default_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| looks_like_tenant_id(v))
        .map(ToString::to_string);
    if cfg_default.is_some() {
        return cfg_default;
    }

    env_nonempty(GOV_IDENTITY_TENANT_ID_ENV).filter(|v| looks_like_tenant_id(v))
}

fn tenant_resolution_source(
    explicit_tenant_id: Option<&str>,
    tenant_hint: Option<&str>,
    default_tenant_id: Option<&str>,
) -> &'static str {
    let explicit_ok = explicit_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(looks_like_tenant_id);
    if explicit_ok {
        return "args.tenant_id";
    }

    let hint_ok = tenant_hint
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(looks_like_tenant_id);
    if hint_ok {
        return "identity_candidate.tenant_hint";
    }

    let cfg_ok = default_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(looks_like_tenant_id);
    if cfg_ok {
        return "effective_config.tenant_id";
    }

    let env_ok = env_nonempty(GOV_IDENTITY_TENANT_ID_ENV)
        .as_deref()
        .is_some_and(looks_like_tenant_id);
    if env_ok {
        return GOV_IDENTITY_TENANT_ID_ENV;
    }

    "missing"
}

fn materialize_effective_defaults(
    node_name: &str,
    mut config: EffectiveConfigDocument,
) -> EffectiveConfigDocument {
    if config.node.is_none() {
        config.node = Some(EffectiveNodeSection::default());
    }
    if let Some(node) = config.node.as_mut() {
        if node.name.is_none() {
            node.name = Some(node_name.to_string());
        }
    }
    if config.runtime.is_none() {
        config.runtime = Some(EffectiveRuntimeSection::default());
    }
    if let Some(runtime) = config.runtime.as_mut() {
        materialize_runtime_defaults(runtime);
    }
    if config.behavior.kind.eq_ignore_ascii_case("openai_chat") {
        if config.behavior.capabilities.is_none() {
            config.behavior.capabilities = Some(BehaviorCapabilities {
                multimodal: Some(default_multimodal_for_runtime()),
            });
        } else if let Some(caps) = config.behavior.capabilities.as_mut() {
            if caps.multimodal.is_none() {
                caps.multimodal = Some(default_multimodal_for_runtime());
            }
        }
        if config.behavior.provider.is_none() {
            config.behavior.provider = Some("openai".to_string());
        }
    }
    config
}

fn materialize_runtime_defaults(runtime: &mut EffectiveRuntimeSection) {
    let defaults = RuntimeSection::default();
    if runtime.read_timeout_ms.is_none() {
        runtime.read_timeout_ms = Some(defaults.read_timeout_ms);
    }
    if runtime.handler_timeout_ms.is_none() {
        runtime.handler_timeout_ms = Some(defaults.handler_timeout_ms);
    }
    if runtime.write_timeout_ms.is_none() {
        runtime.write_timeout_ms = Some(defaults.write_timeout_ms);
    }
    if runtime.queue_capacity.is_none() {
        runtime.queue_capacity = Some(defaults.queue_capacity);
    }
    if runtime.worker_pool_size.is_none() {
        runtime.worker_pool_size = Some(defaults.worker_pool_size);
    }
    if runtime.retry_max_attempts.is_none() {
        runtime.retry_max_attempts = Some(defaults.retry_max_attempts);
    }
    if runtime.retry_initial_backoff_ms.is_none() {
        runtime.retry_initial_backoff_ms = Some(defaults.retry_initial_backoff_ms);
    }
    if runtime.retry_max_backoff_ms.is_none() {
        runtime.retry_max_backoff_ms = Some(defaults.retry_max_backoff_ms);
    }
    if runtime.metrics_log_interval_ms.is_none() {
        runtime.metrics_log_interval_ms = Some(defaults.metrics_log_interval_ms);
    }
    if runtime.immediate_memory.is_none() {
        runtime.immediate_memory = Some(defaults.immediate_memory);
    }
    if runtime.cognitive_definition.is_none() {
        runtime.cognitive_definition = Some(defaults.cognitive_definition);
    }
}

fn is_control_plane(msg: &Message) -> bool {
    msg.meta.msg_type.eq_ignore_ascii_case("system")
        || msg.meta.msg_type.eq_ignore_ascii_case("admin")
}

fn build_control_plane_response(msg: &Message, response_msg: &str, payload: Value) -> Message {
    let mut response = build_reply_message_runtime_src(msg, payload);
    response.meta.msg = Some(response_msg.to_string());
    response
}

fn redact_secrets(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut output = serde_json::Map::new();
            for (k, v) in map {
                if k.eq_ignore_ascii_case("api_key") {
                    output.insert(k.clone(), Value::String("***REDACTED***".to_string()));
                } else {
                    output.insert(k.clone(), redact_secrets(v));
                }
            }
            Value::Object(output)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_secrets).collect()),
        _ => value.clone(),
    }
}

fn node_not_configured_payload(state: NodeLifecycleState) -> Value {
    json!({
        "type": "error",
        "code": "node_not_configured",
        "message": "AI node is not configured yet. Retry later.",
        "retryable": true,
        "details": {
            "state": state.as_str()
        }
    })
}

fn extract_instructions_from_effective_config(
    behavior: &EffectiveBehaviorSection,
) -> Option<String> {
    behavior
        .instructions
        .as_ref()
        .and_then(|v| {
            if let Some(inline) = v.as_str() {
                return Some(inline.to_string());
            }
            v.get("value")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            behavior
                .params
                .as_ref()
                .and_then(|p| p.system_prompt.clone())
        })
}

fn extract_model_settings_from_effective_config(
    behavior: &EffectiveBehaviorSection,
) -> ModelSettings {
    let direct = behavior.model_settings.as_ref();
    let params = behavior.params.as_ref();
    ModelSettings {
        temperature: direct
            .and_then(|v| v.temperature)
            .or_else(|| params.and_then(|v| v.temperature)),
        top_p: direct
            .and_then(|v| v.top_p)
            .or_else(|| params.and_then(|v| v.top_p)),
        max_output_tokens: direct
            .and_then(|v| v.max_output_tokens)
            .or_else(|| params.and_then(|v| v.max_output_tokens)),
    }
}

fn node_runtime_not_ready_payload() -> Value {
    json!({
        "type": "error",
        "code": "node_runtime_not_ready",
        "message": "AI node runtime is not ready to process user messages yet.",
        "retryable": true
    })
}

fn missing_openai_api_key_payload() -> Value {
    json!({
        "type": "error",
        "code": "missing_openai_api_key",
        "message": "Missing OpenAI API key in SY.vault resource_type=openai.",
        "retryable": true
    })
}

fn openai_runtime_error_payload(err: &fluxbee_ai_sdk::errors::AiSdkError) -> Value {
    match err {
        fluxbee_ai_sdk::errors::AiSdkError::Http(http_err)
            if http_err.is_timeout() || http_err.is_connect() =>
        {
            json!({
                "type": "error",
                "code": "provider_unreachable",
                "message": "The AI provider is temporarily unreachable. Please retry shortly.",
                "retryable": true
            })
        }
        fluxbee_ai_sdk::errors::AiSdkError::Timeout(_)
        | fluxbee_ai_sdk::errors::AiSdkError::RecoverableExhausted(_) => json!({
            "type": "error",
            "code": "provider_timeout",
            "message": "The AI provider did not respond in time. Please retry.",
            "retryable": true
        }),
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(msg) => {
            if let Some((status, detail)) = parse_openai_status_error(msg) {
                if status == 400 || status == 404 || status == 422 {
                    if extract_openai_error_param(&detail)
                        .as_deref()
                        .is_some_and(is_openai_attachment_param)
                    {
                        return json!({
                            "type": "error",
                            "code": "provider_attachment_invalid_request",
                            "message": "The AI provider rejected one or more attached files for the current model/provider.",
                            "retryable": false,
                            "provider_status": status,
                            "provider_detail": trim_chars(&detail, 280)
                        });
                    }
                }
                let (code, retryable, message) = match status {
                    400 | 404 | 422 => (
                        "provider_invalid_request",
                        false,
                        "The request is not valid for the AI provider.",
                    ),
                    401 | 403 => (
                        "provider_auth_error",
                        false,
                        "AI provider authentication failed. Check configured credentials.",
                    ),
                    408 => (
                        "provider_timeout",
                        true,
                        "The AI provider timed out while processing the request.",
                    ),
                    429 => (
                        "provider_rate_limited",
                        true,
                        "The AI provider is rate limiting requests. Retry shortly.",
                    ),
                    500..=599 => (
                        "provider_unavailable",
                        true,
                        "The AI provider is temporarily unavailable.",
                    ),
                    _ => (
                        "provider_error",
                        true,
                        "The AI provider returned an error while processing the request.",
                    ),
                };
                return json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                    "retryable": retryable,
                    "provider_status": status,
                    "provider_detail": trim_chars(&detail, 280)
                });
            }
            json!({
                "type": "error",
                "code": "provider_error",
                "message": "The AI provider returned an unexpected error.",
                "retryable": false
            })
        }
        other => json!({
            "type": "error",
            "code": "ai_runtime_error",
            "message": format!("AI runtime failure: {}", trim_chars(&other.to_string(), 220)),
            "retryable": other.is_recoverable()
        }),
    }
}

fn ai_final_output_error_payload(err: &fluxbee_ai_sdk::errors::AiSdkError) -> Value {
    match err {
        fluxbee_ai_sdk::errors::AiSdkError::Blob(blob_err) => json!({
            "type": "error",
            "code": "artifact_materialization_failed",
            "message": format!(
                "The AI generated a user-facing artifact, but it could not be materialized for delivery: {}",
                trim_chars(&blob_err.to_string(), 220)
            ),
            "retryable": err.is_recoverable()
        }),
        fluxbee_ai_sdk::errors::AiSdkError::Payload(_)
        | fluxbee_ai_sdk::errors::AiSdkError::Json(_)
        | fluxbee_ai_sdk::errors::AiSdkError::Protocol(_) => json!({
            "type": "error",
            "code": "artifact_generation_failed",
            "message": format!(
                "The AI could not build the final artifact response: {}",
                trim_chars(&err.to_string(), 220)
            ),
            "retryable": false
        }),
        other => json!({
            "type": "error",
            "code": "artifact_generation_failed",
            "message": format!(
                "The AI failed while preparing the final artifact response: {}",
                trim_chars(&other.to_string(), 220)
            ),
            "retryable": other.is_recoverable()
        }),
    }
}

fn parse_openai_status_error(message: &str) -> Option<(u16, String)> {
    let marker = "openai error status=";
    let idx = message.find(marker)?;
    let after = &message[idx + marker.len()..];
    let mut parts = after.splitn(2, ' ');
    let status = parts.next()?.trim().parse::<u16>().ok()?;
    let detail = after
        .split_once(" body=")
        .map(|(_, body)| body.trim().to_string())
        .unwrap_or_default();
    Some((status, detail))
}

#[derive(Debug, Default)]
struct AttachmentObservabilitySummary {
    count: usize,
    total_bytes: u64,
    mimes: Vec<String>,
}

fn attachment_summary_for_observability(
    resolved_user_input: Option<&ResolvedModelInput>,
) -> AttachmentObservabilitySummary {
    let Some(input) = resolved_user_input else {
        return AttachmentObservabilitySummary::default();
    };
    let count = input.attachments.len();
    let total_bytes = input
        .attachments
        .iter()
        .map(|attachment| attachment.blob_ref.size)
        .sum();
    let mimes = input
        .attachments
        .iter()
        .map(|attachment| attachment.blob_ref.mime.clone())
        .collect::<Vec<_>>();
    AttachmentObservabilitySummary {
        count,
        total_bytes,
        mimes,
    }
}

fn extract_openai_error_param(detail: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(detail).ok()?;
    parsed
        .get("error")
        .and_then(|error| error.get("param"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn is_openai_attachment_param(param: &str) -> bool {
    let param = param.trim();
    param.contains(".file_data")
        || param.contains(".file_id")
        || param.contains(".file_url")
        || param.contains(".image_url")
        || param.contains(".content")
}

fn infer_state_dir_from_dynamic(dynamic_config_dir: &std::path::Path) -> PathBuf {
    dynamic_config_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/var/lib/fluxbee/state"))
}

fn sanitize_storage_key(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "ai-node".to_string()
    } else {
        output
    }
}

async fn init_thread_state_store(
    node_name: &str,
    dynamic_config_dir: &std::path::Path,
) -> Option<Arc<dyn ThreadStateStore>> {
    let state_dir = infer_state_dir_from_dynamic(dynamic_config_dir);
    let store_root = LanceDbThreadStateStore::path_for_node(&state_dir, node_name);
    let store = LanceDbThreadStateStore::new(store_root);
    match store.ensure_ready().await {
        Ok(()) => {
            tracing::info!(
                node_name = %node_name,
                path = %store.root_dir().display(),
                "thread state store ready"
            );
            Some(Arc::new(store))
        }
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "thread state store unavailable; continuing in degraded mode"
            );
            None
        }
    }
}

async fn init_immediate_memory_store(
    node_name: &str,
    dynamic_config_dir: &std::path::Path,
) -> Option<Arc<ImmediateMemoryStore>> {
    let state_dir = infer_state_dir_from_dynamic(dynamic_config_dir);
    let store_root = ImmediateMemoryStore::path_for_node(&state_dir, node_name);
    let store = ImmediateMemoryStore::new(store_root);
    match store.ensure_ready().await {
        Ok(()) => {
            tracing::info!(
                node_name = %node_name,
                path = %store.root_dir().display(),
                "immediate memory store ready"
            );
            Some(Arc::new(store))
        }
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "immediate memory store unavailable; continuing without immediate persistence"
            );
            None
        }
    }
}

fn prune_recent_interactions(
    interactions: Vec<fluxbee_ai_sdk::ImmediateInteraction>,
    max_items: usize,
) -> Vec<fluxbee_ai_sdk::ImmediateInteraction> {
    if max_items == 0 {
        return Vec::new();
    }
    let len = interactions.len();
    let keep_from = len.saturating_sub(max_items);
    interactions.into_iter().skip(keep_from).collect()
}

fn trim_summary(
    mut summary: fluxbee_ai_sdk::ConversationSummary,
    max_chars: usize,
) -> fluxbee_ai_sdk::ConversationSummary {
    summary.goal = summary.goal.map(|v| trim_chars(&v, max_chars));
    summary.current_focus = summary.current_focus.map(|v| trim_chars(&v, max_chars));
    summary.decisions = summary
        .decisions
        .into_iter()
        .map(|v| trim_chars(&v, max_chars))
        .collect();
    summary.confirmed_facts = summary
        .confirmed_facts
        .into_iter()
        .map(|v| trim_chars(&v, max_chars))
        .collect();
    summary.open_questions = summary
        .open_questions
        .into_iter()
        .map(|v| trim_chars(&v, max_chars))
        .collect();
    summary
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
        out.push(ch);
    }
    out
}

fn invalid_payload_missing_thread_id() -> Value {
    json!({
        "type": "error",
        "code": "invalid_payload",
        "message": "Missing required thread_id for user message.",
        "retryable": false
    })
}

fn extract_thread_id(msg: &Message) -> Option<String> {
    msg.meta
        .thread_id
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn extract_src_ilk(msg: &Message) -> Option<String> {
    msg.meta
        .src_ilk
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn src_ilk_source(msg: &Message) -> &'static str {
    if msg
        .meta
        .src_ilk
        .as_deref()
        .map(str::trim)
        .is_some_and(|v| !v.is_empty())
    {
        return "meta";
    }
    "missing"
}

fn incoming_sender_hint(msg: &Message) -> Option<String> {
    let ctx = msg.meta.context.as_ref()?;
    let io = ctx.get("io")?;
    let sender = io.get("sender")?;
    let kind = sender.get("kind").and_then(Value::as_str).map(str::trim);
    let id = sender.get("id").and_then(Value::as_str).map(str::trim);
    match (kind, id) {
        (Some(k), Some(i)) if !k.is_empty() && !i.is_empty() => Some(format!("{k}:{i}")),
        (_, Some(i)) if !i.is_empty() => Some(i.to_string()),
        _ => None,
    }
}

fn text_preview(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut out = String::new();
    for ch in compact.chars().take(max_chars) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn render_memory_package_prompt_block(memory_package: Option<&MemoryPackage>) -> Option<String> {
    let package = memory_package?;
    let mut lines = Vec::new();
    lines.push("Conversation memory:".to_string());
    lines.push(format!("Thread: {}", package.thread_id));
    if let Some(context) = package.dominant_context.as_ref() {
        lines.push(format!("Dominant context: {}", context.label));
    }
    if let Some(reason) = package.dominant_reason.as_ref() {
        lines.push(format!("Dominant reason: {}", reason.label));
    }

    let context_labels = package
        .contexts
        .iter()
        .take(4)
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    if !context_labels.is_empty() {
        lines.push(format!("Contexts: {}", context_labels.join(", ")));
    }

    let reason_labels = package
        .reasons
        .iter()
        .take(4)
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    if !reason_labels.is_empty() {
        lines.push(format!("Reasons: {}", reason_labels.join(", ")));
    }

    let memory_labels = package
        .memories
        .iter()
        .take(3)
        .map(|item| item.summary.as_str())
        .collect::<Vec<_>>();
    if !memory_labels.is_empty() {
        lines.push(format!("Memories: {}", memory_labels.join(", ")));
    }

    if let Some(truncated) = package.truncated.as_ref() {
        let mut truncated_fields = Vec::new();
        if truncated.dropped_contexts > 0 {
            truncated_fields.push(format!("contexts={}", truncated.dropped_contexts));
        }
        if truncated.dropped_reasons > 0 {
            truncated_fields.push(format!("reasons={}", truncated.dropped_reasons));
        }
        if truncated.dropped_memories > 0 {
            truncated_fields.push(format!("memories={}", truncated.dropped_memories));
        }
        if truncated.dropped_episodes > 0 {
            truncated_fields.push(format!("episodes={}", truncated.dropped_episodes));
        }
        if !truncated_fields.is_empty() {
            lines.push(format!("Truncated fields: {}", truncated_fields.join(", ")));
        }
    }

    Some(lines.join("\n"))
}

fn inject_memory_package_into_text_input(input: &str, cognition_block: Option<&str>) -> String {
    let Some(block) = cognition_block.filter(|value| !value.trim().is_empty()) else {
        return input.to_string();
    };
    if input.trim().is_empty() {
        return block.to_string();
    }
    format!("{block}\n\nCurrent user message:\n{input}")
}

fn inject_memory_package_into_input_parts(
    mut parts: Vec<Value>,
    cognition_block: Option<&str>,
) -> Vec<Value> {
    let Some(block) = cognition_block.filter(|value| !value.trim().is_empty()) else {
        return parts;
    };
    let mut enriched = Vec::with_capacity(parts.len() + 1);
    enriched.push(json!({
        "type": "input_text",
        "text": block,
    }));
    enriched.append(&mut parts);
    enriched
}

#[allow(dead_code)]
fn require_src_ilk(ctx: &BehaviorContext) -> fluxbee_ai_sdk::Result<&str> {
    ctx.src_ilk
        .as_deref()
        .ok_or_else(|| fluxbee_ai_sdk::errors::AiSdkError::Protocol("missing_src_ilk".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_ai_sdk::{Destination, Meta, Routing};
    use fluxbee_sdk::protocol::{
        MemoryContextSummary, MemoryPackage, MemoryReasonSummary, MemorySummary,
    };
    use std::fs;
    use std::sync::Arc;
    use std::sync::{Mutex, OnceLock};
    use tokio::sync::RwLock;

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn sample_request() -> Message {
        Message {
            routing: Routing {
                src: "SY.orchestrator@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.frontdesk.gov@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-123".to_string(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some(MSG_NODE_STATUS_GET.to_string()),
                src_ilk: None,
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    #[test]
    fn control_plane_response_keeps_trace_id_and_replies_to_request_src() {
        let req = sample_request();
        let res = build_control_plane_response(
            &req,
            MSG_NODE_STATUS_GET_RESPONSE,
            json!({"status":"ok","health_state":"HEALTHY"}),
        );

        assert_eq!(res.routing.trace_id, req.routing.trace_id);
        assert!(matches!(
            res.routing.dst,
            Destination::Unicast(ref dst) if dst == &req.routing.src
        ));
    }

    #[test]
    fn control_plane_response_sets_expected_msg_name() {
        let req = sample_request();
        let res = build_control_plane_response(
            &req,
            MSG_NODE_STATUS_GET_RESPONSE,
            json!({"status":"ok","health_state":"HEALTHY"}),
        );
        assert_eq!(res.meta.msg.as_deref(), Some(MSG_NODE_STATUS_GET_RESPONSE));
    }

    fn test_node() -> GenericAiNode {
        let gov_identity = GovIdentityConfig::default();
        GenericAiNode {
            mode: RunnerMode::Default,
            node_name: "SY.frontdesk.gov".to_string(),
            self_ilk_id: None,
            self_tenant_id: None,
            behavior: Arc::new(RwLock::new(None)),
            config_dir: PathBuf::from("/tmp"),
            dynamic_config_dir: PathBuf::from("/tmp"),
            router_socket: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp"),
            thread_state_store: None,
            immediate_memory_store: None,
            gov_identity,
            vault: None,
            control_plane: Arc::new(RwLock::new(ControlPlaneState {
                current_state: NodeLifecycleState::Unconfigured,
                config_source: "none",
                effective_config: None,
                schema_version: 0,
                config_version: 0,
            })),
            cognitive_definition: Arc::new(
                RwLock::new(CognitiveDefinitionRuntimeState::disabled()),
            ),
            cognitive_definition_config: CognitiveDefinitionRuntimeConfig {
                enabled: false,
                poll_interval: Duration::from_secs(DEFAULT_COGNITIVE_POLL_INTERVAL_SECS),
                blob_root: PathBuf::from(DEFAULT_AGENT_ASSET_BLOB_ROOT),
            },
        }
    }

    fn cognitive_temp_root(test_name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fluxbee-ai-cognitive-tests-{}-{}",
            test_name,
            Uuid::new_v4()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("agent-assets")).expect("create agent-assets temp root");
        path
    }

    fn write_cognitive_asset(root: &std::path::Path, hash: &str, value: Value) {
        let path = root.join("agent-assets").join(format!("{hash}.json"));
        fs::write(
            path,
            serde_json::to_string_pretty(&value).expect("serialize asset"),
        )
        .expect("write cognitive asset");
    }

    fn sample_agent_ilk() -> IdentityIlkOption {
        IdentityIlkOption {
            ilk_id: "ilk:11111111-1111-4111-8111-111111111111".to_string(),
            tenant_id: "tnt:22222222-2222-4222-8222-222222222222".to_string(),
            display_name: Some("Support Agent".to_string()),
            handler_node: Some("AI.support@motherbee".to_string()),
            registration_status: "complete".to_string(),
            ilk_type: "agent".to_string(),
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
        }
    }

    #[test]
    fn cognitive_definition_empty_hashes_uses_default_unconfigured_prompt() {
        let root = cognitive_temp_root("empty-hashes");
        let ilk = sample_agent_ilk();
        let hashes = CognitiveDefinitionHashes::default();

        let state = compose_cognitive_state(&root, 7, &ilk, hashes).expect("compose state");

        assert_eq!(state.definition_state, "empty");
        assert_eq!(state.ilk_id.as_deref(), Some(ilk.ilk_id.as_str()));
        assert_eq!(state.last_identity_seq, Some(7));
        assert_eq!(
            state.active_prompt.as_deref(),
            Some(DEFAULT_UNCONFIGURED_AGENT_PROMPT)
        );
        assert_eq!(
            state.active_prompt_chars,
            DEFAULT_UNCONFIGURED_AGENT_PROMPT.chars().count()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_reuses_state_when_hashes_are_unchanged() {
        let root = cognitive_temp_root("reuse-unchanged-hashes");
        let role_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        write_cognitive_asset(
            &root,
            role_hash,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Answer as a Fluxbee support agent."
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);
        let mut state =
            compose_cognitive_state(&root, 8, &ilk, hashes.clone()).expect("compose state");

        assert!(should_reuse_cognitive_state(&state, &ilk.ilk_id, &hashes));

        state.last_identity_seq = Some(12);
        assert!(
            should_reuse_cognitive_state(&state, &ilk.ilk_id, &hashes),
            "identity SHM heartbeat seq changes must not force recomposition"
        );

        let mut changed_hashes = hashes.clone();
        changed_hashes.skill_hashes =
            vec!["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()];
        assert!(!should_reuse_cognitive_state(
            &state,
            &ilk.ilk_id,
            &changed_hashes
        ));

        state.failed_hashes.push(CognitiveAssetFailure {
            hash: role_hash.to_string(),
            asset_type: "role".to_string(),
            error: "temporary read failure".to_string(),
        });
        assert!(!should_reuse_cognitive_state(&state, &ilk.ilk_id, &hashes));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_composes_role_skill_and_handbook_assets() {
        let root = cognitive_temp_root("compose-assets");
        let role_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let skill_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let handbook_hash = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        write_cognitive_asset(
            &root,
            role_hash,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Answer as a Fluxbee support agent.",
                "tone": "direct",
                "limits": ["Do not invent platform capabilities."]
            }),
        );
        write_cognitive_asset(
            &root,
            skill_hash,
            json!({
                "asset_type": "skill",
                "name": "triage",
                "description": "Classify operator requests.",
                "instructions": ["Ask for missing node names before mutating state."],
                "constraints": ["No SCMD execution without operator confirmation."],
                "examples": [{"input": "list nodes", "output": "read-only inventory"}]
            }),
        );
        write_cognitive_asset(
            &root,
            handbook_hash,
            json!({
                "asset_type": "handbook",
                "name": "Fluxbee basics",
                "sections": [{
                    "title": "Routing",
                    "content": "Use workflows for business orchestration."
                }]
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        ilk.skill_hashes = vec![skill_hash.to_string()];
        ilk.handbook_hashes = vec![handbook_hash.to_string()];
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 8, &ilk, hashes).expect("compose state");
        let prompt = state.active_prompt.as_deref().expect("active prompt");

        assert_eq!(state.definition_state, "composed");
        assert_eq!(state.role_hash_loaded.as_deref(), Some(role_hash));
        assert_eq!(state.skill_hashes_loaded, vec![skill_hash.to_string()]);
        assert_eq!(
            state.handbook_hashes_loaded,
            vec![handbook_hash.to_string()]
        );
        assert!(state.failed_hashes.is_empty());
        assert!(prompt.contains("Answer as a Fluxbee support agent."));
        assert!(prompt.contains("Ask for missing node names before mutating state."));
        assert!(prompt.contains("Use workflows for business orchestration."));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_composes_personality_before_role() {
        let root = cognitive_temp_root("compose-personality");
        let role_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let personality_hash = "9999999999999999999999999999999999999999999999999999999999999999";
        write_cognitive_asset(
            &root,
            role_hash,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Receive and triage support tickets.",
            }),
        );
        write_cognitive_asset(
            &root,
            personality_hash,
            json!({
                "asset_type": "personality",
                "name": "Argentine engineer",
                "system_fields": {
                    "timezone": "America/Argentina/Mendoza",
                    "country_code": "AR",
                    "primary_language": "es-AR",
                    "additional_languages": [{ "code": "en", "level": "C1" }]
                },
                "biographical": {
                    "display_name": "Lucía",
                    "nationality": "Argentinian",
                    "birth_year": 1985
                },
                "narrative": {
                    "summary": "Engineer comfortable in formal and informal contexts.",
                    "communication_style": "Direct but friendly."
                }
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        ilk.personality_hash = Some(personality_hash.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 21, &ilk, hashes).expect("compose state");

        assert_eq!(state.definition_state, "composed");
        assert_eq!(
            state.personality_hash_loaded.as_deref(),
            Some(personality_hash)
        );
        let prompt = state.active_prompt.as_deref().expect("prompt");
        let personality_idx = prompt
            .find("[PERSONALITY:")
            .expect("personality block present");
        let role_idx = prompt.find("[ROLE:").expect("role block present");
        assert!(
            personality_idx < role_idx,
            "personality must render before role"
        );
        assert!(prompt.contains("Argentine engineer"));
        assert!(prompt.contains("America/Argentina/Mendoza"));
        assert!(prompt.contains("es-AR"));
        assert!(prompt.contains("Display name: Lucía"));
        assert!(prompt.contains("Direct but friendly."));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_personality_failure_yields_partial() {
        let root = cognitive_temp_root("personality-partial");
        let role_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let missing_personality =
            "5555555555555555555555555555555555555555555555555555555555555555";
        write_cognitive_asset(
            &root,
            role_hash,
            json!({
                "asset_type": "role",
                "name": "R",
                "description": "Answer support requests."
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        ilk.personality_hash = Some(missing_personality.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 22, &ilk, hashes).expect("compose state");

        assert_eq!(state.definition_state, "partial");
        assert_eq!(state.role_hash_loaded.as_deref(), Some(role_hash));
        assert!(state.personality_hash_loaded.is_none());
        let failure = state
            .failed_hashes
            .iter()
            .find(|f| f.asset_type == "personality")
            .expect("personality failure recorded");
        assert_eq!(failure.hash, missing_personality);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_personality_rejects_missing_required_system_fields() {
        let root = cognitive_temp_root("personality-bad");
        let personality_hash = "7777777777777777777777777777777777777777777777777777777777777777";
        write_cognitive_asset(
            &root,
            personality_hash,
            json!({
                "asset_type": "personality",
                "name": "Broken",
                "system_fields": { "timezone": "America/Buenos_Aires" }
                // missing country_code and primary_language
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.personality_hash = Some(personality_hash.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 23, &ilk, hashes).expect("compose state");

        assert_eq!(state.definition_state, "error");
        assert!(state.personality_hash_loaded.is_none());
        let failure = state
            .failed_hashes
            .iter()
            .find(|f| f.asset_type == "personality")
            .expect("personality failure");
        assert!(
            failure.error.contains("country_code") || failure.error.contains("primary_language"),
            "expected missing required system_fields, got: {}",
            failure.error
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_reports_partial_when_some_assets_fail() {
        let root = cognitive_temp_root("partial-assets");
        let role_hash = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let missing_skill_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        write_cognitive_asset(
            &root,
            role_hash,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Answer with the loaded role."
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        ilk.skill_hashes = vec![missing_skill_hash.to_string()];
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 9, &ilk, hashes).expect("compose state");

        assert_eq!(state.definition_state, "partial");
        assert_eq!(state.role_hash_loaded.as_deref(), Some(role_hash));
        assert_eq!(state.failed_hashes.len(), 1);
        assert_eq!(state.failed_hashes[0].asset_type, "skill");
        assert_eq!(state.failed_hashes[0].hash, missing_skill_hash);
        assert!(state
            .active_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("Answer with the loaded role.")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_reports_error_for_malformed_asset_schema() {
        let root = cognitive_temp_root("malformed-asset");
        let role_hash = "abababababababababababababababababababababababababababababababab";
        write_cognitive_asset(
            &root,
            role_hash,
            json!({
                "asset_type": "role",
                "name": "Broken role"
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 11, &ilk, hashes).expect("compose state");

        assert_eq!(state.definition_state, "error");
        assert!(state.role_hash_loaded.is_none());
        assert_eq!(state.failed_hashes.len(), 1);
        assert_eq!(state.failed_hashes[0].asset_type, "role");
        assert!(state.failed_hashes[0].error.contains("role.description"));
        assert_eq!(
            state.active_prompt.as_deref(),
            Some(DEFAULT_UNCONFIGURED_AGENT_PROMPT)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_rejects_invalid_hashes() {
        assert!(validate_hash64(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_ok());
        assert!(validate_hash64("not-a-hash").is_err());
        assert!(validate_hash64(
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
        )
        .is_err());
    }

    #[test]
    fn cognitive_definition_truncates_large_composed_prompt() {
        let root = cognitive_temp_root("truncate-prompt");
        let handbook_hash = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let large_content = "x".repeat(COMPOSED_PROMPT_MAX_BYTES * 2);
        write_cognitive_asset(
            &root,
            handbook_hash,
            json!({
                "asset_type": "handbook",
                "name": "Large handbook",
                "sections": [{
                    "title": "Large",
                    "content": large_content
                }]
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.handbook_hashes = vec![handbook_hash.to_string()];
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_cognitive_state(&root, 10, &ilk, hashes).expect("compose state");
        let prompt = state.active_prompt.as_deref().expect("active prompt");

        assert_eq!(state.definition_state, "composed");
        assert!(state.prompt_truncated);
        assert!(prompt.len() <= COMPOSED_PROMPT_MAX_BYTES);
        assert!(prompt.contains("[TRUNCATED]"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn node_status_get_respects_handler_enabled_env_false() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED, "false");
        std::env::remove_var(NODE_STATUS_DEFAULT_HEALTH_STATE);
        let node = test_node();
        let req = sample_request();
        let response = node
            .handle_control_plane(req)
            .await
            .expect("control-plane should not fail");
        assert!(response.is_none());
        std::env::remove_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED);
    }

    #[tokio::test]
    async fn node_status_get_uses_env_health_state_and_falls_back_to_healthy() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED, "true");
        let node = test_node();
        let req = sample_request();

        std::env::set_var(NODE_STATUS_DEFAULT_HEALTH_STATE, "DEGRADED");
        let degraded = node
            .handle_control_plane(req.clone())
            .await
            .expect("control-plane should not fail")
            .expect("status response should exist");
        assert_eq!(
            degraded.payload.get("health_state").and_then(Value::as_str),
            Some("DEGRADED")
        );

        std::env::set_var(NODE_STATUS_DEFAULT_HEALTH_STATE, "not-a-valid-state");
        let fallback = node
            .handle_control_plane(req)
            .await
            .expect("control-plane should not fail")
            .expect("status response should exist");
        assert_eq!(
            fallback.payload.get("health_state").and_then(Value::as_str),
            Some("HEALTHY")
        );

        std::env::remove_var(NODE_STATUS_DEFAULT_HEALTH_STATE);
        std::env::remove_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED);
    }

    #[tokio::test]
    async fn openai_chat_missing_api_key_returns_error_payload_instead_of_fatal() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::remove_var("OPENAI_API_KEY_MISSING_FOR_TEST");
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                model: "gpt-4.1-mini".to_string(),
                instructions: Some("Test instructions".to_string()),
                model_settings: ModelSettings::default(),
                base_url: None,
                immediate_memory: ImmediateMemorySection::default(),
                multimodal: true,
            }));
        }

        let mut msg = sample_user_request_with_context(
            json!({ "thread_id": "sim-thread-1" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        msg.meta.thread_id = Some("thread:sim-thread-1".to_string());
        let response = node
            .on_message(msg)
            .await
            .expect("on_message should not fail fatally")
            .expect("response should be present");

        assert_eq!(
            response.payload.get("code").and_then(Value::as_str),
            Some("missing_openai_api_key")
        );
        assert_eq!(
            response.payload.get("retryable").and_then(Value::as_bool),
            Some(true)
        );
        std::env::remove_var("OPENAI_API_KEY_MISSING_FOR_TEST");
    }

    fn sample_user_request_with_context(
        context: Value,
        top_level_src_ilk: Option<&str>,
    ) -> Message {
        Message {
            routing: Routing {
                src: "IO.sim.local@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.frontdesk.gov@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-user-123".to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: None,
                src_ilk: top_level_src_ilk.map(ToString::to_string),
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: Some(context),
                ..Meta::default()
            },
            payload: json!({"type":"text","content":"hola"}),
        }
    }

    fn sample_memory_package() -> MemoryPackage {
        MemoryPackage {
            package_version: 2,
            thread_id: "thread:canonical-1".to_string(),
            dominant_context: Some(MemoryContextSummary {
                context_id: "context:1".to_string(),
                label: "refund dispute".to_string(),
                weight: 3.0,
            }),
            dominant_reason: Some(MemoryReasonSummary {
                reason_id: "reason:1".to_string(),
                label: "confrontational pushback".to_string(),
                weight: 2.0,
            }),
            contexts: vec![MemoryContextSummary {
                context_id: "context:1".to_string(),
                label: "refund dispute".to_string(),
                weight: 3.0,
            }],
            reasons: vec![MemoryReasonSummary {
                reason_id: "reason:1".to_string(),
                label: "confrontational pushback".to_string(),
                weight: 2.0,
            }],
            memories: vec![MemorySummary {
                memory_id: "memory:1".to_string(),
                summary: "charged twice previously".to_string(),
                weight: 0.92,
                dominant_context_id: Some("context:1".to_string()),
                dominant_reason_id: Some("reason:1".to_string()),
            }],
            episodes: vec![],
            truncated: None,
        }
    }

    #[test]
    fn render_memory_package_prompt_block_returns_none_when_absent() {
        assert!(render_memory_package_prompt_block(None).is_none());
    }

    #[test]
    fn inject_memory_package_into_text_input_prefixes_cognition_block() {
        let block = render_memory_package_prompt_block(Some(&sample_memory_package()))
            .expect("memory block");
        let enriched =
            inject_memory_package_into_text_input("please help with this refund", Some(&block));
        assert!(enriched.contains("Conversation memory:"));
        assert!(enriched.contains("Dominant context: refund dispute"));
        assert!(enriched.contains("Current user message:\nplease help with this refund"));
    }

    #[test]
    fn inject_memory_package_into_input_parts_adds_leading_text_part() {
        let block = render_memory_package_prompt_block(Some(&sample_memory_package()))
            .expect("memory block");
        let enriched = inject_memory_package_into_input_parts(
            vec![json!({"type":"input_text","text":"hello"})],
            Some(&block),
        );
        assert_eq!(enriched.len(), 2);
        assert_eq!(
            enriched[0].get("text").and_then(Value::as_str),
            Some(block.as_str())
        );
        assert_eq!(
            enriched[1].get("text").and_then(Value::as_str),
            Some("hello")
        );
    }

    #[test]
    fn default_mode_registry_does_not_expose_gov_tools() {
        let node = test_node();
        let registry = node
            .build_tool_registry(&BehaviorContext {
                thread_id: None,
                src_ilk: None,
            })
            .expect("registry");
        let names = registry
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect::<Vec<_>>();
        assert!(!names.iter().any(|name| name == "ilk_register"));
        assert!(names.iter().any(|name| name == "generate_csv_artifact"));
        assert!(names.iter().any(|name| name == "generate_text_artifact"));
        assert!(names.iter().any(|name| name == "generate_json_artifact"));
        assert!(names
            .iter()
            .any(|name| name == "generate_markdown_artifact"));
        assert!(names.iter().any(|name| name == "generate_html_artifact"));
        assert!(names.iter().any(|name| name == "generate_pdf_artifact"));
        assert!(names.iter().any(|name| name == "generate_xlsx_artifact"));
        assert!(names.iter().any(|name| name == "generate_docx_artifact"));
        assert!(names.iter().any(|name| name == "generate_png_artifact"));
        assert!(names.iter().any(|name| name == "generate_jpeg_artifact"));
    }

    #[tokio::test]
    async fn default_mode_rejects_ilk_register_with_unknown_tool() {
        let node = test_node();
        let registry = node
            .build_tool_registry(&BehaviorContext {
                thread_id: Some("legacy-thread-1".to_string()),
                src_ilk: Some("ilk:11111111-1111-4111-8111-111111111111".to_string()),
            })
            .expect("registry");

        let results = fluxbee_ai_sdk::dispatch_tool_calls(
            &registry,
            vec![fluxbee_ai_sdk::FunctionToolCall {
                call_id: "call_1".to_string(),
                response_id: None,
                name: "ilk_register".to_string(),
                arguments: json!({
                    "src_ilk": "ilk:11111111-1111-4111-8111-111111111111",
                    "identity_candidate": {
                        "name": "Noelia Eguren",
                        "email": "neguren@4iplatform.com"
                    }
                }),
            }],
        )
        .await;

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert!(result.is_error);
        assert_eq!(result.name, "ilk_register");
        assert_eq!(result.output, Value::String("unknown_tool".to_string()));
    }

    #[test]
    fn extract_src_ilk_reads_from_meta_top_level_first() {
        let msg = sample_user_request_with_context(
            json!({ "src_ilk": "ilk:legacy-context-value" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        assert_eq!(
            extract_src_ilk(&msg).as_deref(),
            Some("ilk:11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(src_ilk_source(&msg), "meta");
    }

    #[test]
    fn extract_src_ilk_does_not_read_legacy_meta_context() {
        let msg = sample_user_request_with_context(
            json!({ "src_ilk": "ilk:11111111-1111-4111-8111-111111111111" }),
            None,
        );
        assert_eq!(extract_src_ilk(&msg), None);
        assert_eq!(src_ilk_source(&msg), "missing");
    }

    #[test]
    fn extract_src_ilk_reports_missing_when_absent() {
        let msg = sample_user_request_with_context(json!({}), None);
        assert_eq!(extract_src_ilk(&msg), None);
        assert_eq!(src_ilk_source(&msg), "missing");
    }

    #[test]
    fn extract_thread_id_reads_from_meta_top_level_first() {
        let mut msg =
            sample_user_request_with_context(json!({ "thread_id": "legacy-thread-1" }), None);
        msg.meta.thread_id = Some("thread:canonical-1".to_string());
        assert_eq!(
            extract_thread_id(&msg).as_deref(),
            Some("thread:canonical-1")
        );
    }

    #[test]
    fn extract_thread_id_does_not_read_legacy_meta_context() {
        let msg = sample_user_request_with_context(json!({ "thread_id": "legacy-thread-1" }), None);
        assert_eq!(extract_thread_id(&msg), None);
    }

    #[test]
    fn require_src_ilk_returns_missing_src_ilk_error() {
        let ctx = BehaviorContext {
            thread_id: None,
            src_ilk: None,
        };
        let err = require_src_ilk(&ctx).expect_err("missing src_ilk should fail");
        assert!(err.to_string().contains("missing_src_ilk"));
    }

    #[test]
    fn extract_final_output_from_tool_results_parses_artifact_envelope() {
        let csv_b64 = base64::engine::general_purpose::STANDARD.encode("a,b\n1,2\n");
        let result = FunctionLoopRunResult {
            final_assistant_text: Some("texto provisional".to_string()),
            items: vec![FunctionLoopItem::ToolResult {
                result: fluxbee_ai_sdk::FunctionToolResult {
                    call_id: "call_1".to_string(),
                    response_id: None,
                    name: "generate_csv_artifact".to_string(),
                    arguments: json!({}),
                    output: json!({
                        "status": "ok",
                        "final_output": {
                            "text": "aca esta",
                            "artifacts": [{
                                "filename": "reporte.csv",
                                "mime": "text/csv",
                                "bytes_base64": csv_b64
                            }]
                        }
                    }),
                    is_error: false,
                },
            }],
            tokens_used: 0,
        };

        let output = extract_final_output_from_tool_results(&result)
            .expect("tool final output should parse")
            .expect("final output should exist");
        match output {
            AiBehaviorOutput::Final(final_output) => {
                assert_eq!(final_output.text.as_deref(), Some("aca esta"));
                assert_eq!(final_output.artifacts.len(), 1);
                assert_eq!(final_output.artifacts[0].filename, "reporte.csv");
                assert_eq!(final_output.artifacts[0].mime, "text/csv");
                assert_eq!(final_output.artifacts[0].bytes, b"a,b\n1,2\n".to_vec());
            }
            other => panic!("expected final output, got {other:?}"),
        }
    }

    fn decode_tool_artifact(output: Value) -> (Option<String>, Vec<ToolFinalArtifactItem>) {
        let final_output = output
            .get("final_output")
            .cloned()
            .expect("tool should return final_output");
        let envelope: ToolFinalArtifactEnvelope =
            serde_json::from_value(final_output).expect("tool final_output should deserialize");
        (envelope.text, envelope.artifacts)
    }

    #[tokio::test]
    async fn generate_pdf_artifact_tool_returns_pdf_bytes() {
        let tool = GeneratePdfArtifactTool;
        let output = tool
            .call(json!({
                "filename": "reporte.pdf",
                "title": "Reporte",
                "lines": ["Linea 1", "Linea 2"]
            }))
            .await
            .expect("pdf tool should succeed");
        let (_text, artifacts) = decode_tool_artifact(output);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].filename, "reporte.pdf");
        assert_eq!(artifacts[0].mime, "application/pdf");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&artifacts[0].bytes_base64)
            .expect("valid base64");
        assert!(bytes.starts_with(b"%PDF-1.4"));
    }

    #[tokio::test]
    async fn generate_textual_artifact_tools_return_expected_mime_and_filenames() {
        let text_tool = GenerateTextArtifactTool;
        let json_tool = GenerateJsonArtifactTool;
        let markdown_tool = GenerateMarkdownArtifactTool;
        let html_tool = GenerateHtmlArtifactTool;

        let text_output = text_tool
            .call(json!({
                "filename": "nota.txt",
                "content": "hola mundo"
            }))
            .await
            .expect("text tool should succeed");
        let json_output = json_tool
            .call(json!({
                "filename": "payload.json",
                "data": {"ok": true, "count": 2}
            }))
            .await
            .expect("json tool should succeed");
        let markdown_output = markdown_tool
            .call(json!({
                "filename": "readme.md",
                "content": "# Hola\n\n- uno\n- dos"
            }))
            .await
            .expect("markdown tool should succeed");
        let html_output = html_tool
            .call(json!({
                "filename": "page.html",
                "content": "<h1>Hola</h1><p>Mundo</p>"
            }))
            .await
            .expect("html tool should succeed");

        let (_, text_artifacts) = decode_tool_artifact(text_output);
        let (_, json_artifacts) = decode_tool_artifact(json_output);
        let (_, markdown_artifacts) = decode_tool_artifact(markdown_output);
        let (_, html_artifacts) = decode_tool_artifact(html_output);

        assert_eq!(text_artifacts[0].filename, "nota.txt");
        assert_eq!(text_artifacts[0].mime, "text/plain");
        assert_eq!(json_artifacts[0].filename, "payload.json");
        assert_eq!(json_artifacts[0].mime, "application/json");
        assert_eq!(markdown_artifacts[0].filename, "readme.md");
        assert_eq!(markdown_artifacts[0].mime, "text/markdown");
        assert_eq!(html_artifacts[0].filename, "page.html");
        assert_eq!(html_artifacts[0].mime, "text/html");

        let text_bytes = base64::engine::general_purpose::STANDARD
            .decode(&text_artifacts[0].bytes_base64)
            .expect("valid txt base64");
        let json_bytes = base64::engine::general_purpose::STANDARD
            .decode(&json_artifacts[0].bytes_base64)
            .expect("valid json base64");
        let markdown_bytes = base64::engine::general_purpose::STANDARD
            .decode(&markdown_artifacts[0].bytes_base64)
            .expect("valid markdown base64");
        let html_bytes = base64::engine::general_purpose::STANDARD
            .decode(&html_artifacts[0].bytes_base64)
            .expect("valid html base64");

        assert_eq!(
            String::from_utf8(text_bytes).expect("utf8 txt"),
            "hola mundo"
        );
        assert!(String::from_utf8(json_bytes)
            .expect("utf8 json")
            .contains("\"ok\": true"));
        assert!(String::from_utf8(markdown_bytes)
            .expect("utf8 markdown")
            .contains("# Hola"));
        assert!(String::from_utf8(html_bytes)
            .expect("utf8 html")
            .contains("<h1>Hola</h1>"));
    }

    #[tokio::test]
    async fn generate_xlsx_artifact_tool_returns_zip_with_workbook_parts() {
        let tool = GenerateXlsxArtifactTool;
        let output = tool
            .call(json!({
                "filename": "tabla.xlsx",
                "headers": ["nombre", "edad"],
                "rows": [["Ana", "30"], ["Luis", "41"]]
            }))
            .await
            .expect("xlsx tool should succeed");
        let (_text, artifacts) = decode_tool_artifact(output);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].filename, "tabla.xlsx");
        assert_eq!(
            artifacts[0].mime,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&artifacts[0].bytes_base64)
            .expect("valid base64");
        let cursor = Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(cursor).expect("xlsx should be a zip archive");
        zip.by_name("[Content_Types].xml")
            .expect("xlsx content types");
        zip.by_name("xl/workbook.xml").expect("xlsx workbook");
        zip.by_name("xl/worksheets/sheet1.xml").expect("xlsx sheet");
    }

    #[tokio::test]
    async fn generate_docx_artifact_tool_returns_zip_with_document_parts() {
        let tool = GenerateDocxArtifactTool;
        let output = tool
            .call(json!({
                "filename": "nota.docx",
                "title": "Nota",
                "paragraphs": ["Parrafo 1", "Parrafo 2"],
                "bullets": ["Uno", "Dos"]
            }))
            .await
            .expect("docx tool should succeed");
        let (_text, artifacts) = decode_tool_artifact(output);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].filename, "nota.docx");
        assert_eq!(
            artifacts[0].mime,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&artifacts[0].bytes_base64)
            .expect("valid base64");
        let cursor = Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(cursor).expect("docx should be a zip archive");
        zip.by_name("[Content_Types].xml")
            .expect("docx content types");
        zip.by_name("word/document.xml").expect("docx document");
        zip.by_name("word/styles.xml").expect("docx styles");
    }

    #[tokio::test]
    async fn generate_png_and_jpeg_artifact_tools_return_decodable_images() {
        let png_tool = GeneratePngArtifactTool;
        let jpeg_tool = GenerateJpegArtifactTool;

        let png_output = png_tool
            .call(json!({
                "filename": "grafico.png",
                "bands": ["rojo", "verde", "azul"],
                "width": 320,
                "height": 180
            }))
            .await
            .expect("png tool should succeed");
        let jpeg_output = jpeg_tool
            .call(json!({
                "filename": "grafico.jpg",
                "bands": ["uno", "dos"],
                "width": 320,
                "height": 180
            }))
            .await
            .expect("jpeg tool should succeed");

        let (_, png_artifacts) = decode_tool_artifact(png_output);
        let (_, jpeg_artifacts) = decode_tool_artifact(jpeg_output);

        let png_bytes = base64::engine::general_purpose::STANDARD
            .decode(&png_artifacts[0].bytes_base64)
            .expect("valid png base64");
        let jpeg_bytes = base64::engine::general_purpose::STANDARD
            .decode(&jpeg_artifacts[0].bytes_base64)
            .expect("valid jpeg base64");

        let png = image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
            .expect("png should decode");
        let jpeg = image::load_from_memory_with_format(&jpeg_bytes, image::ImageFormat::Jpeg)
            .expect("jpeg should decode");

        assert_eq!(png.width(), 320);
        assert_eq!(png.height(), 180);
        assert_eq!(jpeg.width(), 320);
        assert_eq!(jpeg.height(), 180);
    }

    #[test]
    fn parse_effective_config_rejects_openai_secret_contract_fields() {
        let cases = [
            (
                json!({
                    "behavior": {"kind": "openai_chat", "model": "gpt-4.1-mini"},
                    "secrets": {"openai": {"api_key": "sk-test"}}
                }),
                "config.secrets",
            ),
            (
                json!({
                    "behavior": {
                        "kind": "openai_chat",
                        "model": "gpt-4.1-mini",
                        "api_key": "sk-test"
                    }
                }),
                "config.behavior.api_key",
            ),
            (
                json!({
                    "behavior": {
                        "kind": "openai_chat",
                        "model": "gpt-4.1-mini",
                        "api_key_env": "OPENAI_API_KEY"
                    }
                }),
                "config.behavior.api_key_env",
            ),
            (
                json!({
                    "behavior": {
                        "kind": "openai_chat",
                        "model": "gpt-4.1-mini",
                        "openai": {"api_key": "sk-test"}
                    }
                }),
                "config.behavior.openai",
            ),
            (
                json!({
                    "behavior": {"kind": "openai_chat", "model": "gpt-4.1-mini"},
                    "assets": {"role_hash": "abc"}
                }),
                "config.assets",
            ),
        ];

        for (config, field) in cases {
            assert_eq!(find_openai_secret_contract_field(&config), Some(field));
            let err = parse_effective_config_doc(&config)
                .expect_err("secret-bearing AI config should be rejected");
            assert!(err.to_string().contains(field));
        }
    }

    #[test]
    fn resolve_tenant_id_prefers_explicit_over_hint() {
        let explicit = "tnt:11111111-1111-4111-8111-111111111111";
        let hint = "tnt:22222222-2222-4222-8222-222222222222";
        let out = resolve_tenant_id_for_register(Some(explicit), Some(hint), None);
        assert_eq!(out.as_deref(), Some(explicit));
    }

    #[test]
    fn resolve_tenant_id_uses_hint_when_explicit_missing() {
        let hint = "tnt:22222222-2222-4222-8222-222222222222";
        let out = resolve_tenant_id_for_register(None, Some(hint), None);
        assert_eq!(out.as_deref(), Some(hint));
    }
}
