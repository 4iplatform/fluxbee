use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use fluxbee_ai_sdk::{
    build_ai_behavior_response, build_model_user_content_parts,
    build_output_schema_fallback_instruction, build_reply_message_runtime_src,
    create_function_calling_model, create_llm_client, extract_text,
    resolve_model_input_from_payload_with_options, resolve_response_envelope_output_schema,
    AiAccount, AiBehaviorOutput, AiFinalOutput, AiNode, AiProvider, AiUserArtifact,
    FunctionCallingConfig, FunctionCallingRunner, FunctionLoopItem, FunctionLoopRunResult,
    FunctionRunInput, FunctionTool, FunctionToolDefinition, FunctionToolProvider,
    FunctionToolRegistry, HiveAiConfig, ImmediateConversationMemory, LanceDbThreadStateStore,
    Message, ModelInputOptions, ModelSettings, NodeRuntime, ResolvedModelInput, RuntimeConfig,
    TenantSource, ThreadStateStore, ThreadStateToolsProvider, ToolRunWatch, UsageRecorder,
};
use fluxbee_sdk::identity::{find_ilk_by_handler_node_from_hive_config, IdentityIlkOption};
use fluxbee_sdk::managed_control_plane::{
    bootstrap_managed_control_plane, build_node_status_response_payload,
    persist_effective_config_with_root, ContractError, ControlPlaneErrorInfo,
    ManagedControlPlaneState, ManagedNodeConfigContract, ManagedNodeLifecycleState,
    DEFAULT_MANAGED_NODES_ROOT,
};
use fluxbee_sdk::protocol::{MemoryPackage, Meta};
use fluxbee_sdk::{
    managed_node_instance_dir, managed_node_name, AdminCommandRequest, NodeConfig, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, VaultCallerOwned,
    VaultClient,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs as tokio_fs;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tracing_subscriber::EnvFilter;

const MSG_NODE_STATUS_GET: &str = "NODE_STATUS_GET";
const MSG_NODE_STATUS_GET_RESPONSE: &str = "NODE_STATUS_GET_RESPONSE";
const IMMEDIATE_INTERACTION_MAX_CHARS: usize = 1_200;
const AI_RUNTIME_KIND: &str = "ai.generic";
const DEFAULT_AGENT_ASSET_BLOB_ROOT: &str = "/var/lib/fluxbee/blob";
const AGENT_ASSET_MAX_BYTES: u64 = 256 * 1024;
const COMPOSED_PROMPT_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_COGNITIVE_POLL_INTERVAL_SECS: u64 = 10;
/// The prompt of an agent with no definition: its ILK has no hashes, none of its assets loaded, or
/// the node has not found its agent ILK (yet). The model then only answers that it is unconfigured.
const DEFAULT_UNCONFIGURED_AGENT_PROMPT: &str = r#"You are an AI agent that has not yet received its operational configuration.

If you receive a message, respond with this exact structure:
{
  "status": "unconfigured",
  "message": "This agent has not been configured yet. Please contact the system administrator to assign a role, skills, and handbooks."
}

Do not interpret messages beyond confirming receipt.
Do not perform any action."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiApiKeySource {
    Vault,
    Missing,
}

impl AiApiKeySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Vault => "vault",
            Self::Missing => "missing",
        }
    }
}

/// Where the node runs: its name, router socket and directories, from the orchestrator's
/// environment (or the bootstrap arguments of a manual run).
#[derive(Debug)]
struct NodeSection {
    name: String,
    version: String,
    router_socket: String,
    uuid_persistence_dir: String,
    config_dir: String,
    dynamic_config_dir: String,
}

/// `runtime.immediate_memory`: applied with each `CONFIG_SET`, like `behavior`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ImmediateMemorySection {
    enabled: bool,
    recent_interactions_max: usize,
    summary_max_chars: usize,
}

/// `runtime.cognitive_definition`: read when the node starts, so a change applies at the next
/// start.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CognitiveDefinitionSection {
    poll_interval_secs: u64,
    #[serde(default)]
    blob_root: Option<String>,
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
#[serde(deny_unknown_fields)]
struct EffectiveBehaviorSection {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_settings: Option<RunnerModelSettings>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    capabilities: Option<BehaviorCapabilities>,
}

/// The `runtime` the node reads. Timeouts, worker pool and queue are the AI runtime's own
/// (`RuntimeConfig::default()`), not config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct EffectiveRuntimeSection {
    #[serde(default)]
    immediate_memory: Option<ImmediateMemorySection>,
    #[serde(default)]
    cognitive_definition: Option<CognitiveDefinitionSection>,
}

impl Default for ImmediateMemorySection {
    fn default() -> Self {
        Self {
            enabled: false,
            recent_interactions_max: 10,
            summary_max_chars: 1_600,
        }
    }
}

