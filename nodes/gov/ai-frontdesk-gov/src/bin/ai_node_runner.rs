use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fluxbee_ai_sdk::router_client::{RouterReader, RouterWriter};
use fluxbee_ai_sdk::{
    build_openai_user_content_parts, build_reply_message_runtime_src, build_text_response,
    extract_text, resolve_model_input_from_payload_with_options, AiNode, AiNodeConfig,
    FunctionCallingConfig, FunctionCallingRunner, FunctionRunInput, FunctionTool,
    FunctionToolDefinition, FunctionToolProvider, FunctionToolRegistry,
    ImmediateConversationMemory, LanceDbThreadStateStore, Message, ModelInputOptions,
    ModelSettings, NodeRuntime, OpenAiResponsesClient, ResolvedModelInput, RetryPolicy,
    RouterClient, RuntimeConfig, ThreadStateStore, ThreadStateToolsProvider,
};
use fluxbee_sdk::node_client::NodeError;
use fluxbee_sdk::protocol::{
    Destination, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_secret_record, load_node_secret_record, load_node_secret_record_with_root,
    managed_node_config_path, managed_node_name, save_node_secret_record,
    save_node_secret_record_with_root, NodeSecretDescriptor, NodeSecretWriteOptions,
    NODE_SECRET_REDACTION_TOKEN,
};
use fluxbee_sdk::{MSG_ILK_REGISTER, MSG_TNT_CREATE};
use gov_common::{
    gov_identity_config_from_env, identity_error_to_tool_payload, looks_like_tenant_id,
    resolve_tenant_id_for_register, tenant_resolution_source, GovIdentityConfig,
    GOV_IDENTITY_TENANT_ID_ENV,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs as tokio_fs;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const MSG_NODE_STATUS_GET: &str = "NODE_STATUS_GET";
const MSG_NODE_STATUS_GET_RESPONSE: &str = "NODE_STATUS_GET_RESPONSE";
const NODE_STATUS_DEFAULT_HANDLER_ENABLED: &str = "NODE_STATUS_DEFAULT_HANDLER_ENABLED";
const NODE_STATUS_DEFAULT_HEALTH_STATE: &str = "NODE_STATUS_DEFAULT_HEALTH_STATE";
const IMMEDIATE_INTERACTION_MAX_CHARS: usize = 1_200;
const AI_LOCAL_SECRET_KEY_OPENAI: &str = "openai_api_key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiApiKeySource {
    LocalFile,
    ControlPlaneLegacy,
    YamlInlineLegacy,
    EnvLegacy,
    Missing,
}