impl Default for CognitiveDefinitionSection {
    fn default() -> Self {
        Self {
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

fn default_multimodal_for_runtime() -> bool {
    true
}

#[derive(Debug, Clone)]
enum NodeBehavior {
    Echo,
    AiChat(AiChatRuntime),
}

#[derive(Debug, Clone)]
struct AiChatRuntime {
    model: String,
    model_settings: ModelSettings,
    base_url: Option<String>,
    immediate_memory: ImmediateMemorySection,
    multimodal: bool,
}

#[derive(Clone)]
struct ResolvedAiCredential {
    provider: AiProvider,
    api_key: String,
}

// Manual Debug so a future `{:?}` on this struct can never leak the live provider api_key
// (audit B5 — the derived Debug printed it verbatim; mirrors io-api's PublicationRuntime).
impl std::fmt::Debug for ResolvedAiCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAiCredential")
            .field("provider", &self.provider)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

struct GenericAiNode {
    node_name: String,
    /// Self ILK, read from `FLUXBEE_NODE_ILK_ID` env injected by orchestrator
    /// at spawn (after `ILK_REGISTER` to SY.identity). `None` only when the
    /// node was started manually outside the orchestrator pipeline; in that
    /// case vault and any identity-bearing call will be rejected by auth.
    self_ilk_id: Option<String>,
    /// Self tenant, read from `FLUXBEE_NODE_TENANT_ID` env injected by the orchestrator at
    /// spawn. The AI key is the one SY.vault gives this node: owned by it, assigned to it, or its
    /// tenant's general one (docs/ilk-tenant-model-v1.md §2.2).
    self_tenant_id: Option<String>,
    behavior: Arc<RwLock<Option<NodeBehavior>>>,
    config_dir: PathBuf,
    thread_state_store: Option<Arc<dyn ThreadStateStore>>,
    immediate_memory_store: Option<Arc<ImmediateMemoryStore>>,
    /// Vault accessor over the canonical `Arc<RouterDispatcher>`. `None` when
    /// `self_ilk_id` / hive suffix is missing — the node still boots in a
    /// degraded state.
    vault: Option<VaultClient>,
    /// Where each model call's usage record goes (docs/router-stream-v1.md §7.2).
    usage: UsageRecorder,
    control_plane: Arc<RwLock<ControlPlaneState>>,
    cognitive_definition: Arc<RwLock<CognitiveDefinitionRuntimeState>>,
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

/// Publishes an HTML page as a public, read-only URL. Unlike the `generate_*` tools (which
/// only produce a downloadable artifact), this one ACTS: it writes the bytes to the blob
/// store and drives `SY.admin`'s `publish_artifact` over the mesh socket, returning the
/// capability URL. Only registered when the node has a resolved identity (`vault` present),
/// because publishing is tenant-scoped by the producer's router-stamped identity (admin-side).
struct PublishHtmlTool {
    vault: VaultClient,
}

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

#[derive(Debug, Default)]
struct BehaviorContext {
    thread_id: Option<String>,
    src_ilk: Option<String>,
    trace_id: Option<String>,
    /// Whether a tool has run in this message's tool loop (`ToolRunWatch`): a failure after that
    /// is never answered as retryable, since sending the message again would run the tool again.
    tool_ran: AtomicBool,
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

/// ai.generic's config-plane lifecycle IS the SDK's canonical one — no local re-definition, so it
/// cannot drift from the shared control-plane.
type NodeLifecycleState = ManagedNodeLifecycleState;

#[derive(Debug)]
struct ControlPlaneState {
    current_state: NodeLifecycleState,
    config_source: &'static str,
    effective_config: Option<Value>,
    schema_version: u32,
    config_version: u64,
    /// Why the config is not in use: the rejection of the `config.json` it booted with
    /// (FAILED_CONFIG). A valid `CONFIG_SET` clears it.
    last_error: Option<ControlPlaneErrorInfo>,
}

impl Default for ControlPlaneState {
    fn default() -> Self {
        Self {
            current_state: NodeLifecycleState::Unconfigured,
            config_source: "none",
            effective_config: None,
            schema_version: 0,
            config_version: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone)]
struct CognitiveDefinitionRuntimeConfig {
    poll_interval: Duration,
    blob_root: PathBuf,
}

impl From<CognitiveDefinitionSection> for CognitiveDefinitionRuntimeConfig {
    fn from(value: CognitiveDefinitionSection) -> Self {
        let poll_secs = value.poll_interval_secs.max(1).min(300);
        Self {
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
    /// Why the last poll learned nothing (the identity SHM or an asset could not be read). The
    /// definition above is still the last one resolved; the next poll tries again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_poll_error: Option<String>,
    #[serde(skip)]
    last_hashes: CognitiveDefinitionHashes,
    /// The system prompt every model call runs with. There is always one: the composed definition,
    /// or the fixed "unconfigured" prompt while the agent has none.
    #[serde(skip)]
    active_prompt: String,
}

impl CognitiveDefinitionRuntimeState {
    /// The agent's ILK is not found (yet): not in the identity SHM, not an `agent` ILK, the SHM is
    /// unreadable, or the first poll has not run. It runs the "unconfigured" prompt.
    fn unresolved() -> Self {
        Self {
            definition_state: "unresolved".to_string(),
            ilk_id: None,
            role_hash_loaded: None,
            skill_hashes_loaded: Vec::new(),
            handbook_hashes_loaded: Vec::new(),
            personality_hash_loaded: None,
            failed_hashes: Vec::new(),
            prompt_truncated: false,
            active_prompt_chars: DEFAULT_UNCONFIGURED_AGENT_PROMPT.chars().count(),
            last_recompose_at: None,
            last_identity_seq: None,
            last_poll_error: None,
            last_hashes: CognitiveDefinitionHashes::default(),
            active_prompt: DEFAULT_UNCONFIGURED_AGENT_PROMPT.to_string(),
        }
    }

    fn has_failures(&self) -> bool {
        !self.failed_hashes.is_empty()
    }

    /// Whether the agent has a definition from its ILK (`composed`, `partial`, `error` or
    /// `empty`). From then on only a definite new answer replaces it (`apply_identity_lookup`).
    fn is_resolved(&self) -> bool {
        self.last_identity_seq.is_some()
    }
}

fn should_reuse_cognitive_state(
    current: &CognitiveDefinitionRuntimeState,
    ilk_id: &str,
    hashes: &CognitiveDefinitionHashes,
) -> bool {
    current.last_identity_seq.is_some()
        && current.ilk_id.as_deref() == Some(ilk_id)
        && current.last_hashes == *hashes
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
            trace_id: Some(msg.routing.trace_id.clone()),
            tool_ran: AtomicBool::new(false),
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

        let (input, resolved_user_input): (String, Option<ResolvedModelInput>) =
            if msg.meta.msg_type.eq_ignore_ascii_case("user") {
                let options = ModelInputOptions {
                    multimodal: matches!(&behavior, NodeBehavior::AiChat(ai) if ai.multimodal),
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
        // What the person wrote, without the memory block in front: what the history keeps.
        let user_text = input.clone();
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
            NodeBehavior::AiChat(ai) => {
                let input_parts = if ai.multimodal {
                    if let Some(resolved) = resolved_user_input.as_ref() {
                        match build_model_user_content_parts(resolved).await {
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
                    .run_ai_chat(ai, input, input_parts, &user_text, &behavior_ctx, &msg.meta)
                    .await
                {
                    Ok(output) => output,
                    Err(err) if err.to_string().contains("missing AI api key") => {
                        tracing::warn!(
                            node_name = %self.node_name,
                            trace_id = %msg.routing.trace_id,
                            error = %err,
                            "AI runtime missing api key; replying with runtime-not-ready payload"
                        );
                        let payload = missing_ai_api_key_payload(&err.to_string());
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
                                    model = %ai.model,
                                    provider_status = status,
                                    provider_param = ?extract_openai_error_param(msg_text),
                                    provider_detail = %trim_chars(&detail, 280),
                                    attachment_count = attachment_summary.count,
                                    attachment_total_bytes = attachment_summary.total_bytes,
                                    attachment_mimes = ?attachment_summary.mimes,
                                    error = %err,
                                    "AI runtime request failed with structured provider status; replying with provider error payload"
                                );
                            } else {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    model = %ai.model,
                                    attachment_count = attachment_summary.count,
                                    attachment_total_bytes = attachment_summary.total_bytes,
                                    attachment_mimes = ?attachment_summary.mimes,
                                    error = %err,
                                    "AI runtime request failed; replying with provider error payload"
                                );
                            }
                        } else {
                            tracing::warn!(
                                node_name = %self.node_name,
                                trace_id = %msg.routing.trace_id,
                                model = %ai.model,
                                attachment_count = attachment_summary.count,
                                attachment_total_bytes = attachment_summary.total_bytes,
                                attachment_mimes = ?attachment_summary.mimes,
                                error = %err,
                                "AI runtime request failed; replying with provider error payload"
                            );
                        }
                        let payload = ai_chat_failure_payload(
                            &err,
                            behavior_ctx.tool_ran.load(Ordering::Relaxed),
                        );
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
    async fn run_ai_chat(
        &self,
        ai: &AiChatRuntime,
        input: String,
        input_parts: Option<Vec<fluxbee_ai_sdk::ModelContentPart>>,
        user_text: &str,
        ctx: &BehaviorContext,
        meta: &Meta,
    ) -> fluxbee_ai_sdk::Result<AiBehaviorOutput> {
        let credential = self
            .resolve_ai_credential()
            .await
            .map_err(fluxbee_ai_sdk::errors::AiSdkError::Protocol)?;
        let account = self.usage_account(ctx);
        let client = create_llm_client(
            credential.provider,
            credential.api_key.clone(),
            ai.base_url.clone(),
            account.clone(),
        );
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
            provider = %credential.provider,
            "AI chat request prepared"
        );
        if !tool_registry.definitions().is_empty() {
            let prompt = self.agent_prompt().await;
            let system = match &output_schema {
                Some(schema) => format!(
                    "{prompt}\n\n{}",
                    build_output_schema_fallback_instruction(schema)?
                ),
                None => prompt,
            };
            let model = create_function_calling_model(
                credential.provider,
                credential.api_key.clone(),
                ai.base_url.clone(),
                ai.model.clone(),
                Some(system),
                ai.model_settings.clone(),
                account,
            );
            let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
            let immediate_memory = self.load_immediate_memory_for_input(ai, ctx).await;
            let run_input = self.build_function_run_input(
                input.clone(),
                input_parts.clone(),
                ctx,
                ai,
                immediate_memory,
            );
            let watched = ToolRunWatch::new(model.as_ref(), &ctx.tool_ran);
            let result = runner
                .run_with_input(&watched, &tool_registry, run_input)
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
                self.persist_immediate_turn(ai, ctx, user_text, &summary_text)
                    .await;
                return Ok(output);
            }
            if let Some(text) = result.final_assistant_text {
                self.persist_immediate_turn(ai, ctx, user_text, &text).await;
                return Ok(AiBehaviorOutput::text(text));
            }
        }

        let req = fluxbee_ai_sdk::llm::LlmRequest {
            model: ai.model.clone(),
            system: Some(self.agent_prompt().await),
            input,
            input_parts,
            output_schema,
            max_output_tokens: None,
            model_settings: Some(ai.model_settings.clone()),
        };
        let response = fluxbee_ai_sdk::llm::LlmClient::generate(client.as_ref(), req).await?;
        self.persist_immediate_turn(ai, ctx, user_text, &response.content)
            .await;
        Ok(AiBehaviorOutput::text(response.content))
    }

    /// Its own tenant pays for its calls (docs/router-stream-v1.md §7.3); the root tenant, as
    /// `fallback_root`, only for a node started without `FLUXBEE_NODE_TENANT_ID`.
    fn usage_account(&self, ctx: &BehaviorContext) -> AiAccount {
        AiAccount::charge(
            self.usage.clone(),
            &self.node_name,
            self.self_tenant_id.clone(),
            TenantSource::Node,
        )
        .with_node_ilk(self.self_ilk_id.clone())
        .with_counterpart(ctx.src_ilk.clone())
        .with_trace(ctx.trace_id.clone())
        .with_thread(ctx.thread_id.clone())
    }

    /// The agent's system prompt: its composed definition, or the fixed "unconfigured" prompt
    /// while it has none. Never absent, so a model call never runs without one.
    async fn agent_prompt(&self) -> String {
        self.cognitive_definition.read().await.active_prompt.clone()
    }

    fn build_function_run_input(
        &self,
        input: String,
        input_parts: Option<Vec<fluxbee_ai_sdk::ModelContentPart>>,
        ctx: &BehaviorContext,
        ai: &AiChatRuntime,
        immediate_memory: Option<ImmediateConversationMemory>,
    ) -> FunctionRunInput {
        if !ai.immediate_memory.enabled {
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
        ai: &AiChatRuntime,
        ctx: &BehaviorContext,
    ) -> Option<ImmediateConversationMemory> {
        if !ai.immediate_memory.enabled {
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
                .map(|summary| trim_summary(summary, ai.immediate_memory.summary_max_chars));
            record.recent_interactions = prune_recent_interactions(
                record.recent_interactions,
                ai.immediate_memory.recent_interactions_max,
            );
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                memory_hit = true,
                recent_interactions = record.recent_interactions.len(),
                recent_interactions_max = ai.immediate_memory.recent_interactions_max,
                summary_max_chars = ai.immediate_memory.summary_max_chars,
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
                recent_interactions_max = ai.immediate_memory.recent_interactions_max,
                summary_max_chars = ai.immediate_memory.summary_max_chars,
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
        ai: &AiChatRuntime,
        ctx: &BehaviorContext,
        user_input: &str,
        assistant_output: &str,
    ) {
        if !ai.immediate_memory.enabled {
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
            .map(|summary| trim_summary(summary, ai.immediate_memory.summary_max_chars));
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
            ai.immediate_memory.recent_interactions_max,
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
                recent_interactions_max = ai.immediate_memory.recent_interactions_max,
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
        // Publishing ACTION: expose it only when the node has a resolved identity (vault).
        // Publishing is tenant-scoped by the producer's router-stamped identity (resolved
        // admin-side), and vault is Some exactly when that identity is present — so a node
        // without identity simply has no publish tool (no dead/unreachable action).
        if let Some(vault) = self.vault.as_ref() {
            registry.register(Arc::new(PublishHtmlTool {
                vault: vault.clone(),
            }))?;
        }
        Ok(())
    }

    /// The AI key comes from SY.vault by resource type, like every other AI consumer: the
    /// provider is the hive's (hive.yaml `ai`, OpenAI when absent) and the key is one this node
    /// owns, one assigned to it, or its tenant's general one (docs/ilk-tenant-model-v1.md §2.2).
    /// The root tenant's keys are the core's. Nodes do not name a Vault key.
    async fn resolve_ai_credential(&self) -> std::result::Result<ResolvedAiCredential, String> {
        let Some(vault) = self.vault.as_ref() else {
            return Err(
                "missing AI api key: no vault client (the node has no self ILK)".to_string(),
            );
        };
        let provider = hive_ai_provider(&self.config_dir)?;
        let tenant = self
            .self_tenant_id
            .as_deref()
            .unwrap_or(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID);
        match vault
            .resolve_resource(provider.resource_type(), tenant, Duration::from_secs(5))
            .await
        {
            Ok(Some(value)) => match extract_ai_api_key_from_value(&value) {
                Some(api_key) => Ok(ResolvedAiCredential { provider, api_key }),
                None => Err(format!(
                    "missing AI api key: the {provider} secret in SY.vault has no api_key"
                )),
            },
            Ok(None) => Err(format!(
                "missing AI api key: SY.vault has no {provider} secret for this node (owned, assigned, or tenant {tenant}'s general one)"
            )),
            Err(err) => Err(format!("missing AI api key: SY.vault lookup failed: {err}")),
        }
    }

    async fn handle_control_plane(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        let Some(command) = msg.meta.msg.as_deref() else {
            return Ok(None);
        };
        let (response_msg, response_payload) = if command.eq_ignore_ascii_case(MSG_NODE_STATUS_GET)
        {
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
        if let Some(field) = find_ai_secret_contract_field(&config) {
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
        let prev_error = state.last_error.take();

        state.current_state = NodeLifecycleState::Configured;
        state.config_source = "persisted";
        state.effective_config = Some(materialized_config);
        state.schema_version = schema_version;
        state.config_version = config_version;
        // Single-config model: persist back to the node-dir config.json (SDK-owned, preserves the
        // orchestrator's _system). No state/ai-nodes file, so a kill/respawn can't leave a stale one
        // to shadow the fresh config.
        if let Err(err) = persist_effective_config_with_root(
            &self.node_name,
            state.config_version,
            state
                .effective_config
                .as_ref()
                .expect("effective_config was just set to Some above"),
            std::path::Path::new(DEFAULT_MANAGED_NODES_ROOT),
        ) {
            state.current_state = prev_state;
            state.config_source = prev_source;
            state.effective_config = prev_effective;
            state.schema_version = prev_schema;
            state.config_version = prev_version;
            state.last_error = prev_error;
            return self.error_response(
                "config_persist_error",
                format!("Failed to persist config: {err}"),
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
        // Live key/provider validation occurs on every AI request.
        let api_key_source = if self
            .self_ilk_id
            .as_deref()
            .filter(|v| !v.is_empty())
            .is_some()
            && self.vault.is_some()
        {
            AiApiKeySource::Vault
        } else {
            AiApiKeySource::Missing
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
                        "allowed": ["echo", "ai_chat"]
                    },
                    "config.behavior.model": {
                        "examples": ["gpt-5.5", "claude-sonnet-5-5"]
                    }
                },
                "optional_fields": [
                    "config.behavior.model_settings",
                    "config.behavior.base_url",
                    "config.behavior.capabilities.multimodal",
                    "config.runtime.immediate_memory",
                    "config.runtime.cognitive_definition"
                ],
                "resources": [
                    {
                        "resource_type": "openai|anthropic",
                        "required": true,
                        "source": api_key_source.as_str(),
                        "configured": api_key_source != AiApiKeySource::Missing,
                        "provider": "SY.vault"
                    }
                ],
                "notes": [
                    "Cognitive assets are not part of CONFIG_SET. Apply role_hash, skill_hashes, handbook_hashes, and personality_hash with set_ilk_definition against the agent ILK.",
                    "AI credentials are not config: the provider is the hive's (hive.yaml ai, OpenAI by default) and its key is read from SY.vault by resource_type openai|anthropic on each request: one this node owns, one assigned to it, or its tenant's general one.",
                    "config.behavior.vault_key is not accepted: nodes do not name a Vault key.",
                    "CONFIG_SET rejects secret-bearing fields such as config.secrets.openai.api_key, config.behavior.openai.api_key, config.behavior.api_key, and config.behavior.api_key_env.",
                    "ai.generic defaults behavior.capabilities.multimodal=true unless explicitly overridden.",
                    "Cognitive role/skill/handbook prompt definition is loaded from identity SHM and blob://agent-assets/<hash>.json.",
                    "The prompt is the agent ILK's definition; while it has none (no hashes, no asset loaded, ILK not found) the model runs a fixed unconfigured prompt.",
                    "config.runtime.immediate_memory (enabled, recent_interactions_max, summary_max_chars) applies with CONFIG_SET; config.runtime.cognitive_definition (poll_interval_secs, blob_root) applies at the next start."
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
            "last_error": state.last_error
        })
    }

    /// A node that is not CONFIGURED refuses every completion, so it is not healthy: the SDK's
    /// managed answer reports DEGRADED with its `lifecycle_state`, and `last_error` for a rejected
    /// config.
    async fn build_node_status_get_response(&self) -> Value {
        let state = self.control_plane.read().await;
        build_node_status_response_payload(state.current_state, state.last_error.as_ref())
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

fn spawn_cognitive_definition_poll(
    node_name: String,
    config_dir: PathBuf,
    config: CognitiveDefinitionRuntimeConfig,
    state: Arc<RwLock<CognitiveDefinitionRuntimeState>>,
) {
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
    let lookup = find_ilk_by_handler_node_from_hive_config(config_dir, node_name)
        .map_err(|err| format!("identity SHM unavailable: {err}"));
    let assets = AssetsFolder {
        blob_root: &config.blob_root,
    };
    apply_identity_lookup(node_name, &assets, lookup, state).await
}

/// One poll's answer about the agent's ILK. Once the agent has a definition, only a definite new
/// one replaces it: the ILK is there, is an agent, and every asset it names was read (an asset
/// that is on disk but invalid is left out of it). An ILK that is no longer an agent is definite
/// too. Anything else keeps the definition, says why in `last_poll_error` and is returned for the
/// next poll to retry: the identity SHM unreadable, the ILK absent from it (a degraded identity
/// replica rewrites the region with only the system ILKs), an asset missing (on a worker, a new
/// asset arrives some seconds after the ILK names it) or unreadable. Before the agent's first
/// definition, a missing ILK or asset is composed as it is (the "unconfigured" prompt, or a
/// `partial` definition), and an unreadable asset keeps the "unconfigured" prompt.
async fn apply_identity_lookup(
    node_name: &str,
    assets: &dyn AssetSource,
    lookup: Result<Option<(u64, IdentityIlkOption)>, String>,
    state: &Arc<RwLock<CognitiveDefinitionRuntimeState>>,
) -> Result<(), String> {
    let resolved = match lookup {
        Ok(value) => value,
        Err(err) => return keep_cognitive_definition(state, err).await,
    };
    let Some((seq, ilk)) = resolved else {
        let missing = "agent ILK not found for handler_node".to_string();
        if state.read().await.is_resolved() {
            return keep_cognitive_definition(state, missing).await;
        }
        update_cognitive_unresolved(state, missing).await;
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
            current.last_poll_error = None;
            return Ok(());
        }
    }

    let composed = match compose_cognitive_state(assets, seq, &ilk, hashes) {
        Ok(composed) => composed,
        Err(err) => return keep_cognitive_definition(state, err).await,
    };
    if !composed.missing.is_empty() && state.read().await.is_resolved() {
        let waiting = format!(
            "assets not on this hive yet: {}",
            composed.missing.join(", ")
        );
        return keep_cognitive_definition(state, waiting).await;
    }
    let next = composed.state;
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

/// A poll with no definite answer: the definition stays as it is, the reason shows in CONFIG_GET
/// (`last_poll_error`), and the error goes back to the poll loop, which tries again.
async fn keep_cognitive_definition(
    state: &Arc<RwLock<CognitiveDefinitionRuntimeState>>,
    error: String,
) -> Result<(), String> {
    state.write().await.last_poll_error = Some(error.clone());
    Err(error)
}

/// The agent's ILK is not an agent (or, before its first definition, not there): whatever it was
/// composed from no longer applies, so the agent runs the "unconfigured" prompt until a poll finds
/// an agent ILK (and composes it anew, since the failure recorded here rules out reusing a state).
async fn update_cognitive_unresolved(
    state: &Arc<RwLock<CognitiveDefinitionRuntimeState>>,
    message: String,
) {
    let mut unresolved = CognitiveDefinitionRuntimeState::unresolved();
    unresolved.failed_hashes = vec![CognitiveAssetFailure {
        hash: String::new(),
        asset_type: "identity".to_string(),
        error: message,
    }];
    *state.write().await = unresolved;
}

fn hashes_from_identity_ilk(ilk: &IdentityIlkOption) -> CognitiveDefinitionHashes {
    CognitiveDefinitionHashes {
        role_hash: ilk.role_hash.clone(),
        skill_hashes: ilk.skill_hashes.clone(),
        handbook_hashes: ilk.handbook_hashes.clone(),
        personality_hash: ilk.personality_hash.clone(),
    }
}

/// A definition composed from what its assets are, and the assets it names that are not on disk
/// (yet), which `apply_identity_lookup` decides on.
struct ComposedDefinition {
    state: CognitiveDefinitionRuntimeState,
    missing: Vec<String>,
}

/// Composes the definition the ILK names. A missing asset is left out (and listed in `missing`), an
/// invalid one is left out; an unreadable one gives the composition up for this poll (`Err`).
fn compose_cognitive_state(
    assets: &dyn AssetSource,
    seq: u64,
    ilk: &IdentityIlkOption,
    hashes: CognitiveDefinitionHashes,
) -> Result<ComposedDefinition, String> {
    if hashes.is_empty() {
        let state = CognitiveDefinitionRuntimeState {
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
            last_poll_error: None,
            last_hashes: hashes,
            active_prompt: DEFAULT_UNCONFIGURED_AGENT_PROMPT.to_string(),
        };
        return Ok(ComposedDefinition {
            state,
            missing: Vec::new(),
        });
    }

    let mut outcomes = AssetOutcomes::default();
    let role = match hashes.role_hash.as_deref() {
        Some(hash) => take_asset(load_role_asset(assets, hash), hash, "role", &mut outcomes)?,
        None => None,
    };

    let mut skills = Vec::new();
    for hash in &hashes.skill_hashes {
        if let Some(asset) =
            take_asset(load_skill_asset(assets, hash), hash, "skill", &mut outcomes)?
        {
            skills.push(asset);
        }
    }

    let mut handbooks = Vec::new();
    for hash in &hashes.handbook_hashes {
        if let Some(asset) = take_asset(
            load_handbook_asset(assets, hash),
            hash,
            "handbook",
            &mut outcomes,
        )? {
            handbooks.push(asset);
        }
    }

    let personality = match hashes.personality_hash.as_deref() {
        Some(hash) => take_asset(
            load_personality_asset(assets, hash),
            hash,
            "personality",
            &mut outcomes,
        )?,
        None => None,
    };
    let AssetOutcomes { failures, missing } = outcomes;

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

    let state = CognitiveDefinitionRuntimeState {
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
        last_poll_error: None,
        last_hashes: hashes,
        active_prompt: prompt,
    };
    Ok(ComposedDefinition { state, missing })
}

/// Why an agent asset did not load.
#[derive(Debug)]
enum AssetLoadError {
    /// Not on disk (yet): on a worker, a new asset arrives some seconds after the ILK names it.
    Missing(String),
    /// On disk but not a valid asset (not a regular file, too large, wrong hash, invalid JSON or
    /// type, a required field empty): waiting does not change it.
    Invalid(String),
    /// It could not be read this time.
    Unreadable(String),
}

/// What a composition's assets gave besides the loaded ones: every one left out (`failures`) and,
/// among them, those not on disk yet (`missing`).
#[derive(Default)]
struct AssetOutcomes {
    failures: Vec<CognitiveAssetFailure>,
    missing: Vec<String>,
}

/// Sorts one asset's load: loaded, left out (missing or invalid), or the whole composition given
/// up for this poll (unreadable).
fn take_asset<T>(
    loaded: Result<T, AssetLoadError>,
    hash: &str,
    asset_type: &str,
    outcomes: &mut AssetOutcomes,
) -> Result<Option<T>, String> {
    let error = match loaded {
        Ok(asset) => return Ok(Some(asset)),
        Err(AssetLoadError::Unreadable(error)) => {
            return Err(format!(
                "{asset_type} asset {hash} could not be read: {error}"
            ))
        }
        Err(AssetLoadError::Missing(error)) => {
            outcomes.missing.push(format!("{asset_type} {hash}"));
            error
        }
        Err(AssetLoadError::Invalid(error)) => error,
    };
    outcomes.failures.push(CognitiveAssetFailure {
        hash: hash.to_string(),
        asset_type: asset_type.to_string(),
        error,
    });
    Ok(None)
}

/// Where an agent asset's bytes come from: the assets folder (`AssetsFolder`), or a test's
/// stand-in that scripts what each read gives.
trait AssetSource: Sync {
    fn read(&self, hash: &str) -> Result<Vec<u8>, AssetLoadError>;
}

/// `<blob_root>/agent-assets/<hash>.json`, read by `read_agent_asset`.
struct AssetsFolder<'a> {
    blob_root: &'a Path,
}

impl AssetSource for AssetsFolder<'_> {
    fn read(&self, hash: &str) -> Result<Vec<u8>, AssetLoadError> {
        read_agent_asset(
            &self.blob_root.join("agent-assets"),
            &format!("{hash}.json"),
        )
    }
}

fn load_role_asset(
    assets: &dyn AssetSource,
    hash: &str,
) -> Result<LoadedRoleAsset, AssetLoadError> {
    let doc = load_cognitive_asset(assets, hash, "role")?;
    let name = required_asset_string(&doc.name, "role.name").map_err(AssetLoadError::Invalid)?;
    let description =
        required_asset_string(doc.description.as_deref().unwrap_or(""), "role.description")
            .map_err(AssetLoadError::Invalid)?;
    Ok(LoadedRoleAsset {
        hash: hash.to_string(),
        name,
        description,
        tone: doc.tone.filter(|value| !value.trim().is_empty()),
        limits: clean_string_vec(doc.limits),
    })
}

fn load_skill_asset(
    assets: &dyn AssetSource,
    hash: &str,
) -> Result<LoadedSkillAsset, AssetLoadError> {
    let doc = load_cognitive_asset(assets, hash, "skill")?;
    let name = required_asset_string(&doc.name, "skill.name").map_err(AssetLoadError::Invalid)?;
    let instructions = clean_string_vec(doc.instructions);
    if instructions.is_empty() {
        return Err(AssetLoadError::Invalid(
            "skill.instructions must contain at least one item".to_string(),
        ));
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
    assets: &dyn AssetSource,
    hash: &str,
) -> Result<LoadedHandbookAsset, AssetLoadError> {
    let doc = load_cognitive_asset(assets, hash, "handbook")?;
    let name =
        required_asset_string(&doc.name, "handbook.name").map_err(AssetLoadError::Invalid)?;
    if doc.sections.is_empty() {
        return Err(AssetLoadError::Invalid(
            "handbook.sections must contain at least one item".to_string(),
        ));
    }
    Ok(LoadedHandbookAsset {
        hash: hash.to_string(),
        name,
        sections: doc.sections,
    })
}

fn load_personality_asset(
    assets: &dyn AssetSource,
    hash: &str,
) -> Result<LoadedPersonalityAsset, AssetLoadError> {
    let doc = load_cognitive_asset(assets, hash, "personality")?;
    let invalid = |message: &str| AssetLoadError::Invalid(message.to_string());
    let name =
        required_asset_string(&doc.name, "personality.name").map_err(AssetLoadError::Invalid)?;
    let system_fields = doc
        .system_fields
        .ok_or_else(|| invalid("personality.system_fields is required"))?;
    if system_fields.timezone.trim().is_empty() {
        return Err(invalid("personality.system_fields.timezone is required"));
    }
    if system_fields.country_code.trim().is_empty() {
        return Err(invalid(
            "personality.system_fields.country_code is required",
        ));
    }
    if system_fields.primary_language.trim().is_empty() {
        return Err(invalid(
            "personality.system_fields.primary_language is required",
        ));
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
    assets: &dyn AssetSource,
    hash: &str,
    expected_type: &str,
) -> Result<CognitiveAssetDocument, AssetLoadError> {
    validate_hash64(hash).map_err(AssetLoadError::Invalid)?;
    let raw = assets.read(hash)?;
    // The name is the SHA-256 of the bytes SY.architect wrote: anything else is not that asset.
    if fluxbee_sdk::blob::sha256_hex(&raw) != hash.trim().to_ascii_lowercase() {
        return Err(AssetLoadError::Invalid(format!(
            "asset content does not match its hash {hash}"
        )));
    }
    let doc: CognitiveAssetDocument = serde_json::from_slice(&raw)
        .map_err(|err| AssetLoadError::Invalid(format!("asset JSON invalid: {err}")))?;
    if doc.asset_type != expected_type {
        return Err(AssetLoadError::Invalid(format!(
            "asset_type mismatch: expected '{expected_type}', got '{}'",
            doc.asset_type
        )));
    }
    Ok(doc)
}

/// The bytes of `<assets_dir>/<name>`, read without trusting what is under that name: the
/// Syncthing user writes the assets folder (A-72). The name is opened without following a link and
/// without blocking (`O_NOFOLLOW | O_NONBLOCK`), what was opened must be a regular file of at most
/// `AGENT_ASSET_MAX_BYTES`, and no more than that limit plus one byte is ever read. A symlink, a
/// FIFO or a directory planted under the name is an invalid asset, never followed or waited on.
fn read_agent_asset(assets_dir: &Path, name: &str) -> Result<Vec<u8>, AssetLoadError> {
    let path = assets_dir.join(name);
    let not_opened = |err: std::io::Error| classify_unopened_asset(&path, err);
    let dir = std::os::fd::OwnedFd::from(fs::File::open(assets_dir).map_err(not_opened)?);
    let file = fs::File::from(
        fluxbee_sdk::blob::safe_fs::open_file_no_follow(&dir, name).map_err(not_opened)?,
    );
    let meta = file.metadata().map_err(|err| {
        AssetLoadError::Unreadable(format!("cannot stat '{}': {err}", path.display()))
    })?;
    if !meta.file_type().is_file() {
        return Err(AssetLoadError::Invalid(format!(
            "'{}' is not a regular file",
            path.display()
        )));
    }
    if meta.len() > AGENT_ASSET_MAX_BYTES {
        return Err(AssetLoadError::Invalid(format!(
            "asset too large: {} bytes > {}",
            meta.len(),
            AGENT_ASSET_MAX_BYTES
        )));
    }
    let mut raw = Vec::with_capacity(meta.len() as usize);
    file.take(AGENT_ASSET_MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|err| {
            AssetLoadError::Unreadable(format!("asset read failed '{}': {err}", path.display()))
        })?;
    if raw.len() as u64 > AGENT_ASSET_MAX_BYTES {
        return Err(AssetLoadError::Invalid(format!(
            "asset too large: more than {AGENT_ASSET_MAX_BYTES} bytes"
        )));
    }
    Ok(raw)
}

/// Why an asset name could not be opened: missing, a symlink or something that is not a regular
/// file (invalid), or a read error such as a denied permission (unreadable). Looking at the name
/// again only sorts the error; nothing is opened through it.
fn classify_unopened_asset(path: &Path, err: std::io::Error) -> AssetLoadError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return AssetLoadError::Missing(format!("asset missing '{}'", path.display()));
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => AssetLoadError::Invalid(format!(
            "'{}' is a symbolic link; an asset is never read through one",
            path.display()
        )),
        Ok(meta) if !meta.file_type().is_file() => {
            AssetLoadError::Invalid(format!("'{}' is not a regular file", path.display()))
        }
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => {
            AssetLoadError::Missing(format!("asset missing '{}'", path.display()))
        }
        _ => AssetLoadError::Unreadable(format!("asset not readable '{}': {err}", path.display())),
    }
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

/// Soft tool error (returned as an `Ok(Value)` tool result so the model can react, never a
/// hard failure). Mirrors `invalid_tool_artifact_error`'s shape.
fn publish_tool_error(message: &str, retryable: bool) -> Value {
    json!({
        "status": "error",
        "error_code": "publish_failed",
        "message": format!("publish_html_page: {message}"),
        "retryable": retryable
    })
}

#[async_trait]
impl FunctionTool for PublishHtmlTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "publish_html_page".to_string(),
            description: "Publish a self-contained HTML page as a public, read-only URL and \
                return the link. Use ONLY when the user explicitly asks to publish or share a \
                page publicly. The HTML must be self-contained (no external network); the \
                returned URL is a capability link you should give back to the user."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "filename": { "type": "string", "minLength": 1 },
                    "content": {
                        "type": "string",
                        "description": "the full self-contained HTML document"
                    }
                },
                "required": ["filename", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateHtmlArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("publish_html_page", err))?;
        let filename = args.filename.trim();
        // Validate with the same gate as generate_html_artifact (mime / extension / traversal).
        let artifact = match AiUserArtifact::from_html(filename, args.content) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("publish_html_page", err)),
        };
        // Write bytes into the local blob store (staging -> active), mirroring
        // materialize_user_artifacts exactly so there is no divergence. Backend errors can carry
        // filesystem paths, so they are LOGGED, never handed to the model — it gets a coarse reason.
        let blob = match fluxbee_sdk::blob::BlobToolkit::new(
            fluxbee_sdk::blob::BlobConfig::default(),
        ) {
            Ok(blob) => blob,
            Err(err) => {
                tracing::warn!(tool = "publish_html_page", error = %err, "blob store unavailable");
                return Ok(publish_tool_error(
                    "could not open the local blob store",
                    true,
                ));
            }
        };
        let blob_ref = match blob.put_bytes(&artifact.bytes, &artifact.filename, &artifact.mime) {
            Ok(blob_ref) => blob_ref,
            Err(err) => {
                tracing::warn!(tool = "publish_html_page", error = %err, "blob write failed");
                return Ok(publish_tool_error(
                    "could not store the page for publishing",
                    true,
                ));
            }
        };
        if let Err(err) = blob.promote(&blob_ref) {
            tracing::warn!(tool = "publish_html_page", error = %err, "blob promote failed");
            return Ok(publish_tool_error(
                "could not store the page for publishing",
                true,
            ));
        }
        // Publish via SY.admin over the mesh socket. Admin resolves THIS node's tenant from its
        // router-stamped identity (never asserted here), curates the bytes, and pushes the edge row.
        // SY.admin + the io.blob curator are motherbee-local, so the target defaults to
        // SY.admin@motherbee (overridable via AI_ADMIN_TARGET) — NOT this node's own hive, which may
        // be a spoke with no admin/curator (mirrors io-api's admin_target default).
        let admin_target = std::env::var("AI_ADMIN_TARGET")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "SY.admin@motherbee".to_string());
        match self
            .vault
            .dispatcher()
            .send_admin_rpc(AdminCommandRequest {
                admin_target: &admin_target,
                action: "publish_artifact",
                target: None,
                params: json!({ "blob_ref": blob_ref }),
                request_id: None,
                timeout: std::time::Duration::from_secs(120),
            })
            .await
        {
            Ok(result) if result.status == "ok" => {
                // The capability link MUST be an absolute public_url. If admin has no public base URL
                // configured it returns only a host-less relative path — unusable as a link — so we
                // fail soft rather than hand the model a broken URL to give the user.
                let url = result
                    .payload
                    .get("public_url")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let Some(url) = url else {
                    tracing::warn!(
                        tool = "publish_html_page",
                        "admin returned no absolute public_url (public_base_url unset); page stored but unreachable"
                    );
                    return Ok(publish_tool_error(
                        "publishing is not fully configured on this hive (no public URL); the page was not published",
                        false,
                    ));
                };
                tracing::info!(tool = "publish_html_page", url = %url, "published HTML page");
                Ok(json!({
                    "status": "ok",
                    "url": url,
                    "publication_id": result.payload.get("publication_id"),
                    "expires_at": result.payload.get("expires_at"),
                    "note": "Page published. Give this URL to the user in your reply."
                }))
            }
            Ok(result) => {
                // Log the free-form error_detail (it may name internal nodes/paths); hand the model
                // only the coarse, non-sensitive error_code (e.g. UNAUTHORIZED, BLOB_CURATE_FAILED).
                if let Some(detail) = result.error_detail.as_ref() {
                    tracing::warn!(tool = "publish_html_page", detail = %detail, "publish rejected");
                }
                let reason = result
                    .error_code
                    .clone()
                    .filter(|code| !code.is_empty())
                    .map(|code| format!("publish rejected ({code})"))
                    .unwrap_or_else(|| "publish rejected".to_string());
                Ok(publish_tool_error(&reason, false))
            }
            Err(err) => {
                tracing::warn!(tool = "publish_html_page", error = %err, target = %admin_target, "publish request failed");
                Ok(publish_tool_error(
                    "could not reach the publishing service",
                    true,
                ))
            }
        }
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

impl NodeBehavior {
    fn kind(&self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::AiChat(_) => "ai_chat",
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Managed runtime nodes are launched by the orchestrator via transient systemd units that do
    // NOT set RUST_LOG, and EnvFilter::from_default_env() with an unset RUST_LOG emits NOTHING — so
    // ai.generic was silent in journald (no config-rejection reason, no completions, no errors),
    // which turned every misconfiguration into a blind debug. Default to INFO like the sibling nodes
    // (io-api / sy-admin / sy-orchestrator); RUST_LOG still overrides when set.
    fluxbee_sdk::logging::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // SY.orchestrator launches the node with no arguments; its name comes from FLUXBEE_NODE_NAME
    // and its config from the node-dir config.json (run_managed_node).
    let args = parse_runner_args()?;
    let node = bootstrap_node_from_args(&args)?;
    tracing::info!(node_name = %node.name, "starting ai_node_runner");
    run_managed_node(node).await
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
    // The name must still be a full L2 name, but the vault is the motherbee's, whatever hive
    // this node runs on.
    node_name.split('@').nth(1).filter(|v| !v.is_empty())?;
    Some(VaultClient::for_primary(
        dispatcher,
        VaultCallerOwned::new(ilk.to_string(), node_name.to_string()),
    ))
}

/// ai.generic's plug-in to the SDK's canonical managed control-plane. It supplies ONLY how to
/// validate + materialize an AI node config; the SDK owns the single-config boot/persist/lifecycle so
/// this node can no longer diverge (the two-location shadow bug is structurally impossible now).
struct AiGenericContract {
    node_name: String,
}

impl ManagedNodeConfigContract for AiGenericContract {
    fn node_family(&self) -> &'static str {
        "AI"
    }
    fn node_kind(&self) -> &'static str {
        "AI.generic"
    }
    fn required_fields(&self) -> &'static [&'static str] {
        &["behavior.kind", "behavior.model"]
    }
    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "behavior.model_settings",
            "behavior.base_url",
            "behavior.capabilities",
            "runtime.immediate_memory",
            "runtime.cognitive_definition",
        ]
    }
    fn notes(&self) -> &'static [&'static str] {
        &[
            "The OpenAI/Anthropic credential is NOT config: the node reads the hive AI provider's \
             key from SY.vault by resource_type openai|anthropic (owned by it, assigned to it, or its \
             tenant's general one).",
            "Cognitive assets (role/skill/handbook/personality) are applied to the agent ILK with \
             set_ilk_definition, not through CONFIG_SET.",
        ]
    }
    fn validate_and_materialize(&self, candidate: &Value) -> Result<Value, ContractError> {
        // parse_effective_config_doc already rejects secret-bearing / unsupported fields under the
        // strict contract; materialize fills defaults; build_behavior validates it is runnable.
        let doc = parse_effective_config_doc(candidate)
            .map_err(|err| ContractError::InvalidConfig(err.to_string()))?;
        let doc = materialize_effective_defaults(&self.node_name, doc);
        build_behavior_from_effective_config(&doc)
            .map_err(|err| ContractError::InvalidConfig(err.to_string()))?;
        serde_json::to_value(&doc).map_err(|err| ContractError::Internal(err.to_string()))
    }
    fn redact_effective_config(&self, effective: &Value) -> Value {
        redact_secrets(effective)
    }
}

/// Adapt the SDK's canonical control-plane state to ai.generic's local view (which the AI-specific
/// CONFIG_GET/status responses read). `config_source` is informational only now: with the
/// single-config model there is exactly one source — the node-dir `config.json`.
fn control_plane_state_from_managed(cp: ManagedControlPlaneState) -> ControlPlaneState {
    let config_source = match cp.current_state {
        ManagedNodeLifecycleState::Unconfigured => "none",
        _ => "config",
    };
    ControlPlaneState {
        current_state: cp.current_state,
        config_source,
        effective_config: cp.effective_config,
        schema_version: cp.schema_version,
        config_version: cp.config_version,
        last_error: cp.last_error,
    }
}

/// The `runtime.cognitive_definition` of the config the node boots with, or its defaults when the
/// node has no config or the config leaves it out (BUG-3: the managed path honors it). It is read
/// at start only.
fn effective_cognitive_section(
    config: Option<&EffectiveConfigDocument>,
) -> CognitiveDefinitionSection {
    config
        .and_then(|cfg| cfg.runtime.as_ref())
        .and_then(|runtime| runtime.cognitive_definition.clone())
        .unwrap_or_default()
}

async fn run_managed_node(
    node: NodeSection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let node_name = node.name.clone();
    // Single-config control-plane: the node-dir config.json is the ONLY source of truth (SDK-owned,
    // WF model). No state/ai-nodes dynamic file, so a stale one can never shadow a fresh run_node.
    let contract = AiGenericContract {
        node_name: node_name.clone(),
    };
    let cp = bootstrap_managed_control_plane(&node_name, &contract).unwrap_or_else(|err| {
        tracing::warn!(node_name = %node_name, error = %err, "control-plane bootstrap failed; booting Unconfigured");
        ManagedControlPlaneState::default()
    });
    match cp.current_state {
        ManagedNodeLifecycleState::Configured => {
            tracing::info!(node_name = %node_name, config_version = cp.config_version, "loaded config at bootstrap")
        }
        ManagedNodeLifecycleState::FailedConfig => {
            tracing::warn!(node_name = %node_name, config_version = cp.config_version, error = ?cp.last_error, "config.json rejected; booting FAILED_CONFIG (send a valid CONFIG_SET to recover)")
        }
        ManagedNodeLifecycleState::Unconfigured => {
            tracing::info!(node_name = %node_name, "no config.json; booting UNCONFIGURED")
        }
    }
    // Stores live in a per-node subdir UNDER the node instance dir (like WF's wf_instances.db) so the
    // generic purge (remove_dir_all of the instance dir) removes them on kill/respawn — nothing
    // survives to shadow config or bleed conversation state/memory into a fresh spawn.
    let instance_dir = managed_node_instance_dir(&node_name)
        .unwrap_or_else(|_| PathBuf::from(node.dynamic_config_dir.clone()));
    let thread_state_store =
        init_thread_state_store(&node_name, instance_dir.join("thread-state")).await;
    let immediate_memory_store =
        init_immediate_memory_store(&node_name, instance_dir.join("immediate-memory")).await;
    // The effective config drives both the cognitive layer (BUG-3: honor runtime.cognitive_definition)
    // and the behavior; deserialize it once.
    let effective_doc: Option<EffectiveConfigDocument> = cp
        .effective_config
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let cognitive_section = effective_cognitive_section(effective_doc.as_ref());
    let cognitive_definition_config = CognitiveDefinitionRuntimeConfig::from(cognitive_section);
    let cognitive_definition = Arc::new(RwLock::new(CognitiveDefinitionRuntimeState::unresolved()));
    let behavior = effective_doc
        .as_ref()
        .and_then(|doc| build_behavior_from_effective_config(doc).ok());
    let state = control_plane_state_from_managed(cp);

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
    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    let profile = build_ai_generic_rpc_profile()
        .map_err(|err| format!("ai-generic rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(runner_node_config, Duration::from_secs(1), profile)
            .await?;
    let vault = vault_client_for(dispatcher.clone(), &node_name, self_ilk_id.as_deref());
    let usage = UsageRecorder::start(dispatcher.clone());
    let ai_node = GenericAiNode {
        node_name,
        self_ilk_id,
        self_tenant_id,
        behavior: Arc::new(RwLock::new(behavior)),
        config_dir: PathBuf::from(node_config_dir),
        thread_state_store,
        immediate_memory_store,
        vault,
        usage,
        control_plane: Arc::new(RwLock::new(state)),
        cognitive_definition: cognitive_definition.clone(),
    };
    spawn_cognitive_definition_poll(
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

fn parse_runner_args() -> Result<BootstrapArgs, Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut parsed = BootstrapArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--node-name" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --node-name".to_string().into());
                };
                parsed.node_name = Some(value.clone());
                i += 2;
            }
            "--version" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --version".to_string().into());
                };
                parsed.version = Some(value.clone());
                i += 2;
            }
            "--router-socket" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --router-socket".to_string().into());
                };
                parsed.router_socket = Some(value.clone());
                i += 2;
            }
            "--uuid-persistence-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --uuid-persistence-dir"
                        .to_string()
                        .into());
                };
                parsed.uuid_persistence_dir = Some(value.clone());
                i += 2;
            }
            "--config-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --config-dir".to_string().into());
                };
                parsed.config_dir = Some(value.clone());
                i += 2;
            }
            "--dynamic-config-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --dynamic-config-dir"
                        .to_string()
                        .into());
                };
                parsed.dynamic_config_dir = Some(value.clone());
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
    let name = args
        .node_name
        .clone()
        .or_else(|| {
            let resolved = managed_node_name("", &["AI_NODE_NAME", "NODE_NAME"]);
            if resolved.trim().is_empty() {
                None
            } else {
                Some(resolved)
            }
        })
        .ok_or_else(|| {
            "pass --node-name, or set FLUXBEE_NODE_NAME (or AI_NODE_NAME)".to_string()
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

fn require_nonempty(
    field: &str,
    value: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty").into());
    }
    Ok(())
}

/// Audit A1 (credential exfiltration): `behavior.base_url` arrives on the NON-secret
/// CONFIG_SET plane but the vault-resolved provider api_key is then sent to it in the clear.
/// An unvalidated base_url turns config-write access into plaintext key exfiltration and
/// defeats Model D's plaintext confinement. A managed base_url may therefore ONLY be https
/// pointing at a provider's canonical API host. An operator can deliberately relax this for a
/// proxy/gateway via the process env `FLUXBEE_AI_ALLOW_INSECURE_BASE_URL` — env is set by
/// whoever launches the node, NOT by a CONFIG_SET writer, so it is not reachable from the
/// config plane.
fn validate_managed_base_url(
    base_url: &Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    const ALLOWED_HOSTS: &[&str] = &["api.openai.com", "api.anthropic.com"];
    let Some(raw) = base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(()); // no override -> provider default endpoint, always safe
    };
    if std::env::var_os("FLUXBEE_AI_ALLOW_INSECURE_BASE_URL").is_some() {
        return Ok(());
    }
    let rest = raw
        .strip_prefix("https://")
        .ok_or_else(|| format!("behavior.base_url must use https:// (got '{raw}')"))?;
    // Authority = everything before the first path/query/fragment delimiter.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Reject ANY userinfo: "https://api.openai.com:x@evil.com" would otherwise extract the
    // userinfo as the "host" while reqwest's URL parser connects to evil.com and ships the key
    // there (audit review — A1 host-extraction bypass). No provider endpoint uses userinfo.
    if authority.contains('@') {
        return Err(
            "behavior.base_url must not contain userinfo ('@'); only a bare provider host is allowed"
                .to_string()
                .into(),
        );
    }
    // Strip an optional :port to get the real host.
    let host = authority.split(':').next().unwrap_or("");
    if !ALLOWED_HOSTS
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
    {
        return Err(format!(
            "behavior.base_url host '{host}' is not an allowed provider endpoint \
             (allowed: api.openai.com, api.anthropic.com); set FLUXBEE_AI_ALLOW_INSECURE_BASE_URL \
             in the node process env to override for a trusted gateway"
        )
        .into());
    }
    Ok(())
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
        "ai_chat" => {
            let model = behavior
                .model
                .clone()
                .ok_or_else(|| "missing behavior.model for ai_chat".to_string())?;
            require_nonempty("behavior.model", &model)?;

            let model_settings = extract_model_settings_from_effective_config(behavior);
            let base_url = behavior.base_url.clone();
            validate_managed_base_url(&base_url)?;
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

            Ok(NodeBehavior::AiChat(AiChatRuntime {
                model,
                model_settings,
                base_url,
                immediate_memory,
                multimodal,
            }))
        }
        other => Err(format!("unsupported behavior.kind '{other}'").into()),
    }
}