impl OpenAiApiKeySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::ControlPlaneLegacy => "effective_config_legacy",
            Self::YamlInlineLegacy => "yaml_inline_legacy",
            Self::EnvLegacy => "env_legacy",
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
    #[serde(default = "default_openai_api_key_env")]
    api_key_env: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    openai: Option<OpenAiCredentialsSection>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiCredentialsSection {
    #[serde(default)]
    api_key: Option<String>,
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
    #[serde(default)]
    secrets: Option<EffectiveSecretsSection>,
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
    api_key_env: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    openai: Option<OpenAiCredentialsSection>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveSecretsSection {
    #[serde(default)]
    openai: Option<EffectiveOpenAiSecrets>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveOpenAiSecrets {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
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

fn default_openai_api_key_env() -> String {
    "OPENAI_API_KEY".to_string()
}

fn default_multimodal_for_runtime() -> bool {
    false
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
    api_key_env: String,
    yaml_inline_api_key: Option<String>,
    base_url: Option<String>,
    immediate_memory: ImmediateMemorySection,
    multimodal: bool,
}

struct GenericAiNode {
    mode: RunnerMode,
    node_name: String,
    behavior: Arc<RwLock<Option<NodeBehavior>>>,
    dynamic_config_dir: PathBuf,
    thread_state_store: Option<Arc<dyn ThreadStateStore>>,
    immediate_memory_store: Option<Arc<ImmediateMemoryStore>>,
    gov_identity: GovIdentityConfig,
    gov_identity_bridge: Option<Arc<GovIdentityBridge>>,
    control_plane: Arc<RwLock<ControlPlaneState>>,
}

struct SharedRouterConnection {
    inner: Mutex<SharedRouterState>,
}

struct SharedRouterState {
    reader: RouterReader,
    writer: RouterWriter,
    backlog: VecDeque<Message>,
}

impl SharedRouterConnection {
    fn new(reader: RouterReader, writer: RouterWriter) -> Self {
        Self {
            inner: Mutex::new(SharedRouterState {
                reader,
                writer,
                backlog: VecDeque::new(),
            }),
        }
    }

    async fn uuid(&self) -> String {
        let guard = self.inner.lock().await;
        guard.writer.uuid().to_string()
    }

    async fn read_runtime_message(&self, timeout: Duration) -> fluxbee_ai_sdk::Result<Message> {
        let mut guard = self.inner.lock().await;
        if let Some(msg) = guard.backlog.pop_front() {
            return Ok(msg);
        }
        guard.reader.read_timeout(timeout).await
    }

    async fn write(&self, msg: Message) -> fluxbee_ai_sdk::Result<()> {
        let guard = self.inner.lock().await;
        guard.writer.write(msg).await
    }
}

struct GovIdentityBridge {
    connection: Arc<SharedRouterConnection>,
}

impl GovIdentityBridge {
    fn new(connection: Arc<SharedRouterConnection>) -> Self {
        Self { connection }
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
        let src = self.connection.uuid().await;
        let req = Message {
            routing: Routing {
                src,
                dst: Destination::Unicast(target.to_string()),
                ttl: 16,
                trace_id: trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(action.to_string()),
                src_ilk: None,
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload,
        };
        self.connection
            .write(req)
            .await
            .map_err(|err| format!("identity send failed: {err}"))?;
        let expected_msg = format!("{action}_RESPONSE");
        let deadline = Instant::now() + timeout;

        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "timeout waiting identity response: action={action} trace_id={trace_id} target={target} timeout_ms={}",
                    timeout.as_millis()
                ));
            }
            let remaining = deadline.saturating_duration_since(now);
            let mut guard = self.connection.inner.lock().await;
            if let Some(idx) = guard
                .backlog
                .iter()
                .position(|msg| msg.routing.trace_id == trace_id)
            {
                let msg = guard.backlog.remove(idx).expect("backlog index");
                drop(guard);
                return Self::parse_identity_reply(msg, &expected_msg, target, trace_id);
            }
            match guard.reader.read_timeout(remaining).await {
                Ok(msg) => {
                    if msg.routing.trace_id == trace_id {
                        drop(guard);
                        return Self::parse_identity_reply(msg, &expected_msg, target, trace_id);
                    }
                    guard.backlog.push_back(msg);
                }
                Err(fluxbee_ai_sdk::errors::AiSdkError::Node(NodeError::Timeout)) => {
                    return Err(format!(
                        "timeout waiting identity response: action={action} trace_id={trace_id} target={target} timeout_ms={}",
                        timeout.as_millis()
                    ));
                }
                Err(err) => return Err(format!("identity receive failed: {err}")),
            }
        }
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

#[derive(Debug, Clone)]
struct BehaviorContext {
    thread_id: Option<String>,
    src_ilk: Option<String>,
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
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                src_ilk = ?behavior_ctx.src_ilk,
                sender = ?incoming_sender_hint(&msg),
                thread_id = ?behavior_ctx.thread_id,
                input_len = input.len(),
                input_preview = %text_preview(&input, 240),
                "incoming user message"
            );
        }
        let output = match &behavior {
            NodeBehavior::Echo => format!("Echo: {input}"),
            NodeBehavior::OpenAiChat(openai) => {
                let input_parts = if openai.multimodal {
                    if let Some(resolved) = resolved_user_input.as_ref() {
                        match build_openai_user_content_parts(resolved).await {
                            Ok(parts) => Some(parts),
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
                    .run_openai_chat(openai, input, input_parts, &behavior_ctx)
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

        let payload = build_text_response(output)?;
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
    ) -> fluxbee_ai_sdk::Result<String> {
        let api_key = self.resolve_openai_api_key(openai).await.ok_or_else(|| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(
                "missing OpenAI api key (local secrets.json, CONFIG_SET override, YAML inline, or env)"
                    .to_string(),
            )
        })?;
        let mut client = OpenAiResponsesClient::new(api_key);
        if let Some(base_url) = &openai.base_url {
            client = client.with_base_url(base_url.clone());
        }
        let tool_registry = self.build_tool_registry(ctx)?;
        if !tool_registry.definitions().is_empty() {
            let model = client.clone().function_model(
                openai.model.clone(),
                openai.instructions.clone(),
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
            if let Some(text) = result.final_assistant_text {
                self.persist_immediate_turn(openai, ctx, &input, &text)
                    .await;
                return Ok(text);
            }
        }

        let current_user_input = input.clone();
        let req = fluxbee_ai_sdk::llm::LlmRequest {
            model: openai.model.clone(),
            system: openai.instructions.clone(),
            input,
            input_parts,
            max_output_tokens: None,
            model_settings: Some(openai.model_settings.clone()),
        };
        let response = fluxbee_ai_sdk::llm::LlmClient::generate(&client, req).await?;
        self.persist_immediate_turn(openai, ctx, &current_user_input, &response.content)
            .await;
        Ok(response.content)
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
        if self.mode == RunnerMode::Gov {
            self.register_gov_tools(&mut registry, ctx)?;
        }
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
        Ok(())
    }

    fn register_gov_tools(
        &self,
        registry: &mut FunctionToolRegistry,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<()> {
        let tool = IlkRegisterTool {
            scoped_src_ilk: ctx.src_ilk.clone(),
            default_tenant_id: self.resolve_effective_tenant_id(),
            identity: self.gov_identity.clone(),
            bridge: self.gov_identity_bridge.clone(),
        };
        registry.register(Arc::new(tool))?;
        Ok(())
    }

    async fn resolve_openai_api_key(&self, openai: &OpenAiChatRuntime) -> Option<String> {
        self.resolve_openai_api_key_with_source(openai).await.0
    }

    async fn resolve_openai_api_key_with_source(
        &self,
        openai: &OpenAiChatRuntime,
    ) -> (Option<String>, OpenAiApiKeySource) {
        let from_local_file = load_local_openai_api_key(&self.node_name);
        if from_local_file.is_some() {
            return (from_local_file, OpenAiApiKeySource::LocalFile);
        }
        let from_control_plane = {
            let state = self.control_plane.read().await;
            state
                .effective_config
                .as_ref()
                .and_then(extract_openai_api_key_from_config)
        };
        if from_control_plane.is_some() {
            return (from_control_plane, OpenAiApiKeySource::ControlPlaneLegacy);
        }
        if openai.yaml_inline_api_key.is_some() {
            return (
                openai.yaml_inline_api_key.clone(),
                OpenAiApiKeySource::YamlInlineLegacy,
            );
        }
        match std::env::var(&openai.api_key_env).ok() {
            Some(value) => (Some(value), OpenAiApiKeySource::EnvLegacy),
            None => (None, OpenAiApiKeySource::Missing),
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
        if let Some(api_key) = extract_openai_api_key_from_effective_config(&config_doc)
            .filter(|value| !value.trim().is_empty() && value != NODE_SECRET_REDACTION_TOKEN)
        {
            let options = build_secret_write_options_from_message(msg);
            if let Err(err) = persist_local_openai_api_key(&self.node_name, &api_key, &options) {
                return self.error_response(
                    "secret_persist_error",
                    format!("Failed to persist local OpenAI secret: {err}"),
                    schema_version,
                    config_version,
                    self.control_plane.read().await.current_state.as_str(),
                );
            }
        }
        strip_openai_api_key_from_effective_config(&mut config_doc);
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
        let (ok, config_source) = if state.effective_config.is_some() {
            (true, state.config_source)
        } else {
            (false, "none")
        };
        let api_key_source = match state.effective_config.as_ref() {
            Some(config) => {
                resolve_openai_api_key_source_from_effective_config(&self.node_name, config)
            }
            None => {
                if load_local_openai_api_key(&self.node_name).is_some() {
                    OpenAiApiKeySource::LocalFile
                } else {
                    OpenAiApiKeySource::Missing
                }
            }
        };
        let error = if ok {
            Value::Null
        } else {
            json!({"code":"node_not_configured","message":"No effective config available"})
        };
        let mut secret_descriptor =
            NodeSecretDescriptor::new("config.secrets.openai.api_key", AI_LOCAL_SECRET_KEY_OPENAI);
        secret_descriptor.required = true;
        secret_descriptor.configured = api_key_source != OpenAiApiKeySource::Missing;
        secret_descriptor.persistence = api_key_source.as_str().to_string();
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
                "node_kind": "AI.frontdesk.gov",
                "supports": ["CONFIG_GET", "CONFIG_SET"],
                "required_fields": [
                    "config.behavior.kind",
                    "config.behavior.model",
                    "config.secrets.openai.api_key"
                ],
                "optional_fields": [
                    "config.behavior.instructions",
                    "config.behavior.model_settings",
                    "config.behavior.base_url",
                    "config.behavior.capabilities.multimodal",
                    "config.secrets.openai.api_key_env"
                ],
                "secrets": [secret_descriptor],
                "notes": [
                    "Preferred secret field is config.secrets.openai.api_key.",
                    "Legacy aliases config.behavior.openai.api_key and config.behavior.api_key remain accepted during migration.",
                    "AI.frontdesk.gov defaults behavior.capabilities.multimodal=false unless explicitly overridden.",
                    "Secret values are persisted in local secrets.json and always returned redacted."
                ]
            },
            "effective_config": state.effective_config.as_ref().map(redact_secrets),
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

fn normalize_health_state(raw: &str) -> &'static str {
    match raw.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => "HEALTHY",
        "DEGRADED" => "DEGRADED",
        "ERROR" => "ERROR",
        "UNKNOWN" => "UNKNOWN",
        _ => "HEALTHY",
    }
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

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
            },
            "roles": [],
            "capabilities": []
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
            mode = %args.mode.as_str(),
            "starting ai_node_runner without YAML config (UNCONFIGURED mode)"
        );
        run_unconfigured_bootstrap(bootstrap_node, args.mode).await?;
        return Ok(());
    }

    let mut runners = JoinSet::new();
    let mode = args.mode;
    for (config_path, cfg) in loaded {
        runners.spawn(async move { run_one_config(config_path, cfg, mode).await });
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

async fn run_single_connection_runtime(
    connection: Arc<SharedRouterConnection>,
    node: GenericAiNode,
    config: RuntimeConfig,
) -> fluxbee_ai_sdk::Result<()> {
    tracing::info!(
        worker_pool_size = config.worker_pool_size,
        queue_capacity = config.queue_capacity,
        read_timeout_ms = config.read_timeout.as_millis() as u64,
        handler_timeout_ms = config.handler_timeout.as_millis() as u64,
        write_timeout_ms = config.write_timeout.as_millis() as u64,
        retry_max_attempts = config.retry_policy.max_attempts,
        retry_initial_backoff_ms = config.retry_policy.initial_backoff.as_millis() as u64,
        retry_max_backoff_ms = config.retry_policy.max_backoff.as_millis() as u64,
        metrics_log_interval_s = config.metrics_log_interval.as_secs(),
        "ai runtime started (single-connection gov mode)"
    );

    let mut read_messages = 0u64;
    let mut idle_read_timeouts = 0u64;
    let mut reconnect_events = 0u64;
    let mut reconnect_wait_cycles = 0u64;
    let mut processed_messages = 0u64;
    let mut responses_sent = 0u64;
    let mut retry_attempts = 0u64;
    let mut next_metrics_log = Instant::now() + config.metrics_log_interval;

    loop {
        let msg = match connection.read_runtime_message(config.read_timeout).await {
            Ok(msg) => msg,
            Err(fluxbee_ai_sdk::errors::AiSdkError::Node(NodeError::Timeout)) => {
                idle_read_timeouts = idle_read_timeouts.saturating_add(1);
                tracing::debug!(
                    read_timeout_ms = config.read_timeout.as_millis() as u64,
                    "ai runtime read timeout (idle)"
                );
                if Instant::now() >= next_metrics_log {
                    tracing::debug!(
                        read_messages,
                        idle_read_timeouts,
                        reconnect_events,
                        reconnect_wait_cycles,
                        enqueued_messages = read_messages,
                        processed_messages,
                        responses_sent,
                        recoverable_exhausted = 0,
                        fatal_errors = 0,
                        retry_attempts,
                        "ai runtime metrics"
                    );
                    next_metrics_log = Instant::now() + config.metrics_log_interval;
                }
                continue;
            }
            Err(err) if is_transient_link_error(&err) => {
                reconnect_events = reconnect_events.saturating_add(1);
                tracing::warn!(
                    error = %err,
                    "ai runtime disconnected from router; entering reconnect wait"
                );
                reconnect_wait_cycles = reconnect_wait_cycles
                    .saturating_add(wait_for_shared_reconnect(connection.as_ref()).await);
                continue;
            }
            Err(err) => return Err(err),
        };
        read_messages = read_messages.saturating_add(1);

        let mut attempt = 0usize;
        let max_attempts = config.retry_policy.max_attempts.max(1);
        let mut backoff = config.retry_policy.initial_backoff;
        let maybe_response = loop {
            attempt += 1;
            match tokio::time::timeout(config.handler_timeout, node.on_message(msg.clone())).await {
                Ok(Ok(response)) => break response,
                Ok(Err(err)) if err.is_recoverable() && attempt < max_attempts => {
                    retry_attempts = retry_attempts.saturating_add(1);
                    tracing::debug!(
                        stage = "handler",
                        attempt,
                        next_backoff_ms = backoff.as_millis() as u64,
                        error = %err,
                        "recoverable error, retrying"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff =
                        std::cmp::min(backoff.saturating_mul(2), config.retry_policy.max_backoff);
                }
                Ok(Err(err)) if err.is_recoverable() => {
                    return Err(fluxbee_ai_sdk::errors::AiSdkError::RecoverableExhausted(
                        format!("handler failed after {max_attempts} attempts: {err}"),
                    ));
                }
                Ok(Err(err)) => return Err(err),
                Err(_) => {
                    return Err(fluxbee_ai_sdk::errors::AiSdkError::Timeout(
                        "node handler timeout".to_string(),
                    ))
                }
            }
        };

        if let Some(mut response) = maybe_response {
            tracing::debug!(
                trace_id = %response.routing.trace_id,
                dst = ?response.routing.dst,
                "sending ai response to router"
            );
            response.routing.src = connection.uuid().await;
            tokio::time::timeout(config.write_timeout, connection.write(response))
                .await
                .map_err(|_| {
                    fluxbee_ai_sdk::errors::AiSdkError::Timeout("router write timeout".to_string())
                })??;
            tracing::debug!("ai response delivered to router");
            responses_sent = responses_sent.saturating_add(1);
        }
        processed_messages = processed_messages.saturating_add(1);

        if Instant::now() >= next_metrics_log {
            tracing::debug!(
                read_messages,
                idle_read_timeouts,
                reconnect_events,
                reconnect_wait_cycles,
                enqueued_messages = read_messages,
                processed_messages,
                responses_sent,
                recoverable_exhausted = 0,
                fatal_errors = 0,
                retry_attempts,
                "ai runtime metrics"
            );
            next_metrics_log = Instant::now() + config.metrics_log_interval;
        }
    }
}

fn is_transient_link_error(err: &fluxbee_ai_sdk::errors::AiSdkError) -> bool {
    matches!(
        err,
        fluxbee_ai_sdk::errors::AiSdkError::Node(NodeError::Io(_) | NodeError::Disconnected)
    )
}

async fn wait_for_shared_reconnect(connection: &SharedRouterConnection) -> u64 {
    let mut attempt: u64 = 0;
    let mut wait_cycles: u64 = 0;
    let mut backoff = Duration::from_millis(200);
    let max_backoff = Duration::from_secs(5);

    loop {
        let poll_timeout = Duration::from_millis(250);
        match connection.read_runtime_message(poll_timeout).await {
            Ok(msg) => {
                let mut guard = connection.inner.lock().await;
                guard.backlog.push_front(msg);
                drop(guard);
                tracing::info!(attempt, "ai runtime reconnected to router");
                return wait_cycles;
            }
            Err(fluxbee_ai_sdk::errors::AiSdkError::Node(NodeError::Timeout)) => {}
            Err(err) if is_transient_link_error(&err) => {}
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "unexpected error while probing reconnect; keeping retry loop"
                );
            }
        }

        attempt = attempt.saturating_add(1);
        wait_cycles = wait_cycles.saturating_add(1);
        let wait_for = with_jitter(backoff);
        tracing::info!(
            attempt,
            backoff_ms = backoff.as_millis() as u64,
            wait_ms = wait_for.as_millis() as u64,
            "ai runtime reconnecting to router"
        );
        tokio::time::sleep(wait_for).await;
        backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
    }
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
    mode: RunnerMode,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let startup_effective_doc = build_startup_effective_config_doc(&cfg);
    let startup_effective_doc =
        materialize_effective_defaults(&cfg.node.name, startup_effective_doc);
    let startup_effective_config = serde_json::to_value(&startup_effective_doc)?;
    let persisted_dynamic =
        load_persisted_dynamic_config(&PathBuf::from(&cfg.node.dynamic_config_dir), &cfg.node.name);
    let behavior = build_behavior(&cfg)?;
    let ai_node_config = AiNodeConfig {
        name: cfg.node.name,
        version: cfg.node.version,
        router_socket: PathBuf::from(cfg.node.router_socket),
        uuid_persistence_dir: PathBuf::from(cfg.node.uuid_persistence_dir),
        config_dir: PathBuf::from(cfg.node.config_dir),
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
        node_name = %ai_node_config.name,
        mode = %mode.as_str(),
        "starting ai_node_runner node instance"
    );

    let node_name = ai_node_config.name.clone();
    let gov_identity = gov_identity_config_from_env();
    let thread_state_store =
        init_thread_state_store(&node_name, &PathBuf::from(&cfg.node.dynamic_config_dir)).await;
    let immediate_memory_store =
        init_immediate_memory_store(&node_name, &PathBuf::from(&cfg.node.dynamic_config_dir)).await;
    let node = GenericAiNode {
        mode,
        node_name,
        behavior: Arc::new(RwLock::new(Some(behavior))),
        dynamic_config_dir: PathBuf::from(cfg.node.dynamic_config_dir),
        thread_state_store,
        immediate_memory_store,
        gov_identity,
        gov_identity_bridge: None,
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
    };
    if mode == RunnerMode::Gov {
        let client = RouterClient::connect(ai_node_config).await?;
        let (reader, writer) = client.split();
        let shared_conn = Arc::new(SharedRouterConnection::new(reader, writer));
        let mut node = node;
        node.gov_identity_bridge = Some(Arc::new(GovIdentityBridge::new(shared_conn.clone())));
        run_single_connection_runtime(shared_conn, node, runtime_config).await?;
    } else {
        let client = RouterClient::connect(ai_node_config).await?;
        let runtime = NodeRuntime::new(client, node);
        runtime.run_with_config(runtime_config).await?;
    }
    Ok(())
}

async fn run_unconfigured_bootstrap(
    node: NodeSection,
    mode: RunnerMode,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let node_name = node.name.clone();
    let dynamic_dir = PathBuf::from(node.dynamic_config_dir.clone());
    let thread_state_store = init_thread_state_store(&node_name, &dynamic_dir).await;
    let immediate_memory_store = init_immediate_memory_store(&node_name, &dynamic_dir).await;
    let persisted_dynamic = load_persisted_dynamic_config(&dynamic_dir, &node_name);
    let spawn_effective = if persisted_dynamic.is_none() {
        load_effective_config_from_spawn(&node_name)
    } else {
        None
    };
    let (behavior, state) = match persisted_dynamic.as_ref() {
        Some(stored) => {
            let mut materialized =
                materialize_effective_defaults(&node_name, stored.config.clone());
            let migrated = migrate_bootstrap_openai_secret(&node_name, &mut materialized);
            if let Err(err) = &migrated {
                tracing::warn!(
                    node_name = %node_name,
                    error = %err,
                    "failed to migrate persisted OpenAI secret to local secrets.json"
                );
            }
            if migrated.as_ref().ok().copied().unwrap_or(false) {
                if let Err(err) = persist_dynamic_config(
                    &dynamic_dir,
                    &node_name,
                    stored.schema_version,
                    stored.config_version,
                    &materialized,
                ) {
                    tracing::warn!(
                        node_name = %node_name,
                        error = %err,
                        "failed to rewrite persisted dynamic config after secret migration"
                    );
                }
            }
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
                let mut spawn_config = spawn_cfg.config.clone();
                let migrated = migrate_bootstrap_openai_secret(&node_name, &mut spawn_config);
                if let Err(err) = &migrated {
                    tracing::warn!(
                        node_name = %node_name,
                        error = %err,
                        "failed to migrate spawn OpenAI secret to local secrets.json"
                    );
                }
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

    let ai_node_config = AiNodeConfig {
        name: node.name,
        version: node.version,
        router_socket: PathBuf::from(node.router_socket),
        uuid_persistence_dir: PathBuf::from(node.uuid_persistence_dir),
        config_dir: PathBuf::from(node.config_dir),
    };
    tracing::info!(
        node_name = %node_name,
        mode = %mode.as_str(),
        "starting ai_node_runner bootstrap instance"
    );
    let gov_identity = gov_identity_config_from_env();
    let ai_node = GenericAiNode {
        mode,
        node_name,
        behavior: Arc::new(RwLock::new(behavior)),
        dynamic_config_dir: dynamic_dir,
        thread_state_store,
        immediate_memory_store,
        gov_identity,
        gov_identity_bridge: None,
        control_plane: Arc::new(RwLock::new(state)),
    };
    if mode == RunnerMode::Gov {
        let client = RouterClient::connect(ai_node_config).await?;
        let (reader, writer) = client.split();
        let shared_conn = Arc::new(SharedRouterConnection::new(reader, writer));
        let mut ai_node = ai_node;
        ai_node.gov_identity_bridge = Some(Arc::new(GovIdentityBridge::new(shared_conn.clone())));
        run_single_connection_runtime(shared_conn, ai_node, RuntimeConfig::default()).await?;
    } else {
        let client = RouterClient::connect(ai_node_config).await?;
        let runtime = NodeRuntime::new(client, ai_node);
        runtime.run_with_config(RuntimeConfig::default()).await?;
    }
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
    mode: RunnerMode,
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
    let mut parsed = RunnerArgs {
        mode: RunnerMode::Gov,
        ..RunnerArgs::default()
    };
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
                if normalized != "gov" {
                    return Err(format!(
                        "--mode={value} is not supported in AI.frontdesk.gov runtime (only gov)"
                    )
                    .into());
                }
                parsed.mode = RunnerMode::Gov;
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
            let yaml_inline_api_key = openai
                .openai
                .as_ref()
                .and_then(|v| v.api_key.clone())
                .or_else(|| openai.api_key.clone());
            let multimodal = openai
                .capabilities
                .as_ref()
                .and_then(|caps| caps.multimodal)
                .unwrap_or_else(default_multimodal_for_runtime);
            NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                model: openai.model.clone(),
                instructions,
                model_settings,
                api_key_env: openai.api_key_env.clone(),
                yaml_inline_api_key,
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
            let api_key_env = behavior
                .api_key_env
                .clone()
                .or_else(|| {
                    config
                        .secrets
                        .as_ref()
                        .and_then(|v| v.openai.as_ref())
                        .and_then(|v| v.api_key_env.clone())
                })
                .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
            let yaml_inline_api_key = extract_openai_api_key_from_effective_config(config)
                .filter(|v| v != "***REDACTED***");
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
                api_key_env,
                yaml_inline_api_key,
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
            api_key_env: Some(openai.api_key_env.clone()),
            api_key: openai.api_key.clone(),
            openai: openai.openai.clone(),
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
        }),
        secrets: None,
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

fn extract_openai_api_key_from_config(config: &Value) -> Option<String> {
    config
        .get("secrets")
        .and_then(|v| v.get("openai"))
        .and_then(|v| v.get("api_key"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            config
                .get("behavior")
                .and_then(|v| v.get("openai"))
                .and_then(|v| v.get("api_key"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            config
                .get("behavior")
                .and_then(|v| v.get("api_key"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn extract_openai_api_key_from_effective_config(
    config: &EffectiveConfigDocument,
) -> Option<String> {
    config
        .secrets
        .as_ref()
        .and_then(|v| v.openai.as_ref())
        .and_then(|v| v.api_key.clone())
        .or_else(|| {
            config
                .behavior
                .openai
                .as_ref()
                .and_then(|v| v.api_key.clone())
        })
        .or_else(|| config.behavior.api_key.clone())
}

fn strip_openai_api_key_from_effective_config(config: &mut EffectiveConfigDocument) {
    if let Some(openai) = config.behavior.openai.as_mut() {
        openai.api_key = None;
    }
    config.behavior.api_key = None;
    if let Some(secrets) = config.secrets.as_mut() {
        if let Some(openai) = secrets.openai.as_mut() {
            openai.api_key = None;
        }
    }
}

fn persist_local_openai_api_key(
    node_name: &str,
    api_key: &str,
    options: &NodeSecretWriteOptions,
) -> Result<(), fluxbee_sdk::NodeSecretError> {
    persist_local_openai_api_key_with_root(node_name, None::<&std::path::Path>, api_key, options)
}

fn persist_local_openai_api_key_with_root(
    node_name: &str,
    root: Option<&std::path::Path>,
    api_key: &str,
    options: &NodeSecretWriteOptions,
) -> Result<(), fluxbee_sdk::NodeSecretError> {
    let mut secrets = match root {
        Some(value) => load_node_secret_record_with_root(node_name, value.to_path_buf()),
        None => load_node_secret_record(node_name),
    }
    .map(|record| record.secrets)
    .unwrap_or_else(|_| serde_json::Map::new());
    secrets.insert(
        AI_LOCAL_SECRET_KEY_OPENAI.to_string(),
        Value::String(api_key.to_string()),
    );
    let record = build_node_secret_record(secrets, options);
    match root {
        Some(value) => {
            save_node_secret_record_with_root(node_name, value.to_path_buf(), &record)?;
        }
        None => {
            save_node_secret_record(node_name, &record)?;
        }
    }
    Ok(())
}

fn load_local_openai_api_key(node_name: &str) -> Option<String> {
    load_local_openai_api_key_with_root(node_name, None::<&std::path::Path>)
}

fn load_local_openai_api_key_with_root(
    node_name: &str,
    root: Option<&std::path::Path>,
) -> Option<String> {
    let record = match root {
        Some(value) => load_node_secret_record_with_root(node_name, value.to_path_buf()).ok(),
        None => load_node_secret_record(node_name).ok(),
    };
    record
        .and_then(|record| record.secrets.get(AI_LOCAL_SECRET_KEY_OPENAI).cloned())
        .and_then(|value| value.as_str().map(ToString::to_string))
        .filter(|value| !value.trim().is_empty() && value != NODE_SECRET_REDACTION_TOKEN)
}

fn build_secret_write_options_from_message(msg: &Message) -> NodeSecretWriteOptions {
    let updated_by_label = msg
        .payload
        .get("requested_by")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| msg.meta.action.clone());
    NodeSecretWriteOptions {
        updated_by_ilk: msg.meta.src_ilk.clone(),
        updated_by_label,
        trace_id: Some(msg.routing.trace_id.clone()),
    }
}

fn migrate_bootstrap_openai_secret(
    node_name: &str,
    config: &mut EffectiveConfigDocument,
) -> Result<bool, fluxbee_sdk::NodeSecretError> {
    migrate_bootstrap_openai_secret_with_root(node_name, None::<&std::path::Path>, config)
}

fn migrate_bootstrap_openai_secret_with_root(
    node_name: &str,
    root: Option<&std::path::Path>,
    config: &mut EffectiveConfigDocument,
) -> Result<bool, fluxbee_sdk::NodeSecretError> {
    let Some(api_key) = extract_openai_api_key_from_effective_config(config)
        .filter(|value| !value.trim().is_empty() && value != NODE_SECRET_REDACTION_TOKEN)
    else {
        return Ok(false);
    };
    if load_local_openai_api_key_with_root(node_name, root).is_none() {
        let options = NodeSecretWriteOptions {
            updated_by_ilk: None,
            updated_by_label: Some("bootstrap_migration".to_string()),
            trace_id: None,
        };
        persist_local_openai_api_key_with_root(node_name, root, &api_key, &options)?;
    }
    strip_openai_api_key_from_effective_config(config);
    Ok(true)
}

fn resolve_openai_api_key_source_from_effective_config(
    node_name: &str,
    config: &Value,
) -> OpenAiApiKeySource {
    if load_local_openai_api_key(node_name).is_some() {
        return OpenAiApiKeySource::LocalFile;
    }
    if extract_openai_api_key_from_config(config)
        .filter(|value| !value.trim().is_empty() && value != NODE_SECRET_REDACTION_TOKEN)
        .is_some()
    {
        return OpenAiApiKeySource::ControlPlaneLegacy;
    }
    let api_key_env = config
        .get("behavior")
        .and_then(|value| value.get("api_key_env"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("OPENAI_API_KEY");
    if std::env::var(api_key_env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some()
    {
        return OpenAiApiKeySource::EnvLegacy;
    }
    OpenAiApiKeySource::Missing
}

fn parse_effective_config_doc(
    config: &Value,
) -> Result<EffectiveConfigDocument, Box<dyn std::error::Error + Send + Sync>> {
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
        if config.behavior.api_key_env.is_none() {
            let inherited = config
                .secrets
                .as_ref()
                .and_then(|v| v.openai.as_ref())
                .and_then(|v| v.api_key_env.clone());
            config.behavior.api_key_env =
                Some(inherited.unwrap_or_else(|| default_openai_api_key_env()));
        }
        if config.secrets.is_none() {
            config.secrets = Some(EffectiveSecretsSection::default());
        }
        if let Some(secrets) = config.secrets.as_mut() {
            if secrets.openai.is_none() {
                secrets.openai = Some(EffectiveOpenAiSecrets::default());
            }
            if let Some(openai_secrets) = secrets.openai.as_mut() {
                if openai_secrets.api_key.is_none() {
                    openai_secrets.api_key = config
                        .behavior
                        .openai
                        .as_ref()
                        .and_then(|v| v.api_key.clone())
                        .or_else(|| config.behavior.api_key.clone());
                }
            }
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
        "message": "Missing OpenAI API key in local secrets.json, CONFIG_SET override, YAML inline, or env.",
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
                dst: Destination::Unicast("AI.frontdesk.gov@motherbee".to_string()),
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
            mode: RunnerMode::Gov,
            node_name: "AI.frontdesk.gov".to_string(),
            behavior: Arc::new(RwLock::new(None)),
            dynamic_config_dir: PathBuf::from("/tmp"),
            thread_state_store: None,
            immediate_memory_store: None,
            gov_identity,
            gov_identity_bridge: None,
            control_plane: Arc::new(RwLock::new(ControlPlaneState {
                current_state: NodeLifecycleState::Unconfigured,
                config_source: "none",
                effective_config: None,
                schema_version: 0,
                config_version: 0,
            })),
        }
    }

    fn temp_secret_root(test_name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fluxbee-ai-secret-tests-{}-{}",
            test_name,
            Uuid::new_v4()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp root");
        path
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
        let mut node = test_node();
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
                api_key_env: "OPENAI_API_KEY_MISSING_FOR_TEST".to_string(),
                yaml_inline_api_key: None,
                base_url: None,
                immediate_memory: ImmediateMemorySection::default(),
                multimodal: false,
            }));
        }

        let msg = sample_user_request_with_context(
            json!({ "thread_id": "sim-thread-1" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
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
                dst: Destination::Unicast("AI.frontdesk.gov@motherbee".to_string()),
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

    #[test]
    fn frontdesk_registry_exposes_ilk_register_by_default() {
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
        assert!(names.iter().any(|name| name == "ilk_register"));
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
    fn strip_openai_secret_removes_inline_secret_fields() {
        let mut config = EffectiveConfigDocument {
            behavior: EffectiveBehaviorSection {
                kind: "openai_chat".to_string(),
                api_key: Some("legacy-top-level".to_string()),
                openai: Some(OpenAiCredentialsSection {
                    api_key: Some("legacy-nested".to_string()),
                }),
                ..EffectiveBehaviorSection::default()
            },
            secrets: Some(EffectiveSecretsSection {
                openai: Some(EffectiveOpenAiSecrets {
                    api_key: Some("canonical-secret".to_string()),
                    api_key_env: Some("OPENAI_API_KEY".to_string()),
                }),
            }),
            ..EffectiveConfigDocument::default()
        };

        strip_openai_api_key_from_effective_config(&mut config);

        assert_eq!(config.behavior.api_key, None);
        assert_eq!(
            config
                .behavior
                .openai
                .as_ref()
                .and_then(|value| value.api_key.as_ref()),
            None
        );
        assert_eq!(
            config
                .secrets
                .as_ref()
                .and_then(|value| value.openai.as_ref())
                .and_then(|value| value.api_key.as_ref()),
            None
        );
        assert_eq!(
            config
                .secrets
                .as_ref()
                .and_then(|value| value.openai.as_ref())
                .and_then(|value| value.api_key_env.as_deref()),
            Some("OPENAI_API_KEY")
        );
    }

    #[test]
    fn build_secret_write_options_uses_requested_by_and_trace_id() {
        let msg = Message {
            routing: Routing {
                src: "SY.admin@motherbee".to_string(),
                dst: Destination::Unicast("AI.chat@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-config-123".to_string(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some("CONFIG_SET".to_string()),
                src_ilk: Some("ilk:123".to_string()),
                scope: None,
                target: Some("node_config_control".to_string()),
                action: Some("CONFIG_SET".to_string()),
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload: json!({
                "requested_by": "archi",
                "config": {
                    "secrets": {
                        "openai": {
                            "api_key": "sk-test"
                        }
                    }
                }
            }),
        };

        let options = build_secret_write_options_from_message(&msg);

        assert_eq!(options.updated_by_ilk.as_deref(), Some("ilk:123"));
        assert_eq!(options.updated_by_label.as_deref(), Some("archi"));
        assert_eq!(options.trace_id.as_deref(), Some("trace-config-123"));
    }

    #[test]
    fn local_openai_secret_roundtrip_survives_reload_with_root() {
        let root = temp_secret_root("roundtrip");
        let node_name = "AI.chat@motherbee";
        let options = NodeSecretWriteOptions {
            updated_by_ilk: Some("ilk:test".to_string()),
            updated_by_label: Some("archi".to_string()),
            trace_id: Some("trace-secret-1".to_string()),
        };

        persist_local_openai_api_key_with_root(
            node_name,
            Some(root.as_path()),
            "sk-test-roundtrip",
            &options,
        )
        .expect("persist local secret");

        let loaded = load_local_openai_api_key_with_root(node_name, Some(root.as_path()));
        assert_eq!(loaded.as_deref(), Some("sk-test-roundtrip"));

        let secret_record = load_node_secret_record_with_root(node_name, root.clone())
            .expect("load redacted secret record");
        assert_eq!(secret_record.updated_by_label.as_deref(), Some("archi"));
        assert_eq!(
            secret_record.secrets.get(AI_LOCAL_SECRET_KEY_OPENAI),
            Some(&Value::String("sk-test-roundtrip".to_string()))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn migrate_bootstrap_secret_persists_local_file_and_strips_inline_secret() {
        let root = temp_secret_root("migrate");
        let node_name = "AI.chat@motherbee";
        let mut config = EffectiveConfigDocument {
            behavior: EffectiveBehaviorSection {
                kind: "openai_chat".to_string(),
                api_key: Some("legacy-top-level".to_string()),
                openai: Some(OpenAiCredentialsSection {
                    api_key: Some("legacy-nested".to_string()),
                }),
                ..EffectiveBehaviorSection::default()
            },
            secrets: Some(EffectiveSecretsSection {
                openai: Some(EffectiveOpenAiSecrets {
                    api_key: Some("canonical-secret".to_string()),
                    api_key_env: Some("OPENAI_API_KEY".to_string()),
                }),
            }),
            ..EffectiveConfigDocument::default()
        };

        let changed =
            migrate_bootstrap_openai_secret_with_root(node_name, Some(root.as_path()), &mut config)
                .expect("migrate bootstrap secret");

        assert!(changed);
        assert_eq!(
            load_local_openai_api_key_with_root(node_name, Some(root.as_path())).as_deref(),
            Some("canonical-secret")
        );
        assert_eq!(extract_openai_api_key_from_effective_config(&config), None);

        let value = serde_json::to_value(&config).expect("serialize config");
        let serialized = serde_json::to_string(&value).expect("serialize config json");
        assert!(!serialized.contains("canonical-secret"));
        let redacted = redact_secrets(&value);
        let redacted_json = serde_json::to_string(&redacted).expect("serialize redacted config");
        assert!(!redacted_json.contains("canonical-secret"));
        let source = resolve_openai_api_key_source_from_effective_config(node_name, &value);
        assert_eq!(source, OpenAiApiKeySource::Missing);

        let _ = fs::remove_dir_all(root);
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