/// Extract an AI provider api_key from a vault `value`. Vault may
/// return either a bare string or an object with `api_key` field; both are
/// accepted (matches what SY consumers do).
fn extract_ai_api_key_from_value(value: &Value) -> Option<String> {
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

fn find_ai_secret_contract_field(config: &Value) -> Option<&'static str> {
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
    if let Some(field) = find_ai_secret_contract_field(config) {
        return Err(format!(
            "secret-bearing field '{field}' is not accepted by ai.generic; use a Vault key with resource_type=openai|anthropic"
        )
        .into());
    }
    if let Some(field) = find_unsupported_ai_config_field(config) {
        return Err(format!(
            "unsupported field '{field}'; cognitive assets must be applied through set_ilk_definition"
        )
        .into());
    }
    if config
        .get("behavior")
        .and_then(|behavior| behavior.get("vault_key"))
        .is_some()
    {
        return Err(
            "behavior.vault_key is not accepted: ai.generic reads the hive AI provider's \
             key from SY.vault by resource_type (owned by it, assigned to it, or its tenant's general one)"
                .into(),
        );
    }
    Ok(serde_json::from_value::<EffectiveConfigDocument>(
        config.clone(),
    )?)
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
    if config.behavior.kind.eq_ignore_ascii_case("ai_chat") {
        if config.behavior.capabilities.is_none() {
            config.behavior.capabilities = Some(BehaviorCapabilities {
                multimodal: Some(default_multimodal_for_runtime()),
            });
        } else if let Some(caps) = config.behavior.capabilities.as_mut() {
            if caps.multimodal.is_none() {
                caps.multimodal = Some(default_multimodal_for_runtime());
            }
        }
    }
    config
}

fn materialize_runtime_defaults(runtime: &mut EffectiveRuntimeSection) {
    if runtime.immediate_memory.is_none() {
        runtime.immediate_memory = Some(ImmediateMemorySection::default());
    }
    if runtime.cognitive_definition.is_none() {
        runtime.cognitive_definition = Some(CognitiveDefinitionSection::default());
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

fn extract_model_settings_from_effective_config(
    behavior: &EffectiveBehaviorSection,
) -> ModelSettings {
    behavior
        .model_settings
        .as_ref()
        .map(|settings| ModelSettings {
            temperature: settings.temperature,
            top_p: settings.top_p,
            max_output_tokens: settings.max_output_tokens,
        })
        .unwrap_or_default()
}

fn node_runtime_not_ready_payload() -> Value {
    json!({
        "type": "error",
        "code": "node_runtime_not_ready",
        "message": "AI node runtime is not ready to process user messages yet.",
        "retryable": true
    })
}

fn missing_ai_api_key_payload(reason: &str) -> Value {
    json!({
        "type": "error",
        "code": "missing_ai_api_key",
        "message": "SY.vault has no usable key for the hive's AI provider (resource_type openai|anthropic): none owned by or assigned to this node, and no general one in its tenant.",
        "retryable": true,
        "details": { "reason": reason }
    })
}

#[derive(Debug, Deserialize)]
struct HiveAiFile {
    #[serde(default)]
    ai: Option<HiveAiConfig>,
}

/// The hive's AI provider: hive.yaml `ai`, the section every SY.* AI consumer reads, or OpenAI
/// when it is absent. It decides which Vault resource type holds the key.
fn hive_ai_provider(config_dir: &Path) -> std::result::Result<AiProvider, String> {
    let path = config_dir.join("hive.yaml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let hive: HiveAiFile =
        serde_yaml::from_str(&raw).map_err(|err| format!("invalid {}: {err}", path.display()))?;
    let engine = hive
        .ai
        .as_ref()
        .map(HiveAiConfig::effective)
        .transpose()
        .map_err(|err| format!("invalid ai section in {}: {err}", path.display()))?
        .unwrap_or_else(HiveAiConfig::fallback);
    Ok(engine.provider)
}

/// What the sender gets for a failed AI run. Once a tool has run, no failure is `retryable`
/// (`provider_timeout` on a later turn included): sending the same message again would run that
/// tool again.
fn ai_chat_failure_payload(err: &fluxbee_ai_sdk::errors::AiSdkError, tool_ran: bool) -> Value {
    let mut payload = ai_runtime_error_payload(err);
    if tool_ran {
        payload["retryable"] = Value::Bool(false);
    }
    payload
}

fn ai_runtime_error_payload(err: &fluxbee_ai_sdk::errors::AiSdkError) -> Value {
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
        // Either provider: the answer was billed and cut, so the same request gets the same cut.
        _ if err.is_truncated_answer() => json!({
            "type": "error",
            "code": "provider_output_truncated",
            "message": "The AI's answer hit its token limit and was cut. Raise behavior.model_settings.max_output_tokens.",
            "retryable": false
        }),
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(msg) => {
            if let Some((status, detail)) = parse_openai_status_error(msg) {
                if status == 400 || status == 404 || status == 422 {
                    if extract_openai_error_param(msg)
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

/// Extract (status, detail) from a provider HTTP error. Matches BOTH the OpenAI and Anthropic
/// adapter formats (audit M3): the SDK emits "<provider> error status={code} type={t} message={m}"
/// (Anthropic also carries request_id=). The old parser matched only "openai error status=" + a
/// now-removed " body=" tail, so every Anthropic error collapsed to a generic unclassified payload
/// and (after the B6 body-scrub) OpenAI details were lost too.
fn parse_openai_status_error(message: &str) -> Option<(u16, String)> {
    let idx = message.find("error status=")?;
    let after = &message[idx + "error status=".len()..];
    let status = after.split([' ', ',']).next()?.trim().parse::<u16>().ok()?;
    let detail = after
        .split_once("message=")
        .map(|(_, msg)| msg.trim().to_string())
        .or_else(|| {
            after
                .split_once(" body=")
                .map(|(_, body)| body.trim().to_string())
        })
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

/// Extract the OpenAI error `param` (the offending request field) from a provider error string
/// "openai error status=N type=T param=P message=M". The raw JSON body no longer travels (audit
/// B6), so param is now a named token bounded by " message="; empty/absent yields None.
fn extract_openai_error_param(error_str: &str) -> Option<String> {
    let after = error_str.split_once("param=")?.1;
    let param = after
        .split_once(" message=")
        .map(|(param, _)| param)
        .unwrap_or(after)
        .trim();
    (!param.is_empty()).then(|| param.to_string())
}

fn is_openai_attachment_param(param: &str) -> bool {
    let param = param.trim();
    param.contains(".file_data")
        || param.contains(".file_id")
        || param.contains(".file_url")
        || param.contains(".image_url")
        || param.contains(".content")
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
    store_root: PathBuf,
) -> Option<Arc<dyn ThreadStateStore>> {
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
    store_root: PathBuf,
) -> Option<Arc<ImmediateMemoryStore>> {
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
    mut parts: Vec<fluxbee_ai_sdk::ModelContentPart>,
    cognition_block: Option<&str>,
) -> Vec<fluxbee_ai_sdk::ModelContentPart> {
    let Some(block) = cognition_block.filter(|value| !value.trim().is_empty()) else {
        return parts;
    };
    let mut enriched = Vec::with_capacity(parts.len() + 1);
    enriched.push(fluxbee_ai_sdk::ModelContentPart::Text {
        text: block.to_string(),
    });
    enriched.append(&mut parts);
    enriched
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
    use uuid::Uuid;

    #[test]
    fn validate_managed_base_url_allows_provider_hosts_rejects_arbitrary() {
        // A1: only https provider hosts pass; an arbitrary/http host is rejected (credential exfil).
        assert!(validate_managed_base_url(&None).is_ok());
        assert!(validate_managed_base_url(&Some(String::new())).is_ok());
        assert!(validate_managed_base_url(&Some("https://api.openai.com/v1".into())).is_ok());
        assert!(validate_managed_base_url(&Some("https://api.anthropic.com".into())).is_ok());
        assert!(validate_managed_base_url(&Some("http://api.openai.com".into())).is_err()); // not https
        assert!(validate_managed_base_url(&Some("https://attacker.example/v1".into())).is_err());
        assert!(
            validate_managed_base_url(&Some("https://api.openai.com.evil.com".into())).is_err()
        );
        // userinfo bypass attempts (the real host is the attacker) MUST be rejected:
        assert!(
            validate_managed_base_url(&Some("https://api.openai.com:x@evil.com/v1".into()))
                .is_err()
        );
        assert!(
            validate_managed_base_url(&Some("https://api.openai.com@evil.com".into())).is_err()
        );
        assert!(validate_managed_base_url(&Some("https://api.openai.com:443".into())).is_ok());
        // explicit port OK
    }

    #[test]
    fn parse_provider_status_error_handles_both_providers() {
        // M3: status extracted from BOTH adapter formats; detail is the message (no raw body).
        let (s, d) = parse_openai_status_error(
            "openai error status=401 type=invalid_request_error message=Incorrect API key",
        )
        .expect("openai parsed");
        assert_eq!(s, 401);
        assert_eq!(d, "Incorrect API key");
        let (s, d) = parse_openai_status_error(
            "anthropic error status=429 type=rate_limit_error request_id=req_1 message=slow down",
        )
        .expect("anthropic parsed");
        assert_eq!(s, 429);
        assert_eq!(d, "slow down");
        assert!(parse_openai_status_error("some unrelated error").is_none());
    }

    #[test]
    fn extract_openai_error_param_reads_named_token_and_classifies_attachment() {
        // M3: attachment classification must survive the body->param=/message= format change.
        let err = "openai error status=400 type=invalid_request_error param=input[0].content[1].file_data message=Invalid value";
        let param = extract_openai_error_param(err).expect("param extracted");
        assert_eq!(param, "input[0].content[1].file_data");
        assert!(is_openai_attachment_param(&param));
        // Empty param -> None; no param field -> None.
        assert!(
            extract_openai_error_param("openai error status=401 type=x param= message=nope")
                .is_none()
        );
        assert!(extract_openai_error_param("anthropic error status=429 message=slow").is_none());
    }
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
                dst: Destination::Unicast("AI.generic@motherbee".to_string()),
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
        GenericAiNode {
            node_name: "AI.generic@motherbee".to_string(),
            self_ilk_id: None,
            self_tenant_id: None,
            behavior: Arc::new(RwLock::new(None)),
            config_dir: PathBuf::from("/tmp"),
            thread_state_store: None,
            immediate_memory_store: None,
            vault: None,
            usage: UsageRecorder::disabled(),
            control_plane: Arc::new(RwLock::new(ControlPlaneState::default())),
            cognitive_definition: Arc::new(RwLock::new(
                CognitiveDefinitionRuntimeState::unresolved(),
            )),
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

    /// Writes an asset the way SY.architect does, named by the SHA-256 of its bytes, and returns
    /// that hash.
    fn write_cognitive_asset(root: &std::path::Path, value: Value) -> String {
        let bytes = serde_json::to_vec_pretty(&value).expect("serialize asset");
        let hash = fluxbee_sdk::blob::sha256_hex(&bytes);
        fs::write(
            root.join("agent-assets").join(format!("{hash}.json")),
            bytes,
        )
        .expect("write cognitive asset");
        hash
    }

    /// An agent asset is the bytes its name hashes: one whose content changed under the same name
    /// (an edit by whoever could write agent-assets) is refused, never loaded as the agent's role.
    #[test]
    fn an_agent_asset_must_hash_to_its_name() {
        let root = cognitive_temp_root("asset-hash");
        let hash = write_cognitive_asset(
            &root,
            json!({"asset_type": "role", "name": "Support role", "description": "Help."}),
        );
        assert!(load_cognitive_asset(&AssetsFolder { blob_root: &root }, &hash, "role").is_ok());
        let path = root.join("agent-assets").join(format!("{hash}.json"));
        fs::write(
            &path,
            serde_json::to_vec(
                &json!({"asset_type": "role", "name": "Evil", "description": "Lie."}),
            )
            .expect("json"),
        )
        .expect("swap the asset");
        let err = load_cognitive_asset(&AssetsFolder { blob_root: &root }, &hash, "role")
            .expect_err("a changed asset");
        assert!(
            matches!(&err, AssetLoadError::Invalid(message) if message.contains("does not match its hash")),
            "{err:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The assets folder is written by the Syncthing user (A-72), so a name there is never
    /// followed or waited on: a symlink (even to the right bytes), a FIFO or a directory is an
    /// invalid asset, and so is a file over the size limit.
    #[cfg(unix)]
    #[test]
    fn an_agent_asset_is_read_only_from_a_regular_file_under_its_name() {
        let root = cognitive_temp_root("asset-file-kinds");
        let assets = root.join("agent-assets");
        let folder = AssetsFolder { blob_root: &root };
        let invalid = |result: Result<CognitiveAssetDocument, AssetLoadError>| match result {
            Err(AssetLoadError::Invalid(message)) => message,
            other => panic!("expected an invalid asset, got {other:?}"),
        };

        // Nothing under the name: missing, which a worker waits for.
        assert!(matches!(
            load_cognitive_asset(&folder, &"4".repeat(64), "role"),
            Err(AssetLoadError::Missing(_))
        ));

        let hash = write_cognitive_asset(
            &root,
            json!({"asset_type": "role", "name": "Support role", "description": "Help."}),
        );
        let real = root.join(format!("{hash}.real"));
        fs::rename(assets.join(format!("{hash}.json")), &real).expect("move the asset out");
        std::os::unix::fs::symlink(&real, assets.join(format!("{hash}.json")))
            .expect("plant a symlink");
        let message = invalid(load_cognitive_asset(&folder, &hash, "role"));
        assert!(message.contains("symbolic link"), "{message}");

        let fifo_hash = "1".repeat(64);
        let made = std::process::Command::new("mkfifo")
            .arg(assets.join(format!("{fifo_hash}.json")))
            .status()
            .expect("run mkfifo");
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let fifo_root = root.clone();
        std::thread::spawn(move || {
            let folder = AssetsFolder {
                blob_root: &fifo_root,
            };
            let _ = tx.send(load_cognitive_asset(&folder, &fifo_hash, "role"));
        });
        let loaded = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a FIFO must not block the read");
        let message = invalid(loaded);
        assert!(message.contains("not a regular file"), "{message}");

        let dir_hash = "2".repeat(64);
        fs::create_dir(assets.join(format!("{dir_hash}.json"))).expect("plant a directory");
        let message = invalid(load_cognitive_asset(&folder, &dir_hash, "role"));
        assert!(message.contains("not a regular file"), "{message}");

        let big_hash = "3".repeat(64);
        fs::write(
            assets.join(format!("{big_hash}.json")),
            vec![b' '; AGENT_ASSET_MAX_BYTES as usize + 1],
        )
        .expect("write an oversized file");
        let message = invalid(load_cognitive_asset(&folder, &big_hash, "role"));
        assert!(message.contains("too large"), "{message}");
        let _ = fs::remove_dir_all(&root);
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
    fn effective_cognitive_section_honors_config_and_defaults() {
        // BUG-3: the managed bootstrap must honor runtime.cognitive_definition (previously ignored).
        let configured = parse_effective_config_doc(&json!({
            "behavior": {"kind": "ai_chat", "model": "gpt-5.5"},
            "runtime": {"cognitive_definition": {"poll_interval_secs": 30, "blob_root": "/srv/blob"}}
        }))
        .expect("config parses");
        let section = effective_cognitive_section(Some(&configured));
        assert_eq!(section.poll_interval_secs, 30);
        assert_eq!(section.blob_root.as_deref(), Some("/srv/blob"));
        // No config, or a config without it: the defaults.
        for config in [None, Some(&EffectiveConfigDocument::default())] {
            let section = effective_cognitive_section(config);
            assert_eq!(
                section.poll_interval_secs,
                DEFAULT_COGNITIVE_POLL_INTERVAL_SECS
            );
            assert!(section.blob_root.is_none());
        }
    }

    const AGENT_NODE: &str = "AI.support@motherbee";

    /// Composes from the assets folder under `root`, as the node does.
    fn compose_from_disk(
        root: &Path,
        seq: u64,
        ilk: &IdentityIlkOption,
        hashes: CognitiveDefinitionHashes,
    ) -> CognitiveDefinitionRuntimeState {
        compose_cognitive_state(&AssetsFolder { blob_root: root }, seq, ilk, hashes)
            .expect("compose state")
            .state
    }

    /// Scripts what each asset read gives: valid bytes, missing, or unreadable.
    #[derive(Default)]
    struct ScriptedAssets {
        files: HashMap<String, Vec<u8>>,
        unreadable: std::collections::HashSet<String>,
    }

    impl ScriptedAssets {
        /// Adds an asset whose bytes hash to its name, and returns that hash.
        fn add(&mut self, value: Value) -> String {
            let bytes = serde_json::to_vec(&value).expect("serialize asset");
            let hash = fluxbee_sdk::blob::sha256_hex(&bytes);
            self.files.insert(hash.clone(), bytes);
            hash
        }
    }

    impl AssetSource for ScriptedAssets {
        fn read(&self, hash: &str) -> Result<Vec<u8>, AssetLoadError> {
            if self.unreadable.contains(hash) {
                return Err(AssetLoadError::Unreadable(
                    "Permission denied (os error 13)".to_string(),
                ));
            }
            self.files
                .get(hash)
                .cloned()
                .ok_or_else(|| AssetLoadError::Missing(format!("asset missing {hash}")))
        }
    }

    fn role(description: &str) -> Value {
        json!({"asset_type": "role", "name": "Support role", "description": description})
    }

    fn agent_with_role(hash: &str) -> IdentityIlkOption {
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(hash.to_string());
        ilk
    }

    /// The poll had no definite answer: the agent still runs the prompt with `prompt_part`, and
    /// `last_poll_error` says why.
    async fn assert_definition_kept(
        state: &Arc<RwLock<CognitiveDefinitionRuntimeState>>,
        outcome: Result<(), String>,
        prompt_part: &str,
        reason: &str,
    ) {
        let error = outcome.expect_err("no definite answer");
        assert!(error.contains(reason), "{error}");
        let current = state.read().await;
        assert!(current.active_prompt.contains(prompt_part));
        assert_eq!(current.last_poll_error.as_deref(), Some(error.as_str()));
    }

    /// Before its first definition the agent has nothing to keep: a read that fails leaves the
    /// "unconfigured" prompt, and a missing asset is composed as it is (here `partial`).
    #[tokio::test]
    async fn before_its_first_definition_an_agent_takes_what_a_poll_finds() {
        let node = test_node();
        let state = &node.cognitive_definition;
        let mut assets = ScriptedAssets::default();

        let shm_down = Err("identity SHM unavailable: busy".to_string());
        assert!(apply_identity_lookup(AGENT_NODE, &assets, shm_down, state)
            .await
            .is_err());
        apply_identity_lookup(AGENT_NODE, &assets, Ok(None), state)
            .await
            .expect("no ILK yet");
        assert_eq!(state.read().await.definition_state, "unresolved");

        let unreadable = assets.add(role("Help customers."));
        assets.unreadable.insert(unreadable.clone());
        let lookup = Ok(Some((1, agent_with_role(&unreadable))));
        assert!(apply_identity_lookup(AGENT_NODE, &assets, lookup, state)
            .await
            .is_err());
        assert_eq!(node.agent_prompt().await, DEFAULT_UNCONFIGURED_AGENT_PROMPT);

        let mut ilk = agent_with_role(&"ab".repeat(32));
        let skill = assets.add(json!({
            "asset_type": "skill", "name": "triage", "instructions": ["Ask first."]
        }));
        ilk.skill_hashes = vec![skill];
        apply_identity_lookup(AGENT_NODE, &assets, Ok(Some((2, ilk))), state)
            .await
            .expect("composed without the missing role");
        let current = state.read().await.clone();
        assert_eq!(current.definition_state, "partial");
        assert!(current.active_prompt.contains("Ask first."));
    }

    /// Once the agent has a definition, a poll that settles nothing keeps it: the identity SHM
    /// unreadable, the ILK absent (a degraded replica), a new asset not on this hive yet (on a
    /// worker it arrives seconds after the ILK names it) or unreadable. The new definition replaces
    /// it once every asset is read.
    #[tokio::test]
    async fn an_agent_keeps_its_definition_until_a_definite_new_one() {
        let node = test_node();
        let state = &node.cognitive_definition;
        let mut assets = ScriptedAssets::default();
        let first = assets.add(role("Help customers."));
        let ilk = agent_with_role(&first);
        apply_identity_lookup(AGENT_NODE, &assets, Ok(Some((1, ilk.clone()))), state)
            .await
            .expect("composed");

        let shm_down = Err("identity SHM unavailable: busy".to_string());
        let outcome = apply_identity_lookup(AGENT_NODE, &assets, shm_down, state).await;
        assert_definition_kept(
            state,
            outcome,
            "Help customers.",
            "identity SHM unavailable",
        )
        .await;
        let outcome = apply_identity_lookup(AGENT_NODE, &assets, Ok(None), state).await;
        assert_definition_kept(state, outcome, "Help customers.", "agent ILK not found").await;

        let sell = assets.add(role("Sell more."));
        let in_flight = assets.files.remove(&sell).expect("the new role");
        let lookup = Ok(Some((2, agent_with_role(&sell))));
        let outcome = apply_identity_lookup(AGENT_NODE, &assets, lookup, state).await;
        assert_definition_kept(state, outcome, "Help customers.", "not on this hive yet").await;

        assets.files.insert(sell.clone(), in_flight);
        assets.unreadable.insert(sell.clone());
        let lookup = Ok(Some((3, agent_with_role(&sell))));
        let outcome = apply_identity_lookup(AGENT_NODE, &assets, lookup, state).await;
        assert_definition_kept(state, outcome, "Help customers.", "could not be read").await;

        assets.unreadable.clear();
        apply_identity_lookup(
            AGENT_NODE,
            &assets,
            Ok(Some((4, agent_with_role(&sell)))),
            state,
        )
        .await
        .expect("the new definition");
        let current = state.read().await.clone();
        assert_eq!(current.definition_state, "composed");
        assert!(current.active_prompt.contains("Sell more."));
        assert!(current.last_poll_error.is_none());
    }

    /// A definite answer replaces a definition: an asset that is there but invalid (wrong hash,
    /// wrong type) is left out, a cleared definition is `empty`, and an ILK that is no longer an
    /// agent leaves the agent unresolved.
    #[tokio::test]
    async fn an_invalid_asset_a_cleared_definition_or_a_non_agent_ilk_is_definite() {
        let node = test_node();
        let state = &node.cognitive_definition;
        let mut assets = ScriptedAssets::default();
        let first = assets.add(role("Help customers."));
        let ilk = agent_with_role(&first);
        let resolve = |seq| Ok(Some((seq, agent_with_role(&first))));

        let tampered = "cd".repeat(32);
        let lie = serde_json::to_vec(&role("Lie.")).expect("json");
        assets.files.insert(tampered.clone(), lie);
        let skill_as_role = assets.add(json!({
            "asset_type": "skill", "name": "triage", "instructions": ["Ask first."]
        }));
        for (seq, invalid) in [(2, &tampered), (4, &skill_as_role)] {
            apply_identity_lookup(AGENT_NODE, &assets, resolve(seq - 1), state)
                .await
                .expect("composed");
            let lookup = Ok(Some((seq, agent_with_role(invalid))));
            apply_identity_lookup(AGENT_NODE, &assets, lookup, state)
                .await
                .expect("an invalid asset is an answer");
            let current = state.read().await.clone();
            assert_eq!(current.definition_state, "error");
            assert_eq!(current.active_prompt, DEFAULT_UNCONFIGURED_AGENT_PROMPT);
        }

        apply_identity_lookup(AGENT_NODE, &assets, resolve(5), state)
            .await
            .expect("composed");
        let cleared = sample_agent_ilk();
        apply_identity_lookup(AGENT_NODE, &assets, Ok(Some((6, cleared))), state)
            .await
            .expect("a cleared definition");
        assert_eq!(state.read().await.definition_state, "empty");

        apply_identity_lookup(AGENT_NODE, &assets, resolve(7), state)
            .await
            .expect("composed");
        let mut person = ilk.clone();
        person.ilk_type = "human".to_string();
        apply_identity_lookup(AGENT_NODE, &assets, Ok(Some((8, person))), state)
            .await
            .expect("no longer an agent");
        let current = state.read().await.clone();
        assert_eq!(current.definition_state, "unresolved");
        assert_eq!(current.active_prompt, DEFAULT_UNCONFIGURED_AGENT_PROMPT);
    }

    #[test]
    fn cognitive_definition_empty_hashes_uses_default_unconfigured_prompt() {
        let root = cognitive_temp_root("empty-hashes");
        let ilk = sample_agent_ilk();
        let hashes = CognitiveDefinitionHashes::default();

        let state = compose_from_disk(&root, 7, &ilk, hashes);

        assert_eq!(state.definition_state, "empty");
        assert_eq!(state.ilk_id.as_deref(), Some(ilk.ilk_id.as_str()));
        assert_eq!(state.last_identity_seq, Some(7));
        assert_eq!(state.active_prompt, DEFAULT_UNCONFIGURED_AGENT_PROMPT);
        assert_eq!(
            state.active_prompt_chars,
            DEFAULT_UNCONFIGURED_AGENT_PROMPT.chars().count()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_reuses_state_when_hashes_are_unchanged() {
        let root = cognitive_temp_root("reuse-unchanged-hashes");
        let role_hash = write_cognitive_asset(
            &root,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Answer as a Fluxbee support agent."
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);
        let mut state = compose_from_disk(&root, 8, &ilk, hashes.clone());

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
        let role_hash = write_cognitive_asset(
            &root,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Answer as a Fluxbee support agent.",
                "tone": "direct",
                "limits": ["Do not invent platform capabilities."]
            }),
        );
        let skill_hash = write_cognitive_asset(
            &root,
            json!({
                "asset_type": "skill",
                "name": "triage",
                "description": "Classify operator requests.",
                "instructions": ["Ask for missing node names before mutating state."],
                "constraints": ["No SCMD execution without operator confirmation."],
                "examples": [{"input": "list nodes", "output": "read-only inventory"}]
            }),
        );
        let handbook_hash = write_cognitive_asset(
            &root,
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

        let state = compose_from_disk(&root, 8, &ilk, hashes);
        let prompt = state.active_prompt.as_str();

        assert_eq!(state.definition_state, "composed");
        assert_eq!(state.role_hash_loaded.as_deref(), Some(role_hash.as_str()));
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
        let role_hash = write_cognitive_asset(
            &root,
            json!({
                "asset_type": "role",
                "name": "Support role",
                "description": "Receive and triage support tickets.",
            }),
        );
        let personality_hash = write_cognitive_asset(
            &root,
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

        let state = compose_from_disk(&root, 21, &ilk, hashes);

        assert_eq!(state.definition_state, "composed");
        assert_eq!(
            state.personality_hash_loaded.as_deref(),
            Some(personality_hash.as_str())
        );
        let prompt = state.active_prompt.as_str();
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
        let missing_personality =
            "5555555555555555555555555555555555555555555555555555555555555555";
        let role_hash = write_cognitive_asset(
            &root,
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

        let state = compose_from_disk(&root, 22, &ilk, hashes);

        assert_eq!(state.definition_state, "partial");
        assert_eq!(state.role_hash_loaded.as_deref(), Some(role_hash.as_str()));
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
        let personality_hash = write_cognitive_asset(
            &root,
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

        let state = compose_from_disk(&root, 23, &ilk, hashes);

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
        let missing_skill_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let role_hash = write_cognitive_asset(
            &root,
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

        let state = compose_from_disk(&root, 9, &ilk, hashes);

        assert_eq!(state.definition_state, "partial");
        assert_eq!(state.role_hash_loaded.as_deref(), Some(role_hash.as_str()));
        assert_eq!(state.failed_hashes.len(), 1);
        assert_eq!(state.failed_hashes[0].asset_type, "skill");
        assert_eq!(state.failed_hashes[0].hash, missing_skill_hash);
        assert!(state.active_prompt.contains("Answer with the loaded role."));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cognitive_definition_reports_error_for_malformed_asset_schema() {
        let root = cognitive_temp_root("malformed-asset");
        let role_hash = write_cognitive_asset(
            &root,
            json!({
                "asset_type": "role",
                "name": "Broken role"
            }),
        );
        let mut ilk = sample_agent_ilk();
        ilk.role_hash = Some(role_hash.to_string());
        let hashes = hashes_from_identity_ilk(&ilk);

        let state = compose_from_disk(&root, 11, &ilk, hashes);

        assert_eq!(state.definition_state, "error");
        assert!(state.role_hash_loaded.is_none());
        assert_eq!(state.failed_hashes.len(), 1);
        assert_eq!(state.failed_hashes[0].asset_type, "role");
        assert!(state.failed_hashes[0].error.contains("role.description"));
        assert_eq!(state.active_prompt, DEFAULT_UNCONFIGURED_AGENT_PROMPT);
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
        let large_content = "x".repeat(COMPOSED_PROMPT_MAX_BYTES * 2);
        let handbook_hash = write_cognitive_asset(
            &root,
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

        let state = compose_from_disk(&root, 10, &ilk, hashes);
        let prompt = state.active_prompt.as_str();

        assert_eq!(state.definition_state, "composed");
        assert!(state.prompt_truncated);
        assert!(prompt.len() <= COMPOSED_PROMPT_MAX_BYTES);
        assert!(prompt.contains("[TRUNCATED]"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn node_status_get_health_follows_the_config_state() {
        // BUG-2: a node that is not CONFIGURED refuses every completion, so it is DEGRADED (never
        // HEALTHY, never ERROR), with its lifecycle_state, and last_error for a rejected config.
        let node = test_node();
        let status = |response: Option<Message>| response.expect("status response").payload;

        let unconfigured = status(
            node.handle_control_plane(sample_request())
                .await
                .expect("control plane"),
        );
        assert_eq!(unconfigured["health_state"], json!("DEGRADED"));
        assert_eq!(unconfigured["lifecycle_state"], json!("UNCONFIGURED"));
        assert!(unconfigured.get("last_error").is_none());

        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::FailedConfig;
            state.last_error = Some(ControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: "unknown field `instructions`".to_string(),
            });
        }
        let failed = status(
            node.handle_control_plane(sample_request())
                .await
                .expect("control plane"),
        );
        assert_eq!(failed["health_state"], json!("DEGRADED"));
        assert_eq!(failed["lifecycle_state"], json!("FAILED_CONFIG"));
        assert_eq!(failed["last_error"]["code"], json!("invalid_config"));

        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
            state.effective_config = Some(json!({}));
            state.last_error = None;
        }
        let healthy = status(
            node.handle_control_plane(sample_request())
                .await
                .expect("control plane"),
        );
        assert_eq!(healthy["health_state"], json!("HEALTHY"));
        assert_eq!(healthy["lifecycle_state"], json!("CONFIGURED"));
    }

    fn config_set_request(config: Value) -> Message {
        let mut msg = sample_request();
        msg.meta.msg = Some("CONFIG_SET".to_string());
        msg.payload = json!({
            "subsystem": "ai_node",
            "node_name": "AI.generic@motherbee",
            "schema_version": 1,
            "config_version": 2,
            "apply_mode": "replace",
            "config": config
        });
        msg
    }

    /// The prompt comes only from the agent ILK: `behavior.instructions`, the old
    /// `behavior.params` alias and the runtime keys nothing reads are unknown fields, so CONFIG_SET
    /// refuses them and nothing is applied or written.
    #[tokio::test]
    async fn config_set_refuses_instructions_and_unread_runtime_keys() {
        let node = test_node();
        let cases = [
            (
                json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5", "instructions": "Be brief."}}),
                "instructions",
            ),
            (
                json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5", "params": {"system_prompt": "Be brief."}}}),
                "params",
            ),
            (
                json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5"}, "runtime": {"handler_timeout_ms": 120000}}),
                "handler_timeout_ms",
            ),
            (
                json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5"}, "runtime": {"immediate_memory": {"enabled": true, "active_operations_max": 8}}}),
                "active_operations_max",
            ),
            (
                json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5"}, "runtime": {"cognitive_definition": {"enabled": false}}}),
                "enabled",
            ),
        ];
        for (config, field) in cases {
            let response = node
                .handle_control_plane(config_set_request(config))
                .await
                .expect("control plane")
                .expect("CONFIG_RESPONSE");
            assert_eq!(response.payload["ok"], json!(false), "{field}");
            assert_eq!(
                response.payload["error"]["code"],
                json!("invalid_config"),
                "{field}"
            );
            let message = response.payload["error"]["message"]
                .as_str()
                .expect("error message");
            assert!(
                message.contains(&format!("unknown field `{field}`")),
                "{message}"
            );
        }
        assert!(node.behavior.read().await.is_none());
        assert_eq!(
            node.control_plane.read().await.current_state,
            NodeLifecycleState::Unconfigured
        );
    }

    #[test]
    fn the_effective_config_carries_only_the_runtime_keys_the_node_reads() {
        let doc = materialize_effective_defaults(
            "AI.sales@motherbee",
            parse_effective_config_doc(
                &json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5"}}),
            )
            .expect("config parses"),
        );
        let runtime = serde_json::to_value(&doc).expect("serialize")["runtime"].clone();
        assert_eq!(
            runtime,
            json!({
                "immediate_memory": {"enabled": false, "recent_interactions_max": 10, "summary_max_chars": 1600},
                "cognitive_definition": {"poll_interval_secs": 10, "blob_root": null}
            })
        );
        // What it writes reads back.
        assert!(
            parse_effective_config_doc(&serde_json::to_value(&doc).expect("serialize")).is_ok()
        );
    }

    #[test]
    fn a_cut_answer_has_its_own_error_code_on_both_providers() {
        for message in [
            "openai response truncated: hit max_output_tokens (raise model_settings.max_output_tokens)",
            "anthropic response truncated: hit max_tokens (raise model_settings.max_output_tokens)",
        ] {
            let payload = ai_runtime_error_payload(&fluxbee_ai_sdk::errors::AiSdkError::Protocol(
                message.to_string(),
            ));
            assert_eq!(payload["code"], json!("provider_output_truncated"), "{message}");
            assert_eq!(payload["retryable"], json!(false));
        }
        let other = ai_runtime_error_payload(&fluxbee_ai_sdk::errors::AiSdkError::Protocol(
            "anthropic response missing text content".to_string(),
        ));
        assert_eq!(other["code"], json!("provider_error"));
    }

    /// A model call that times out is retryable only while no tool has run: on a later turn of
    /// the tool loop, a resend would run that tool again.
    #[test]
    fn a_failure_after_a_tool_ran_is_not_retryable() {
        let timeout = fluxbee_ai_sdk::errors::AiSdkError::Timeout("model call".to_string());
        let before = ai_chat_failure_payload(&timeout, false);
        assert_eq!(before["code"], json!("provider_timeout"));
        assert_eq!(before["retryable"], json!(true));
        let after = ai_chat_failure_payload(&timeout, true);
        assert_eq!(after["code"], json!("provider_timeout"));
        assert_eq!(after["retryable"], json!(false));
    }

    #[tokio::test]
    async fn ai_chat_missing_api_key_returns_error_payload_instead_of_fatal() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::remove_var("OPENAI_API_KEY_MISSING_FOR_TEST");
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::AiChat(AiChatRuntime {
                model: "gpt-5.5".to_string(),
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
            Some("missing_ai_api_key")
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
                dst: Destination::Unicast("AI.generic@motherbee".to_string()),
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
            vec![fluxbee_ai_sdk::ModelContentPart::Text {
                text: "hello".to_string(),
            }],
            Some(&block),
        );
        assert_eq!(enriched.len(), 2);
        assert!(matches!(
            &enriched[0],
            fluxbee_ai_sdk::ModelContentPart::Text { text } if text == &block
        ));
        assert!(matches!(
            &enriched[1],
            fluxbee_ai_sdk::ModelContentPart::Text { text } if text == "hello"
        ));
    }

    #[test]
    fn default_mode_registry_does_not_expose_gov_tools() {
        let node = test_node();
        let registry = node
            .build_tool_registry(&BehaviorContext::default())
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
                ..BehaviorContext::default()
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
    fn parse_effective_config_rejects_ai_secret_contract_fields() {
        let cases = [
            (
                json!({
                    "behavior": {"kind": "ai_chat", "model": "gpt-5.5"},
                    "secrets": {"openai": {"api_key": "sk-test"}}
                }),
                "config.secrets",
            ),
            (
                json!({
                    "behavior": {
                        "kind": "ai_chat",
                        "model": "gpt-5.5",
                        "api_key": "sk-test"
                    }
                }),
                "config.behavior.api_key",
            ),
            (
                json!({
                    "behavior": {
                        "kind": "ai_chat",
                        "model": "gpt-5.5",
                        "api_key_env": "OPENAI_API_KEY"
                    }
                }),
                "config.behavior.api_key_env",
            ),
            (
                json!({
                    "behavior": {
                        "kind": "ai_chat",
                        "model": "gpt-5.5",
                        "openai": {"api_key": "sk-test"}
                    }
                }),
                "config.behavior.openai",
            ),
            (
                json!({
                    "behavior": {"kind": "ai_chat", "model": "gpt-5.5"},
                    "assets": {"role_hash": "abc"}
                }),
                "config.assets",
            ),
        ];

        for (config, field) in cases {
            let rejected_field = find_ai_secret_contract_field(&config)
                .or_else(|| find_unsupported_ai_config_field(&config));
            assert_eq!(rejected_field, Some(field));
            let err = parse_effective_config_doc(&config)
                .expect_err("secret-bearing AI config should be rejected");
            assert!(err.to_string().contains(field));
        }
    }

    #[test]
    fn ai_chat_contract_needs_a_model_and_names_no_key_or_provider() {
        let minimal = parse_effective_config_doc(&json!({
            "behavior": {"kind": "ai_chat", "model": "gpt-5.5"}
        }))
        .expect("kind + model is a complete ai_chat config");
        assert!(build_behavior_from_effective_config(&minimal).is_ok());

        let with_key_name = json!({
            "behavior": {"kind": "ai_chat", "model": "gpt-5.5", "vault_key": "ai/test"}
        });
        let err =
            parse_effective_config_doc(&with_key_name).expect_err("a Vault key name is not config");
        assert!(err
            .to_string()
            .contains("behavior.vault_key is not accepted"));

        let with_provider = json!({
            "behavior": {"kind": "ai_chat", "model": "gpt-5.5", "provider": "openai"}
        });
        assert!(parse_effective_config_doc(&with_provider).is_err());

        let missing_model = parse_effective_config_doc(&json!({
            "behavior": {"kind": "ai_chat"}
        }))
        .expect("document shape parses");
        assert!(build_behavior_from_effective_config(&missing_model).is_err());

        let legacy = parse_effective_config_doc(&json!({
            "behavior": {"kind": "openai_chat", "model": "gpt-4.1-mini"}
        }))
        .expect("document shape parses");
        assert!(build_behavior_from_effective_config(&legacy).is_err());
    }

    #[test]
    fn the_ai_provider_is_the_hives_and_openai_without_an_ai_section() {
        let dir = std::env::temp_dir().join(format!("ai-generic-hive-ai-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");

        fs::write(dir.join("hive.yaml"), "hive_id: motherbee\n").expect("write hive.yaml");
        assert_eq!(hive_ai_provider(&dir), Ok(AiProvider::OpenAi));

        fs::write(
            dir.join("hive.yaml"),
            "hive_id: motherbee\nai:\n  default_provider: anthropic\n  providers:\n    anthropic:\n      model: claude-sonnet-4-5\n",
        )
        .expect("write hive.yaml");
        assert_eq!(hive_ai_provider(&dir), Ok(AiProvider::Anthropic));

        // The selected provider must be configured: a broken ai section is an error, not OpenAI.
        fs::write(
            dir.join("hive.yaml"),
            "hive_id: motherbee\nai:\n  default_provider: anthropic\n",
        )
        .expect("write hive.yaml");
        assert!(hive_ai_provider(&dir).is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
