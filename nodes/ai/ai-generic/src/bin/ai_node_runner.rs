use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    build_ai_behavior_response, build_model_user_content_parts,
    build_output_schema_fallback_instruction, build_reply_message_runtime_src, control_request,
    create_function_calling_model, create_llm_client, extract_text,
    resolve_model_input_from_payload_with_options, resolve_response_envelope_output_schema,
    AgentTool, AiAccount, AiBehaviorOutput, AiFinalOutput, AiNode, AiProvider, AiUserArtifact,
    ControlRequest, FunctionCallingConfig, FunctionCallingModel, FunctionCallingRunner,
    FunctionLoopItem, FunctionLoopRunResult, FunctionRunInput, FunctionToolProvider,
    FunctionToolRegistry, HiveAiConfig, ImmediateConversationMemory, LanceDbThreadStateStore,
    Message, ModelInputOptions, ModelSettings, NodeRuntime, ResolvedModelInput, RuntimeConfig,
    TenantSource, ThreadStateDeleteTool, ThreadStateGetTool, ThreadStatePutTool, ThreadStateStore,
    ThreadStateToolsProvider, ToolCard, ToolCatalog, ToolErrorCode, ToolRunWatch, UsageRecorder,
    MSG_PING, MSG_STATUS,
};
use fluxbee_sdk::identity::{find_ilk_by_handler_node_from_hive_config, IdentityIlkOption};
use fluxbee_sdk::managed_control_plane::{
    bootstrap_managed_control_plane, build_node_status_response_payload,
    persist_effective_config_with_root, ContractError, ControlPlaneErrorInfo,
    ManagedControlPlaneState, ManagedNodeConfigContract, ManagedNodeLifecycleState,
    DEFAULT_MANAGED_NODES_ROOT,
};
use fluxbee_sdk::protocol::{
    MemoryPackage, Meta, MSG_CONFIG_GET, MSG_CONFIG_RESPONSE, MSG_CONFIG_SET, MSG_NODE_HELP,
    MSG_NODE_HELP_RESPONSE, MSG_NODE_STATUS_GET, MSG_NODE_STATUS_GET_RESPONSE,
};
use fluxbee_sdk::{
    managed_node_config_path_with_root, managed_node_instance_dir, managed_node_name,
    timer_node_name, AdminCommandRequest, NodeConfig, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, TimerClient, TimerClientConfig, TimerClientError,
    TimerNowInPayload, TimerNowInResult, VaultCallerOwned, VaultClient,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs as tokio_fs;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tracing_subscriber::EnvFilter;

const IMMEDIATE_INTERACTION_MAX_CHARS: usize = 1_200;
const AI_RUNTIME_KIND: &str = "ai.generic";
const DEFAULT_AGENT_ASSET_BLOB_ROOT: &str = "/var/lib/fluxbee/blob";
const AGENT_ASSET_MAX_BYTES: u64 = 256 * 1024;
const COMPOSED_PROMPT_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_COGNITIVE_POLL_INTERVAL_SECS: u64 = 10;
/// The files one reply carries at most: what an IO node sends out in one message
/// (nodes/io/common text_v1_blob.rs, `DEFAULT_MAX_ATTACHMENTS`); io.slack refuses a reply with
/// more, its text included. Its other two limits, 20 MiB a file and 40 MiB in all, are out of the
/// generators' reach: a file holds what the model wrote in one call.
const REPLY_FILES_MAX: usize = 8;
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
    /// The node's AI provider (agent-tools-v1 T8): its API, and the vault resource its key is
    /// read by. Without it, the hive's `ai.default_provider` at each request.
    #[serde(default)]
    provider: Option<AiProvider>,
    /// The URL requests are posted to: one of `provider`'s, which it needs
    /// (`validate_managed_base_url`).
    #[serde(default)]
    base_url: Option<String>,
    /// Whether the model receives images and documents (true by default).
    #[serde(default)]
    multimodal: Option<bool>,
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
    /// `behavior.model`; `None` follows the hive's model for the node's provider
    /// (`effective_ai_engine`, FINDINGS A-104).
    model: Option<String>,
    model_settings: ModelSettings,
    /// `behavior.provider`; `None` follows the hive's (`effective_ai_engine`).
    provider: Option<AiProvider>,
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
    /// The runtime version the orchestrator launched it at (`launched_runtime_version`), which it
    /// names in its `NODE_HELP` answer. `None` for a node started by hand.
    runtime_version: Option<String>,
    /// The node's connection to its router, for the tools that ask another node of the mesh:
    /// `get_current_time` asks SY.timer of the node's hive. `None` only in a unit test.
    router: Option<Arc<RouterDispatcher>>,
}

/// The files one message's tools make, for its reply (FINDINGS A-98, A-99). A file generator keeps
/// its file here and answers the model only the file's name, type and size, so the bytes never go
/// back into the model's context. The reply carries every file, in the order they were made, with
/// the model's own text (`tool_loop_output`).
#[derive(Default)]
struct ReplyFiles(std::sync::Mutex<Vec<AiUserArtifact>>);

/// What `ReplyFiles::keep` did with a file.
#[derive(Debug, PartialEq)]
enum KeptFile {
    /// It goes after the files the reply already carries.
    Added,
    /// It took the place of the reply's file of the same name: one reply never carries a name
    /// twice, and the new file takes no other place.
    Replaced,
    /// It was dropped: the reply already carries `REPLY_FILES_MAX` files.
    Full,
}

impl ReplyFiles {
    /// The files, whatever a call that panicked holding them left.
    fn files(&self) -> std::sync::MutexGuard<'_, Vec<AiUserArtifact>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Keeps `file` for the reply (`KeptFile`).
    fn keep(&self, file: AiUserArtifact) -> KeptFile {
        let mut files = self.files();
        if let Some(earlier) = files
            .iter_mut()
            .find(|earlier| earlier.filename == file.filename)
        {
            *earlier = file;
            return KeptFile::Replaced;
        }
        if files.len() >= REPLY_FILES_MAX {
            return KeptFile::Full;
        }
        files.push(file);
        KeptFile::Added
    }

    /// Every file kept, in the order they were made; none is left.
    fn take(&self) -> Vec<AiUserArtifact> {
        std::mem::take(&mut *self.files())
    }
}

#[derive(Clone)]
struct GenerateCsvArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateTextArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateJsonArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateMarkdownArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateHtmlArtifactTool {
    files: Arc<ReplyFiles>,
}

/// Publishes an HTML page as a public, read-only URL. Unlike the `generate_*` tools (which
/// only produce a downloadable artifact), this one ACTS: it writes the bytes to the blob
/// store and drives `SY.admin`'s `publish_artifact` over the mesh socket, returning the
/// capability URL. Only registered when the node has a resolved identity (`vault` present),
/// because publishing is tenant-scoped by the producer's router-stamped identity (admin-side).
struct PublishHtmlTool {
    vault: VaultClient,
    /// The blob store the page is written to before SY.admin publishes it: the node's own
    /// (`BlobConfig::default()`).
    blob: fluxbee_sdk::blob::BlobConfig,
}

#[derive(Clone)]
struct GeneratePdfArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateXlsxArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateDocxArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GeneratePngArtifactTool {
    files: Arc<ReplyFiles>,
}

#[derive(Clone)]
struct GenerateJpegArtifactTool {
    files: Arc<ReplyFiles>,
}

/// The current time, from SY.timer of the node's own hive (agent-tools-v1 §6.1).
struct GetCurrentTimeTool {
    router: Arc<RouterDispatcher>,
    /// `SY.timer@<the node's hive>`.
    timer: String,
}

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
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateTextArtifactArgs {
    filename: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateJsonArtifactArgs {
    filename: String,
    data: Value,
    #[serde(default = "default_true")]
    pretty: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateMarkdownArtifactArgs {
    filename: String,
    content: String,
}

/// `generate_html_artifact`'s arguments, and `publish_html_page`'s.
#[derive(Debug, Clone, Deserialize)]
struct GenerateHtmlArtifactArgs {
    filename: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GeneratePdfArtifactArgs {
    filename: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    lines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GenerateXlsxArtifactArgs {
    filename: String,
    rows: Vec<Vec<String>>,
    #[serde(default)]
    headers: Option<Vec<String>>,
    #[serde(default)]
    sheet_name: Option<String>,
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
}

#[derive(Debug, Clone, Deserialize)]
struct GetCurrentTimeArgs {
    #[serde(default)]
    timezone: Option<String>,
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
        let poll_secs = value.poll_interval_secs.clamp(1, 300);
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
                // What this request runs on: the node's provider and model, or the hive's.
                let engine = match effective_ai_engine(ai, &self.config_dir).for_request() {
                    Ok(engine) => engine,
                    Err(err) => {
                        tracing::warn!(
                            node_name = %self.node_name,
                            trace_id = %msg.routing.trace_id,
                            error = %err,
                            "AI request has no provider or no model; replying with the engine error payload"
                        );
                        return Ok(Some(build_reply_message_runtime_src(
                            &msg,
                            ai_engine_error_payload(&err),
                        )));
                    }
                };
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
                    .run_ai_chat(
                        ai,
                        &engine,
                        input,
                        input_parts,
                        &user_text,
                        &behavior_ctx,
                        &msg.meta,
                    )
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
                                    model = %engine.model,
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
                                    model = %engine.model,
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
                                model = %engine.model,
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
        engine: &AiEngine,
        input: String,
        input_parts: Option<Vec<fluxbee_ai_sdk::ModelContentPart>>,
        user_text: &str,
        ctx: &BehaviorContext,
        meta: &Meta,
    ) -> fluxbee_ai_sdk::Result<AiBehaviorOutput> {
        let credential = self
            .resolve_ai_credential(engine.provider)
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
        let files = Arc::new(ReplyFiles::default());
        let tool_registry = self.build_tool_registry(ctx, &files)?;
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
            model = %engine.model,
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
                engine.model.clone(),
                Some(system),
                ai.model_settings.clone(),
                account,
            );
            let watched = ToolRunWatch::new(model.as_ref(), &ctx.tool_ran);
            if let Some(output) = self
                .run_tool_loop(
                    &watched,
                    &tool_registry,
                    &files,
                    ai,
                    ctx,
                    input.clone(),
                    input_parts.clone(),
                    user_text,
                )
                .await?
            {
                return Ok(output);
            }
        }

        let req = fluxbee_ai_sdk::llm::LlmRequest {
            model: engine.model.clone(),
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

    /// One message's tool loop, and its reply: the model's own text with every file its tools made
    /// and the link of every page it published (`tool_loop_output`, FINDINGS A-98, A-99). `None`
    /// when the model ended without text, made no file and published no page: the caller then asks
    /// it once more, without tools.
    async fn run_tool_loop(
        &self,
        model: &dyn FunctionCallingModel,
        tools: &FunctionToolRegistry,
        files: &ReplyFiles,
        ai: &AiChatRuntime,
        ctx: &BehaviorContext,
        input: String,
        input_parts: Option<Vec<fluxbee_ai_sdk::ModelContentPart>>,
        user_text: &str,
    ) -> fluxbee_ai_sdk::Result<Option<AiBehaviorOutput>> {
        let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
        let immediate_memory = self.load_immediate_memory_for_input(ai, ctx).await;
        let run_input =
            self.build_function_run_input(input, input_parts, ctx, ai, immediate_memory);
        let result = runner.run_with_input(model, tools, run_input).await?;
        let Some(output) = tool_loop_output(
            tool_loop_text(&result),
            files.take(),
            &published_links(&result.items),
        ) else {
            return Ok(None);
        };
        if matches!(output, AiBehaviorOutput::Final(_)) {
            self.log_final_output(ctx, &output);
        }
        let summary_text = summarize_behavior_output(&output);
        self.persist_immediate_turn(ai, ctx, user_text, &summary_text)
            .await;
        Ok(Some(output))
    }

    /// The journal's line for a reply that carries files: their names and the length of its text,
    /// never the text. The text can carry the link of a page `publish_html_page` published, and
    /// the link is the key to the page (FINDINGS A-95).
    fn log_final_output(&self, ctx: &BehaviorContext, output: &AiBehaviorOutput) {
        let (artifact_filenames, text_len) = match output {
            AiBehaviorOutput::Text(text) => (Vec::new(), text.len()),
            AiBehaviorOutput::Final(final_output) => (
                final_output
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.filename.as_str())
                    .collect::<Vec<_>>(),
                final_output.text.as_deref().map_or(0, str::len),
            ),
        };
        tracing::info!(
            node_name = %self.node_name,
            thread_id = ?ctx.thread_id,
            src_ilk = ?ctx.src_ilk,
            artifact_filenames = ?artifact_filenames,
            text_len,
            "reply carries the files its tools made"
        );
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

    /// One message's tools. The file generators keep their files in `files`, the reply's.
    fn build_tool_registry(
        &self,
        ctx: &BehaviorContext,
        files: &Arc<ReplyFiles>,
    ) -> fluxbee_ai_sdk::Result<FunctionToolRegistry> {
        let mut registry = FunctionToolRegistry::new();
        self.register_common_tools(&mut registry, ctx, files)?;
        Ok(registry)
    }

    fn register_common_tools(
        &self,
        registry: &mut FunctionToolRegistry,
        ctx: &BehaviorContext,
        files: &Arc<ReplyFiles>,
    ) -> fluxbee_ai_sdk::Result<()> {
        // Every base tool there is (`base_tool_cards`, the catalog), each when its card's
        // `present_when` holds. The state tools keep the sender's "hard state": bound to its
        // src_ilk, never to a key the model names (thread_id is conversational metadata only).
        // Immediate memory is managed separately by the runner as short-horizon context.
        if let (Some(store), Some(src_ilk)) = (&self.thread_state_store, &ctx.src_ilk) {
            let provider = ThreadStateToolsProvider::new(store.clone(), src_ilk.clone());
            provider.register_tools(registry)?;
        }
        let files = || files.clone();
        registry.register(Arc::new(GenerateCsvArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateTextArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateJsonArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateMarkdownArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateHtmlArtifactTool { files: files() }))?;
        registry.register(Arc::new(GeneratePdfArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateXlsxArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateDocxArtifactTool { files: files() }))?;
        registry.register(Arc::new(GeneratePngArtifactTool { files: files() }))?;
        registry.register(Arc::new(GenerateJpegArtifactTool { files: files() }))?;
        // The time, from SY.timer of the node's own hive.
        if let Some(tool) = self
            .router
            .as_ref()
            .and_then(|router| GetCurrentTimeTool::for_node(router.clone(), &self.node_name))
        {
            registry.register(Arc::new(tool))?;
        }
        // Publishing ACTION: expose it only when the node has a resolved identity (vault).
        // Publishing is tenant-scoped by the producer's router-stamped identity (resolved
        // admin-side), and vault is Some exactly when that identity is present — so a node
        // without identity simply has no publish tool (no dead/unreachable action).
        if let Some(vault) = self.vault.as_ref() {
            registry.register(Arc::new(PublishHtmlTool {
                vault: vault.clone(),
                blob: fluxbee_sdk::blob::BlobConfig::default(),
            }))?;
        }
        Ok(())
    }

    /// The AI key comes from SY.vault by resource type, like every other AI consumer: the
    /// node's provider's (`effective_ai_engine`), and only in its tenant: one this node owns,
    /// one assigned to it, or its tenant's general one (docs/ilk-tenant-model-v1.md §2.2). The
    /// root tenant's keys are the core's. Nodes do not name a Vault key.
    async fn resolve_ai_credential(
        &self,
        provider: AiProvider,
    ) -> std::result::Result<ResolvedAiCredential, String> {
        let Some(vault) = self.vault.as_ref() else {
            return Err(
                "missing AI api key: no vault client (the node has no self ILK)".to_string(),
            );
        };
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

    /// A control verb runs only as the router's authority gate delivers it, from the node that
    /// sends it (`control_request`): SY.admin@motherbee for the config, this hive's orchestrator for
    /// the status and the help. Anything else that names a verb is refused with `unauthorized`.
    async fn handle_control_plane(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        let Some(command) = msg.meta.msg.as_deref() else {
            return Ok(None);
        };
        let (response_msg, response_payload) =
            match control_request(&msg, &self.node_name, &CONTROL_VERBS) {
                ControlRequest::Run(MSG_NODE_STATUS_GET) => (
                    MSG_NODE_STATUS_GET_RESPONSE,
                    self.build_node_status_get_response().await,
                ),
                ControlRequest::Run(MSG_NODE_HELP) => {
                    (MSG_NODE_HELP_RESPONSE, self.build_node_help_response())
                }
                ControlRequest::Run(MSG_CONFIG_SET) => {
                    (MSG_CONFIG_RESPONSE, self.apply_config_set(&msg).await)
                }
                ControlRequest::Run(MSG_CONFIG_GET) => {
                    (MSG_CONFIG_RESPONSE, self.build_config_get_response().await)
                }
                ControlRequest::Run(MSG_PING) => ("PONG", self.build_ping_response().await),
                ControlRequest::Run(MSG_STATUS) => {
                    ("STATUS_RESPONSE", self.build_status_response().await)
                }
                ControlRequest::Refuse { verb, reason } => {
                    tracing::warn!(
                        node_name = %self.node_name,
                        trace_id = %msg.routing.trace_id,
                        src_l2_name = ?msg.routing.src_l2_name,
                        msg_type = %msg.meta.msg_type,
                        msg = %command,
                        reason = %reason,
                        "control request refused (unauthorized)"
                    );
                    self.refused_control_response(verb, reason).await
                }
                ControlRequest::Run(_) | ControlRequest::Other => {
                    let state = self.control_plane.read().await.current_state;
                    (
                        MSG_CONFIG_RESPONSE,
                        self.error_response(
                            "unknown_system_msg",
                            format!("Unsupported control-plane command: {command}"),
                            1,
                            0,
                            state.as_str(),
                        ),
                    )
                }
            };
        Ok(Some(build_control_plane_response(
            &msg,
            response_msg,
            response_payload,
        )))
    }

    /// The answer to a control request the node refuses: the response its verb names, so the
    /// sender does not wait out its timeout, with the error code `unauthorized` in that response's
    /// own shape.
    async fn refused_control_response(&self, verb: &str, reason: String) -> (&'static str, Value) {
        let response_msg = match verb {
            MSG_NODE_STATUS_GET => MSG_NODE_STATUS_GET_RESPONSE,
            MSG_NODE_HELP => MSG_NODE_HELP_RESPONSE,
            MSG_PING => "PONG",
            MSG_STATUS => "STATUS_RESPONSE",
            _ => MSG_CONFIG_RESPONSE,
        };
        if response_msg != MSG_CONFIG_RESPONSE {
            let refusal =
                json!({"status": "error", "error_code": "unauthorized", "message": reason});
            return (response_msg, refusal);
        }
        let state = self.control_plane.read().await;
        let refusal = self.error_response(
            "unauthorized",
            reason,
            state.schema_version,
            state.config_version,
            state.current_state.as_str(),
        );
        (response_msg, refusal)
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
                    "Invalid payload.config: field '{field}' is not accepted by ai.generic; load the provider's key into SY.vault with resource_type openai or anthropic"
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
        // A model the node cannot resolve now would fail every message (missing_ai_model), so the
        // config is refused: a provider other than the hive's default needs its model, unless the
        // hive's hive.yaml lists one for it (a worker add_hive made lists only the default's). So
        // is a provider left to a hive.yaml that cannot be read.
        let engine = match &next_behavior {
            NodeBehavior::AiChat(ai) => {
                let engine = effective_ai_engine(ai, &self.config_dir);
                if let Err(err) = engine.provider.as_ref().and(engine.model.as_ref()) {
                    return self.invalid_config_response(
                        Some(schema_version),
                        Some(config_version),
                        format!("Invalid payload.config: {}", unresolved_model_reason(err)),
                    );
                }
                engine.config_json()
            }
            NodeBehavior::Echo => (Value::Null, Value::Null),
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
            let engine = self.engine_in_effect(self.behavior.read().await.as_ref());
            return self.ok_response(
                subsystem,
                state.schema_version,
                state.config_version,
                state.current_state.as_str(),
                state.effective_config.as_ref(),
                engine,
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
            engine,
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

    /// CONFIG_SET's answer to a config it holds: with `ai_provider` and `ai_model`, the `engine`
    /// that config runs on, as CONFIG_GET shows them.
    fn ok_response(
        &self,
        subsystem: &str,
        schema_version: u32,
        config_version: u64,
        state: &str,
        effective_config: Option<&Value>,
        (ai_provider, ai_model): (Value, Value),
    ) -> Value {
        json!({
            "subsystem": subsystem,
            "node_name": self.node_name.as_str(),
            "ok": true,
            "state": state,
            "schema_version": schema_version,
            "config_version": config_version,
            "ai_provider": ai_provider,
            "ai_model": ai_model,
            "error": Value::Null,
            "effective_config": effective_config.map(redact_secrets),
        })
    }

    /// CONFIG_GET's `ai_provider` and `ai_model` for `behavior`: the provider and the model it
    /// runs on, its own or the hive's as hive.yaml says now; both null when it is not ai_chat.
    fn engine_in_effect(&self, behavior: Option<&NodeBehavior>) -> (Value, Value) {
        match behavior {
            Some(NodeBehavior::AiChat(ai)) => {
                effective_ai_engine(ai, &self.config_dir).config_json()
            }
            _ => (Value::Null, Value::Null),
        }
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
        // The provider and the model in effect: the node's own, or the hive's as hive.yaml says
        // now.
        let (ai_provider, ai_model) = self.engine_in_effect(self.behavior.read().await.as_ref());
        json!({
            "subsystem": "ai_node",
            "node_name": self.node_name.as_str(),
            "ok": ok,
            "state": state.current_state.as_str(),
            "config_source": config_source,
            "api_key_source": api_key_source.as_str(),
            "ai_provider": ai_provider,
            "ai_model": ai_model,
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "contract": {
                "node_family": "AI",
                "node_kind": AI_RUNTIME_KIND,
                "supports": ["CONFIG_GET", "CONFIG_SET"],
                "required_fields": [
                    "config.behavior.kind"
                ],
                "field_values": {
                    "config.behavior.kind": {
                        "allowed": ["echo", "ai_chat"]
                    },
                    "config.behavior.model": {
                        "examples": ["gpt-5.5", "claude-sonnet-5-5"]
                    },
                    "config.behavior.provider": {
                        "allowed": ["openai", "anthropic"]
                    }
                },
                "optional_fields": [
                    "config.behavior.model",
                    "config.behavior.model_settings",
                    "config.behavior.provider",
                    "config.behavior.base_url",
                    "config.behavior.multimodal",
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
                    "The AI provider is config.behavior.provider (openai or anthropic), or the hive's when it is absent (hive.yaml ai.default_provider, OpenAI without an ai section); ai_provider shows the one in effect.",
                    "The model is config.behavior.model, or the hive's model for the node's provider when it is absent (hive.yaml ai.providers.<provider>.model; gpt-5.5 for OpenAI without an ai section); ai_model shows the one in effect. Name a model only when the node needs its own, and always one of the node's provider. A provider other than the hive's default needs its model unless the hive's hive.yaml lists one (a worker added by add_hive lists only the default's): CONFIG_SET refuses a config whose model it cannot resolve (invalid_config), and its answer shows ai_provider and ai_model. Without either later (hive.yaml changed), every message fails with missing_ai_model.",
                    "AI credentials are not config: the provider's key is read from SY.vault by resource_type openai|anthropic on each request, only in the node's tenant: one this node owns, one assigned to it, or its tenant's general one.",
                    "config.behavior.vault_key is not accepted: nodes do not name a Vault key.",
                    "CONFIG_SET rejects secret-bearing fields such as config.secrets.openai.api_key, config.behavior.openai.api_key, config.behavior.api_key, and config.behavior.api_key_env.",
                    "config.behavior.base_url needs config.behavior.provider, and it must be https on that provider's host (api.openai.com or api.anthropic.com).",
                    "ai.generic defaults behavior.multimodal=true unless explicitly overridden; behavior.capabilities is refused (it became behavior.multimodal).",
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

    /// `NODE_HELP` (docs/agent-tools-v1.md §3.3): the node describes itself with its runtime's
    /// tool catalog, the cards of every base tool; each card's `present_when` says when a message
    /// gets that tool. Answered in any lifecycle state, like `NODE_STATUS_GET`.
    fn build_node_help_response(&self) -> Value {
        let catalog = tool_catalog(self.runtime_version.as_deref());
        json!({
            "status": "ok",
            "node_name": self.node_name.as_str(),
            "runtime": catalog.runtime,
            "runtime_version": catalog.runtime_version,
            "tools": catalog.tools,
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
    config_dir: &Path,
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

/// The reply of one message's tool loop (FINDINGS A-98, A-99): the model's `text`
/// (`tool_loop_text`), with every file its tools made, in the order they were made, and with each
/// of the `links` its pages were published at that the text does not carry, on a line of its own,
/// so a published link reaches the person even when the model leaves it out. A loop that made
/// files and left no text gets a fixed line naming them. `None` with no text, no file and no link.
fn tool_loop_output(
    text: Option<String>,
    files: Vec<AiUserArtifact>,
    links: &[String],
) -> Option<AiBehaviorOutput> {
    let text = text.filter(|text| !text.trim().is_empty());
    let text = if files.is_empty() {
        text
    } else {
        Some(text.unwrap_or_else(|| files_fallback_line(&files)))
    };
    let missing = links
        .iter()
        .map(String::as_str)
        .filter(|link| !text.as_deref().is_some_and(|text| text.contains(link)))
        .collect::<Vec<_>>();
    let text = if missing.is_empty() {
        text
    } else {
        Some(
            text.as_deref()
                .map(str::trim_end)
                .into_iter()
                .chain(missing)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    };
    if files.is_empty() {
        return text.map(AiBehaviorOutput::text);
    }
    Some(AiBehaviorOutput::final_output(AiFinalOutput::new(
        text, files,
    )))
}

/// The text of one tool loop's reply: the model's final text or, when that is blank, the last text
/// it wrote alongside a tool call. The loop sends only the final turn's text; text in a turn that
/// calls tools would be lost when the model ends without more.
fn tool_loop_text(result: &FunctionLoopRunResult) -> Option<String> {
    let written = |text: &String| !text.trim().is_empty();
    result
        .final_assistant_text
        .clone()
        .filter(written)
        .or_else(|| {
            result.items.iter().rev().find_map(|item| match item {
                FunctionLoopItem::AssistantToolCalls {
                    content: Some(text),
                    ..
                } if written(text) => Some(text.clone()),
                _ => None,
            })
        })
}

/// The links of the pages `publish_html_page` published in one tool loop, in the order published,
/// each once: the `url` of each of its results that says `ok`, as the tool answered the model.
fn published_links(items: &[FunctionLoopItem]) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    for item in items {
        let FunctionLoopItem::ToolResult { result } = item else {
            continue;
        };
        if result.is_error
            || result.name != "publish_html_page"
            || result.output.get("status").and_then(Value::as_str) != Some("ok")
        {
            continue;
        }
        let Some(link) = result
            .output
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|link| !link.is_empty())
        else {
            continue;
        };
        if !links.iter().any(|known| known == link) {
            links.push(link.to_string());
        }
    }
    links
}

/// The reply's text when the model wrote none: the generators' fixed line, in Spanish, naming the
/// files.
fn files_fallback_line(files: &[AiUserArtifact]) -> String {
    let names = files
        .iter()
        .map(|file| file.filename.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if files.len() == 1 {
        format!("Aca esta el archivo solicitado: {names}")
    } else {
        format!("Aca estan los archivos solicitados: {names}")
    }
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

/// One CSV row: every cell quoted, with its quotes doubled, after `csv_cell`.
fn csv_line(cells: &[String]) -> String {
    cells
        .iter()
        .map(|cell| {
            let escaped = csv_cell(cell).replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A cell as the CSV file holds it. Excel and LibreOffice run a cell that starts with `=`, `+`, `-`
/// or `@` as a formula, quoted or not, so it gets a `'` in front (OWASP, CSV injection): a cell
/// that starts with `=`, `+`, `@`, a tab or a carriage return, and one that starts with `-` unless
/// it is a plain number.
fn csv_cell(cell: &str) -> std::borrow::Cow<'_, str> {
    let starts_a_formula = match cell.chars().next() {
        Some('=' | '+' | '@' | '\t' | '\r') => true,
        Some('-') => !is_plain_negative_number(cell),
        _ => false,
    };
    if starts_a_formula {
        std::borrow::Cow::Owned(format!("'{cell}"))
    } else {
        std::borrow::Cow::Borrowed(cell)
    }
}

/// `-` and digits, with a `.` or a `,` between digits: `-12`, `-12.5`, `-1.234,56`.
fn is_plain_negative_number(cell: &str) -> bool {
    cell.strip_prefix('-').is_some_and(|digits| {
        digits
            .split(['.', ','])
            .all(|group| !group.is_empty() && group.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

fn default_true() -> bool {
    true
}

/// A generator's answer to the model once its file is made: the file is kept for the reply, and
/// the model gets only its name, type and size (FINDINGS A-98). A file named as an earlier one
/// takes its place. A reply that already carries `REPLY_FILES_MAX` files takes no more: the model
/// is told, and the file is dropped.
fn keep_reply_file(tool_name: &str, files: &ReplyFiles, file: AiUserArtifact) -> Value {
    let (filename, mime, size_bytes) = (file.filename.clone(), file.mime.clone(), file.bytes.len());
    let kept = files.keep(file);
    if kept == KeptFile::Full {
        tracing::warn!(
            tool = %tool_name,
            filename = %filename,
            max = REPLY_FILES_MAX,
            "the reply already carries the most files it can; this one was dropped"
        );
        return json!({
            "status": "error",
            "error_code": "reply_files_full",
            "message": format!(
                "{tool_name}: this reply already carries {REPLY_FILES_MAX} files, the most one \
                 reply can; {filename} was not made"
            ),
            "retryable": false
        });
    }
    tracing::info!(
        tool = %tool_name,
        filename = %filename,
        mime = %mime,
        size_bytes,
        replaced = kept == KeptFile::Replaced,
        "file kept for the reply"
    );
    json!({
        "status": "ok",
        "filename": filename,
        "mime": mime,
        "size_bytes": size_bytes
    })
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

// The PDF `build_pdf_bytes` writes (FINDINGS A-97): US Letter pages, in points, with 50 pt
// margins, of Helvetica 12 pt, a line every 14 pt.
const PDF_PAGE_WIDTH: u32 = 612;
const PDF_PAGE_HEIGHT: u32 = 792;
const PDF_MARGIN: u32 = 50;
const PDF_FONT_SIZE: u32 = 12;
const PDF_LINE_HEIGHT: u32 = 14;
/// The lines a page holds: the first baseline 50 pt below the top, the last one above the bottom
/// margin. 50.
const PDF_LINES_PER_PAGE: usize =
    ((PDF_PAGE_HEIGHT - 2 * PDF_MARGIN) / PDF_LINE_HEIGHT + 1) as usize;
/// The width a line may take, in the font's units (a thousandth of the font size): the page less
/// its margins.
const PDF_LINE_WIDTH_UNITS: u32 = (PDF_PAGE_WIDTH - 2 * PDF_MARGIN) * 1000 / PDF_FONT_SIZE;

/// The advance width of each Helvetica glyph, by its byte in WinAnsiEncoding, in thousandths of
/// the font size: Adobe's Helvetica metrics (the standard 14 fonts' AFM). Lines are measured with
/// them rather than by counting characters, since an `m` is nearly four times an `i`. 0 marks a
/// byte with no glyph, which the encoder never writes (`pdf_line_bytes`).
const HELVETICA_WIDTHS: [u16; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // 0x20
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 0x30
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // 0x40
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 0x50
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 0x60
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 0, // 0x70
    556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0, // 0x80
    0, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667, // 0x90
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, // 0xA0
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611, // 0xB0
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, // 0xC0
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, // 0xD0
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278, // 0xE0
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500, // 0xF0
];

/// Windows-1252's 0x80-0x9F, where it differs from Latin-1: the rest of WinAnsiEncoding is
/// printable ASCII and U+00A0-U+00FF, each its own byte.
const CP1252_HIGH: [(char, u8); 27] = [
    ('\u{20AC}', 0x80), // €
    ('\u{201A}', 0x82), // single low-9 quotation mark
    ('\u{0192}', 0x83), // ƒ
    ('\u{201E}', 0x84), // double low-9 quotation mark
    ('\u{2026}', 0x85), // …
    ('\u{2020}', 0x86), // †
    ('\u{2021}', 0x87), // ‡
    ('\u{02C6}', 0x88), // ˆ
    ('\u{2030}', 0x89), // ‰
    ('\u{0160}', 0x8A), // Š
    ('\u{2039}', 0x8B), // ‹
    ('\u{0152}', 0x8C), // Œ
    ('\u{017D}', 0x8E), // Ž
    ('\u{2018}', 0x91), // left single quotation mark
    ('\u{2019}', 0x92), // right single quotation mark
    ('\u{201C}', 0x93), // left double quotation mark
    ('\u{201D}', 0x94), // right double quotation mark
    ('\u{2022}', 0x95), // •
    ('\u{2013}', 0x96), // –
    ('\u{2014}', 0x97), // —
    ('\u{02DC}', 0x98), // ˜
    ('\u{2122}', 0x99), // ™
    ('\u{0161}', 0x9A), // š
    ('\u{203A}', 0x9B), // ›
    ('\u{0153}', 0x9C), // œ
    ('\u{017E}', 0x9E), // ž
    ('\u{0178}', 0x9F), // Ÿ
];

/// One line of text in WinAnsiEncoding, the font's: Windows-1252, one byte a character. A tab is
/// a space; any other character without a glyph there, a control character or one outside
/// Windows-1252 (Cyrillic, Chinese, an emoji), is `?`.
fn pdf_line_bytes(line: &str) -> Vec<u8> {
    line.chars()
        .map(|ch| match u32::from(ch) {
            0x09 => b' ',
            code @ (0x20..=0x7E | 0xA0..=0xFF) => code as u8,
            _ => CP1252_HIGH
                .iter()
                .find(|(high, _)| *high == ch)
                .map_or(b'?', |(_, byte)| *byte),
        })
        .collect()
}

/// A line, as `pdf_line_bytes` encodes it, cut into the lines that fit the page's width: at the
/// last space that fits, or inside a word wider than a whole line. The spaces a cut falls on are
/// dropped; the line's own leading spaces stay.
fn wrap_pdf_line(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    loop {
        let mut width = 0;
        let mut end = start;
        let mut last_space = None;
        while let Some(&byte) = bytes.get(end) {
            let glyph = u32::from(HELVETICA_WIDTHS[usize::from(byte)]);
            if width + glyph > PDF_LINE_WIDTH_UNITS {
                break;
            }
            width += glyph;
            if byte == b' ' {
                last_space = Some(end);
            }
            end += 1;
        }
        if end == bytes.len() {
            lines.push(&bytes[start..]);
            return lines;
        }
        // Every glyph is narrower than a line, so `end` is past `start` and so is the cut.
        let cut = if bytes[end] == b' ' {
            end
        } else {
            match last_space {
                Some(space) if space > start => space,
                _ => end,
            }
        };
        lines.push(&bytes[start..cut]);
        start = cut;
        while bytes.get(start) == Some(&b' ') {
            start += 1;
        }
        if start == bytes.len() {
            return lines;
        }
    }
}

/// Every line the PDF prints, encoded and wrapped: the title and a blank line, then each entry of
/// `lines`, an entry with line breaks being several lines.
fn pdf_text_lines(title: Option<&str>, lines: &[String]) -> Vec<Vec<u8>> {
    let title = title.map(str::trim).filter(|title| !title.is_empty());
    let mut entries: Vec<&str> = Vec::new();
    if let Some(title) = title {
        entries.extend([title, ""]);
    }
    if lines.is_empty() && title.is_none() {
        entries.push("Documento generado por AI");
    } else {
        entries.extend(lines.iter().map(String::as_str));
    }
    let mut printed = Vec::new();
    for entry in entries {
        let mut source: Vec<&str> = entry.lines().collect();
        if source.is_empty() {
            // A blank entry is a blank line, which `lines` does not give.
            source.push("");
        }
        for line in source {
            let bytes = pdf_line_bytes(line);
            printed.extend(wrap_pdf_line(&bytes).into_iter().map(<[u8]>::to_vec));
        }
    }
    printed
}

/// One page's content stream: its lines from the top margin down, each a literal string. `\`,
/// `(` and `)` are escaped, and every byte from 0x80 is an octal escape (`\351` for é), so the
/// stream stays ASCII.
fn pdf_page_content(lines: &[Vec<u8>]) -> Vec<u8> {
    let mut content = format!(
        "BT\n/F1 {PDF_FONT_SIZE} Tf\n{PDF_MARGIN} {} Td\n{PDF_LINE_HEIGHT} TL\n",
        PDF_PAGE_HEIGHT - PDF_MARGIN
    )
    .into_bytes();
    for line in lines {
        content.push(b'(');
        for &byte in line {
            match byte {
                b'\\' | b'(' | b')' => content.extend_from_slice(&[b'\\', byte]),
                0x80..=0xFF => content.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
                _ => content.push(byte),
            }
        }
        content.extend_from_slice(b") Tj\nT*\n");
    }
    content.extend_from_slice(b"ET");
    content
}

/// A PDF of `title` and `lines` (FINDINGS A-97): Helvetica in WinAnsiEncoding, so Spanish prints
/// as written; lines wrapped to the page's width; as many pages as the text takes.
fn build_pdf_bytes(title: Option<&str>, lines: &[String]) -> Vec<u8> {
    let text = pdf_text_lines(title, lines);
    let pages: Vec<&[Vec<u8>]> = text.chunks(PDF_LINES_PER_PAGE).collect();
    // Objects 1 to 3 are the catalog, the page tree and the font; page `i` is object 4 + 2i and
    // its content stream the next one.
    let kids = (0..pages.len())
        .map(|page| format!("{} 0 R", 4 + 2 * page))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    for (page, lines) in pages.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PDF_PAGE_WIDTH} {PDF_PAGE_HEIGHT}] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * page
            )
            .into_bytes(),
        );
        let content = pdf_page_content(lines);
        let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        stream.extend_from_slice(&content);
        stream.extend_from_slice(b"\nendstream");
        objects.push(stream);
    }

    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
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

/// The card of every base tool of the runtime, in the order `register_common_tools` registers
/// them: the tool catalog (docs/agent-tools-v1.md §3), which `NODE_HELP` answers and
/// docs/reference/agent-tools.{json,md} hold (the test `the_tool_catalog_matches_the_cards`).
fn base_tool_cards() -> Vec<ToolCard> {
    vec![
        ThreadStateGetTool::card(),
        ThreadStatePutTool::card(),
        ThreadStateDeleteTool::card(),
        GenerateCsvArtifactTool::card(),
        GenerateTextArtifactTool::card(),
        GenerateJsonArtifactTool::card(),
        GenerateMarkdownArtifactTool::card(),
        GenerateHtmlArtifactTool::card(),
        GeneratePdfArtifactTool::card(),
        GenerateXlsxArtifactTool::card(),
        GenerateDocxArtifactTool::card(),
        GeneratePngArtifactTool::card(),
        GenerateJpegArtifactTool::card(),
        GetCurrentTimeTool::card(),
        PublishHtmlTool::card(),
    ]
}

/// The runtime's tool catalog, which `NODE_HELP` answers with the node's runtime version. The
/// repository keeps it, without a version, as docs/reference/agent-tools.{json,md}, and the
/// package ships the JSON next to the runtime with the version it ships
/// (`dist/runtimes/ai.generic/<version>/tools.json`).
fn tool_catalog(runtime_version: Option<&str>) -> ToolCatalog {
    ToolCatalog {
        runtime: AI_RUNTIME_KIND.to_string(),
        runtime_version: runtime_version.map(str::to_string),
        tools: base_tool_cards(),
    }
}

/// What sets one file generator's card apart; `artifact_tool_card` adds what they all share.
struct ArtifactToolSpec {
    name: &'static str,
    summary: &'static str,
    when_to_use: &'static str,
    /// What its file can and cannot hold.
    limits: &'static [&'static str],
    /// The filename endings its type takes, as the card says them (".md or .markdown").
    extensions: &'static str,
    mime: &'static str,
    /// Its own arguments as JSON schema properties: `filename` is every generator's.
    properties: Value,
    /// Its own required arguments, besides `filename`.
    required: &'static [&'static str],
}

/// What every file generator's card says, once and in one limit: the reply is the text the model
/// writes after its last tool call (`tool_loop_text` takes a call's text only when that is
/// blank), every file the reply's calls make goes with it, up to `REPLY_FILES_MAX`, a file named
/// as an earlier one takes its place (`ReplyFiles::keep`), and the model gets back only the
/// file's name, type and size (`keep_reply_file`, `tool_loop_output`). The rules of `filename`
/// are in its input description, which does not count against the description's limit
/// (`TOOL_DESCRIPTION_MAX_CHARS`).
fn artifact_tool_limit() -> String {
    format!(
        "Your reply is the text you write after your last tool call; text written alongside a \
         call is not sent. Every file this reply's calls make goes with it, in order, up to \
         {REPLY_FILES_MAX}: a call past that makes none, and a file with an earlier one's name \
         takes its place. You get back the file's name, type and size, never its content."
    )
}

/// The rules of a `filename`, which `AiUserArtifact` checks, for a file whose name ends in
/// `extensions`.
fn filename_rules(extensions: &str) -> String {
    format!(
        "it ends in {extensions}, with at most 128 bytes and no `/`, `\\`, `..` or control \
         character"
    )
}

fn artifact_tool_card(spec: ArtifactToolSpec) -> ToolCard {
    let ArtifactToolSpec {
        name,
        summary,
        when_to_use,
        limits,
        extensions,
        mime,
        mut properties,
        required,
    } = spec;
    properties["filename"] = json!({
        "type": "string",
        "minLength": 1,
        "description": format!("The file's name, as the user sees it: {}.", filename_rules(extensions))
    });
    let required: Vec<&str> = std::iter::once("filename")
        .chain(required.iter().copied())
        .collect();
    let mut all_limits: Vec<String> = limits.iter().map(|limit| limit.to_string()).collect();
    all_limits.push(artifact_tool_limit());
    ToolCard {
        name: name.to_string(),
        summary: summary.to_string(),
        when_to_use: when_to_use.to_string(),
        limits: all_limits,
        present_when: "always".to_string(),
        input: json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false
        }),
        output: json!({
            "description": "`{status: \"ok\", filename, mime, size_bytes}`: the file is kept for \
                your reply.",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "status": { "const": "ok" },
                        "filename": { "type": "string" },
                        "mime": { "const": mime },
                        "size_bytes": {
                            "type": "integer",
                            "description": "The file's size, in bytes."
                        }
                    },
                    "required": ["status", "filename", "mime", "size_bytes"]
                },
                tool_error_output(&["artifact_filename_invalid", "reply_files_full"])
            ]
        }),
        errors: vec![
            ToolErrorCode::new(
                "artifact_filename_invalid",
                format!(
                    "`filename` is empty, longer than 128 bytes, has `/`, `\\`, `..` or a control \
                     character, or does not end in {extensions}. No file was made."
                ),
            ),
            ToolErrorCode::new(
                "reply_files_full",
                format!(
                    "The reply already carries {REPLY_FILES_MAX} files, the most one reply can. No \
                     file was made: say so in your reply, and make the rest in a later one."
                ),
            ),
        ],
    }
}

/// The `{status: "error"}` result of a tool whose failures carry these codes.
fn tool_error_output(codes: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": { "const": "error" },
            "error_code": { "enum": codes },
            "message": { "type": "string" },
            "retryable": { "type": "boolean" }
        },
        "required": ["status", "error_code", "message", "retryable"]
    })
}

/// A JSON schema for a list of table rows, each a list of cells.
fn rows_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "array", "items": { "type": "string" } },
        "description": description
    })
}

fn strings_schema(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": description })
}

/// The arguments of the two raster generators.
fn raster_properties() -> Value {
    json!({
        "bands": strings_schema("One horizontal band per entry, top to bottom."),
        "width": {
            "type": "integer",
            "minimum": 64,
            "maximum": 2400,
            "description": "Pixels; 1200 by default."
        },
        "height": {
            "type": "integer",
            "minimum": 64,
            "maximum": 2400,
            "description": "Pixels; 800 by default."
        }
    })
}

/// What the PNG and JPEG generators can draw.
const RASTER_TOOL_LIMITS: &[&str] = &[
    "It draws only horizontal bands of color, one per entry of `bands`, top to bottom: each \
     entry's text picks its color and is not drawn. Without `bands`, one color fills the image.",
    "1200 x 800 pixels by default; `width` and `height` are kept between 64 and 2400.",
];

#[async_trait]
impl AgentTool for GenerateCsvArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_csv_artifact",
            summary: "Make a CSV file from rows of text and send it to the user.",
            when_to_use: "When the user wants a table as a file, to open in a spreadsheet or \
                another program.",
            limits: &[
                "Every cell is written quoted. A cell that starts with `=`, `+`, `@`, a tab or a \
                 carriage return, or with `-` and is not a plain number such as `-12.5`, gets a \
                 `'` in front, so a spreadsheet takes it as text, never as a formula.",
                "`headers`, when given, is the first row.",
            ],
            extensions: ".csv",
            mime: "text/csv",
            properties: json!({
                "headers": strings_schema("The first row."),
                "rows": rows_schema("The rows, each a list of cells.")
            }),
            required: &["rows"],
        })
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateCsvArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_csv_artifact", err))?;
        let filename = args.filename.trim();

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
        Ok(keep_reply_file(
            "generate_csv_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateTextArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_text_artifact",
            summary: "Make a plain-text file and send it to the user.",
            when_to_use: "When the user wants text as a file rather than in the message: notes, \
                a log, a long answer to keep.",
            limits: &[],
            extensions: ".txt",
            mime: "text/plain",
            properties: json!({
                "content": { "type": "string", "description": "The file's text." }
            }),
            required: &["content"],
        })
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateTextArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_text_artifact", err))?;
        let filename = args.filename.trim();
        let artifact = match AiUserArtifact::from_text(filename, args.content) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_text_artifact", err)),
        };
        Ok(keep_reply_file(
            "generate_text_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateJsonArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_json_artifact",
            summary: "Make a JSON file and send it to the user.",
            when_to_use: "When the user wants structured data as a file, for a program to read.",
            limits: &[],
            extensions: ".json",
            mime: "application/json",
            properties: json!({
                "data": { "description": "Any JSON value: it is the file's content." },
                "pretty": {
                    "type": "boolean",
                    "description": "Indent the file; true by default."
                }
            }),
            required: &["data"],
        })
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
        Ok(keep_reply_file(
            "generate_json_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateMarkdownArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_markdown_artifact",
            summary: "Make a Markdown file and send it to the user.",
            when_to_use: "When the user wants formatted text (headings, lists, tables) as a file.",
            limits: &[],
            extensions: ".md or .markdown",
            mime: "text/markdown",
            properties: json!({
                "content": { "type": "string", "description": "The file's Markdown." }
            }),
            required: &["content"],
        })
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
        Ok(keep_reply_file(
            "generate_markdown_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateHtmlArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_html_artifact",
            summary: "Make an HTML file and send it to the user.",
            when_to_use: "When the user wants a page as a file, to open in a browser. For a link \
                to a page instead, use `publish_html_page` when you have it.",
            limits: &["It is a file, not a link: the user opens it from the attachment."],
            extensions: ".html or .htm",
            mime: "text/html",
            properties: json!({
                "content": { "type": "string", "description": "The whole HTML document." }
            }),
            required: &["content"],
        })
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GenerateHtmlArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_html_artifact", err))?;
        let filename = args.filename.trim();
        let artifact = match AiUserArtifact::from_html(filename, args.content) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_html_artifact", err)),
        };
        Ok(keep_reply_file(
            "generate_html_artifact",
            &self.files,
            artifact,
        ))
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
impl AgentTool for PublishHtmlTool {
    /// The link's life and the sandbox are SY.admin's and the edge's: `publish_artifact` without
    /// `expires_in_secs` (PUBLICATION_DEFAULT_EXPIRES_SECS, 24 h) and the edge's
    /// `sandboxed-html-v1` CSP.
    fn card() -> ToolCard {
        ToolCard {
            name: "publish_html_page".to_string(),
            summary: "Publish an HTML page at a public link, and get the link.".to_string(),
            when_to_use: "Only when the user asks for a page to open or share by link. To hand \
                over a file instead, use `generate_html_artifact`."
                .to_string(),
            limits: [
                "One self-contained HTML file, served under a sandbox: its inline scripts and \
                 styles run, but it loads nothing from the network (no fetch, no outside \
                 scripts, stylesheets, fonts or images); images and fonts only inline, as \
                 `data:` URLs; forms do not submit.",
                "The link is the key: anyone who has it can open the page, so put nothing \
                 private in it. It expires 24 hours after publishing, and you cannot extend it \
                 or take the page down earlier.",
                "Publishing takes up to two minutes. Your reply is the text you write after your \
                 last tool call; text written alongside a call is not sent. Give the link there: a \
                 reply without it gets it added, on a line of its own.",
            ]
            .map(String::from)
            .into(),
            present_when: "the node has its identity: it was launched with its ILK".to_string(),
            input: json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "minLength": 1,
                        "description": format!(
                            "The page's file name: {}.",
                            filename_rules(".html or .htm")
                        )
                    },
                    "content": {
                        "type": "string",
                        "description": "The whole HTML document, self-contained."
                    }
                },
                "required": ["filename", "content"],
                "additionalProperties": false
            }),
            output: json!({
                "description": "`{status: \"ok\", url, publication_id, expires_at}`: the public \
                    link, the publication's id and when it expires (Unix seconds).",
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "status": { "const": "ok" },
                            "url": { "type": "string", "description": "The public link." },
                            "publication_id": { "type": "string" },
                            "expires_at": {
                                "type": "integer",
                                "description": "Unix seconds."
                            },
                            "note": { "type": "string" }
                        },
                        "required": ["status", "url"]
                    },
                    tool_error_output(&["artifact_filename_invalid", "publish_failed"])
                ]
            }),
            errors: vec![
                ToolErrorCode::new(
                    "artifact_filename_invalid",
                    "`filename` is empty, longer than 128 bytes, has `/`, `\\`, `..` or a \
                     control character, or does not end in .html or .htm. Nothing was published.",
                ),
                ToolErrorCode::new(
                    "publish_failed",
                    "The page was not published; `message` says why. `retryable: true` when this \
                     node's blob store or SY.admin could not be reached; `false` when SY.admin \
                     refused it (the message carries its code, such as BLOB_TOO_LARGE) or the \
                     hive has no public address for links.",
                ),
            ],
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
        let blob = match fluxbee_sdk::blob::BlobToolkit::new(self.blob.clone()) {
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
                // The link is the key to the page: the journal gets its id, never the link (A-95).
                let publication_id = result
                    .payload
                    .get("publication_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let expires_at = result.payload.get("expires_at").and_then(Value::as_u64);
                tracing::info!(
                    tool = "publish_html_page",
                    publication_id,
                    expires_at,
                    "published HTML page"
                );
                Ok(json!({
                    "status": "ok",
                    "url": url,
                    "publication_id": result.payload.get("publication_id"),
                    "expires_at": result.payload.get("expires_at"),
                    "note": "Page published. Give this URL to the user in your reply, the text \
                        you write after your last tool call."
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
impl AgentTool for GeneratePdfArtifactTool {
    /// What `build_pdf_bytes` makes: Letter pages of Helvetica 12 pt text in WinAnsiEncoding,
    /// `PDF_LINES_PER_PAGE` lines a page, each wrapped to the page's width (a test holds the
    /// limits to those constants).
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_pdf_artifact",
            summary: "Make a PDF of text lines and send it to the user.",
            when_to_use: "When the user wants a text document as a PDF.",
            limits: &[
                "US Letter pages of Helvetica 12 pt: the title, a blank line, then one line per \
                 entry of `lines`. A line wider than the page wraps at a space (a longer word is \
                 cut), and the text goes on to new pages, 50 lines a page. No images, tables or \
                 styles.",
                "Western European text prints as written (Windows-1252: accents, ñ, ¿, ¡, €, \
                 curly quotes); any other character, such as Cyrillic, Chinese or an emoji, \
                 prints as `?`.",
                "With no title and no lines, the page says \"Documento generado por AI\".",
            ],
            extensions: ".pdf",
            mime: "application/pdf",
            properties: json!({
                "title": {
                    "type": "string",
                    "description": "The first line, followed by a blank one."
                },
                "lines": strings_schema(
                    "The document's lines, top to bottom; a line break inside an entry starts a \
                     new line."
                )
            }),
            required: &[],
        })
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GeneratePdfArtifactArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("generate_pdf_artifact", err))?;
        let filename = args.filename.trim();
        let bytes = build_pdf_bytes(args.title.as_deref(), &args.lines);
        let artifact = match AiUserArtifact::from_bytes(filename, "application/pdf", bytes) {
            Ok(artifact) => artifact,
            Err(err) => return Ok(invalid_tool_artifact_error("generate_pdf_artifact", err)),
        };
        Ok(keep_reply_file(
            "generate_pdf_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateXlsxArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_xlsx_artifact",
            summary: "Make an XLSX spreadsheet of text cells and send it to the user.",
            when_to_use: "When the user wants a table as an Excel file.",
            limits: &[
                "One sheet, and every cell is text: numbers stay text, with no formulas or \
                 formats. `headers`, when given, is the first row.",
                "`sheet_name` loses `:`, `\\`, `/`, `?`, `*`, `[` and `]` and is cut at 31 \
                 characters; it is Sheet1 without one.",
                "With no headers and no rows, the sheet holds one cell: \"Generated by AI\".",
            ],
            extensions: ".xlsx",
            mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            properties: json!({
                "headers": strings_schema("The first row."),
                "rows": rows_schema("The rows, each a list of cells."),
                "sheet_name": { "type": "string", "description": "Sheet1 by default." }
            }),
            required: &["rows"],
        })
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
        Ok(keep_reply_file(
            "generate_xlsx_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateDocxArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_docx_artifact",
            summary: "Make a DOCX document of plain paragraphs and send it to the user.",
            when_to_use: "When the user wants a text document to edit in Word.",
            limits: &[
                "Plain text in a fixed order: the title, the paragraphs, the bullets, then one \
                 table. No styles or headings: the title is a plain paragraph, and a bullet is a \
                 paragraph that starts with \"- \".",
                "With nothing to write, the document says \"Documento generado por AI\".",
            ],
            extensions: ".docx",
            mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            properties: json!({
                "title": { "type": "string", "description": "The first paragraph." },
                "paragraphs": strings_schema("One paragraph per entry."),
                "bullets": strings_schema("One \"- \" line per entry, after the paragraphs."),
                "table_headers": strings_schema("The table's first row."),
                "table_rows": rows_schema("The table's rows, each a list of cells.")
            }),
            required: &[],
        })
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
        Ok(keep_reply_file(
            "generate_docx_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GeneratePngArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_png_artifact",
            summary: "Make a PNG image of colored horizontal bands and send it to the user.",
            when_to_use: "Only when an image of colored bands will do, such as a placeholder: it \
                cannot draw text, charts or pictures.",
            limits: RASTER_TOOL_LIMITS,
            extensions: ".png",
            mime: "image/png",
            properties: raster_properties(),
            required: &[],
        })
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
        Ok(keep_reply_file(
            "generate_png_artifact",
            &self.files,
            artifact,
        ))
    }
}

#[async_trait]
impl AgentTool for GenerateJpegArtifactTool {
    fn card() -> ToolCard {
        artifact_tool_card(ArtifactToolSpec {
            name: "generate_jpeg_artifact",
            summary: "Make a JPEG image of colored horizontal bands and send it to the user.",
            when_to_use: "Only when an image of colored bands will do, such as a placeholder: it \
                cannot draw text, charts or pictures.",
            limits: RASTER_TOOL_LIMITS,
            extensions: ".jpg or .jpeg",
            mime: "image/jpeg",
            properties: raster_properties(),
            required: &[],
        })
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
        Ok(keep_reply_file(
            "generate_jpeg_artifact",
            &self.files,
            artifact,
        ))
    }
}

/// SY.timer's error code for a zone it does not know (go/sy-timer/main.go, `TIMER_NOW_IN`).
const SY_TIMER_INVALID_TIMEZONE: &str = "TIMER_INVALID_TIMEZONE";

impl GetCurrentTimeTool {
    /// The tool of the node named `node_name` (`AI.sales@worker1`): it asks SY.timer of that
    /// node's hive. `None` for a name without a hive.
    fn for_node(router: Arc<RouterDispatcher>, node_name: &str) -> Option<Self> {
        let (_, hive) = node_name.split_once('@')?;
        let timer = timer_node_name(hive).ok()?;
        Some(Self { router, timer })
    }
}

/// A `get_current_time` failure, in the shape every tool's has.
fn time_tool_error(code: &str, message: &str, retryable: bool) -> Value {
    json!({
        "status": "error",
        "error_code": code,
        "message": format!("get_current_time: {message}"),
        "retryable": retryable
    })
}

/// SY.timer's `TIMER_NOW_IN` answer, as the tool gives it: the instant in UTC and in the zone,
/// the zone, its abbreviation and the local weekday. `None` when the answer is not a time.
fn current_time_answer(now: &TimerNowInResult) -> Option<Value> {
    let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(now.now_utc_ms)?;
    let local = chrono::DateTime::parse_from_rfc3339(&now.now_local_iso).ok()?;
    Some(json!({
        "status": "ok",
        "utc": utc.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "local": now.now_local_iso,
        "timezone": now.tz,
        "abbreviation": now.tz_abbrev,
        // SY.timer gives the local date and not its weekday: the weekday is that date's.
        "weekday": local.format("%A").to_string(),
    }))
}

#[async_trait]
impl AgentTool for GetCurrentTimeTool {
    /// It asks with `TIMER_NOW_IN` (the SDK's `TimerClient`), which no policy gates: the router's
    /// system gate covers only its protected verbs, and the user policy only Resolve. SY.timer runs
    /// on the motherbee and every worker (docs/nodes/sy-timer.md), and the client tries a silent
    /// one three times, 1.4 s in all, before the tool answers `time_unavailable`.
    fn card() -> ToolCard {
        ToolCard {
            name: "get_current_time".to_string(),
            summary: "Get the current date and time, in UTC and in a time zone.".to_string(),
            when_to_use: "Whenever the answer depends on today's date or the time of day: what \
                day it is, how long until a date, whether something is due or open now. You do \
                not know the date: never guess it."
                .to_string(),
            limits: [
                "`timezone` is an IANA zone name, such as `America/Argentina/Buenos_Aires` or \
                 `Europe/Madrid`; without it, UTC. Use the user's zone when you know it.",
                "It only reads the time: it cannot schedule anything or wait.",
            ]
            .map(String::from)
            .into(),
            present_when: "always: SY.timer of the agent's hive answers it, and SY.timer runs on \
                the motherbee and on every worker"
                .to_string(),
            input: json!({
                "type": "object",
                "properties": {
                    "timezone": {
                        "type": "string",
                        "description": "An IANA time zone name, such as \
                            America/Argentina/Buenos_Aires. UTC when absent."
                    }
                },
                "additionalProperties": false
            }),
            output: json!({
                "description": "`{status: \"ok\", utc, local, timezone, abbreviation, weekday}`: \
                    the time in UTC and in the zone (ISO 8601, to the millisecond; the local one \
                    with its offset), the zone and its abbreviation, and the local weekday in \
                    English.",
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "status": { "const": "ok" },
                            "utc": {
                                "type": "string",
                                "description": "In UTC, such as 2026-10-27T15:04:05.123Z."
                            },
                            "local": {
                                "type": "string",
                                "description": "In the zone, with its offset, such as \
                                    2026-10-27T12:04:05.123-03:00."
                            },
                            "timezone": {
                                "type": "string",
                                "description": "The IANA zone of `local`."
                            },
                            "abbreviation": {
                                "type": "string",
                                "description": "The zone's abbreviation at that moment, such as \
                                    CET or -03."
                            },
                            "weekday": {
                                "type": "string",
                                "description": "The weekday of the local date, Monday to Sunday."
                            }
                        },
                        "required": [
                            "status", "utc", "local", "timezone", "abbreviation", "weekday"
                        ]
                    },
                    tool_error_output(&["invalid_timezone", "time_unavailable"])
                ]
            }),
            errors: vec![
                ToolErrorCode::new(
                    "invalid_timezone",
                    "`timezone` is not an IANA time zone name. Call again with one, or without \
                     it for UTC.",
                ),
                ToolErrorCode::new(
                    "time_unavailable",
                    "SY.timer did not answer: the agent's hive does not run it, or it is \
                     restarting. `retryable: true`: try once more shortly, and never guess the \
                     time.",
                ),
            ],
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: GetCurrentTimeArgs = serde_json::from_value(arguments)
            .map_err(|err| invalid_tool_args_error("get_current_time", err))?;
        let timezone = args
            .timezone
            .as_deref()
            .map(str::trim)
            .filter(|timezone| !timezone.is_empty())
            .unwrap_or("UTC");
        let config = TimerClientConfig {
            timer_target: Some(self.timer.clone()),
            ..TimerClientConfig::default()
        };
        let now: Result<TimerNowInResult, TimerClientError> = async {
            TimerClient::new_with_dispatcher(self.router.clone(), config)?
                .now_in(TimerNowInPayload {
                    tz: timezone.to_string(),
                    ..TimerNowInPayload::default()
                })
                .await
        }
        .await;
        match now {
            Ok(now) => Ok(current_time_answer(&now).unwrap_or_else(|| {
                tracing::warn!(
                    tool = "get_current_time",
                    timer = %self.timer,
                    now_utc_ms = now.now_utc_ms,
                    now_local_iso = %now.now_local_iso,
                    "SY.timer answered something that is not a time"
                );
                time_tool_error("time_unavailable", "SY.timer's answer is not a time", true)
            })),
            Err(TimerClientError::ServiceError { code, .. })
                if code == SY_TIMER_INVALID_TIMEZONE =>
            {
                Ok(time_tool_error(
                    "invalid_timezone",
                    &format!("'{timezone}' is not an IANA time zone name"),
                    false,
                ))
            }
            Err(err) => {
                tracing::warn!(
                    tool = "get_current_time",
                    timer = %self.timer,
                    error = %err,
                    "SY.timer did not answer"
                );
                Ok(time_tool_error(
                    "time_unavailable",
                    "SY.timer did not answer; try again shortly",
                    true,
                ))
            }
        }
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
        &["behavior.kind"]
    }
    fn optional_fields(&self) -> &'static [&'static str] {
        &[
            "behavior.model",
            "behavior.model_settings",
            "behavior.provider",
            "behavior.base_url",
            "behavior.multimodal",
            "runtime.immediate_memory",
            "runtime.cognitive_definition",
        ]
    }
    fn notes(&self) -> &'static [&'static str] {
        &[
            "The OpenAI/Anthropic credential is NOT config: the node reads its provider's key \
             (behavior.provider, or the hive's without it) from SY.vault by resource_type \
             openai|anthropic, in its tenant (owned by it, assigned to it, or its tenant's general \
             one).",
            "Without behavior.model the node runs on the hive's model for its provider (hive.yaml \
             ai.providers.<provider>.model); a model it names must be one of its provider.",
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

/// The runtime version the node runs: `_system.runtime_version` of its config.json, which the
/// orchestrator writes before it launches the node (and before it relaunches it at another
/// version). `None` for a node started by hand.
fn launched_runtime_version(node_name: &str, nodes_root: &Path) -> Option<String> {
    let path = managed_node_config_path_with_root(node_name, nodes_root).ok()?;
    let root: Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    root.get("_system")?
        .get("runtime_version")?
        .as_str()
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_string)
}

/// agent-tools-v1 T9 (FINDINGS A-94): `behavior.capabilities.multimodal` became
/// `behavior.multimodal`, and the contract refuses the old key. A config.json written before that
/// is rewritten once, here at boot, and the conversion is reported with the copy it kept.
fn convert_persisted_multimodal_key(node_name: &str, nodes_root: &Path) {
    let Ok(path) = managed_node_config_path_with_root(node_name, nodes_root) else {
        return;
    };
    match convert_capabilities_multimodal(&path) {
        Ok(Some(conversion)) => tracing::info!(
            node_name = %node_name,
            path = %path.display(),
            capabilities = %conversion.capabilities,
            original_copy = %conversion.original_copy.display(),
            "config.json: behavior.capabilities converted to behavior.multimodal (agent-tools-v1 T9)"
        ),
        Ok(None) => {}
        Err(err) => tracing::warn!(
            node_name = %node_name,
            path = %path.display(),
            error = %err,
            "config.json: behavior.capabilities could not be converted to behavior.multimodal; the config is refused"
        ),
    }
}

/// What the T9 conversion did to a config.json: the `behavior.capabilities` it found, and where it
/// kept the file as it was.
#[derive(Debug, Clone, PartialEq)]
struct MultimodalKeyConversion {
    capabilities: Value,
    original_copy: PathBuf,
}

/// Where the T9 conversion keeps a config.json as it found it, next to it: what a rollback to
/// 0.1.85 restores (docs/ai-nodes-deploy-runbook.md).
fn pre_conversion_copy_path(config_path: &Path) -> PathBuf {
    let mut name = config_path.file_name().unwrap_or_default().to_os_string();
    name.push(".pre-0.1.86");
    config_path.with_file_name(name)
}

/// Writes `content` as `path`, mode 0600, through a temp file beside it: created new, synced, then
/// renamed over `path`. Once the temp file exists, any error removes it, so a failure leaves
/// `path` as it was and nothing beside it.
fn write_file_0600(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".tmp.{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let written = file
        .write_all(content)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Rewrites a config.json whose `behavior.capabilities` is the old shape, `{"multimodal": <bool
/// or null>}` or `null`, as `behavior.multimodal` (absent when it said nothing). Before that it
/// keeps the file as it was in `pre_conversion_copy_path` (0600), replacing an older copy, and it
/// converts nothing when it cannot. Anything else under `capabilities`, or a value that meets a
/// `behavior.multimodal` already there, is left as it is for the contract to refuse: nothing is
/// dropped unseen.
fn convert_capabilities_multimodal(
    config_path: &Path,
) -> Result<Option<MultimodalKeyConversion>, String> {
    let raw = match fs::read_to_string(config_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read it: {err}")),
    };
    // Not JSON: the control plane's bootstrap reports it.
    let Ok(mut root) = serde_json::from_str::<Value>(&raw) else {
        return Ok(None);
    };
    // The flat shape, or the one wrapped in `config` (`extract_effective_config`).
    let behavior = if root.get("config").is_some() {
        root.get_mut("config")
            .and_then(|config| config.get_mut("behavior"))
    } else {
        root.get_mut("behavior")
    };
    let Some(behavior) = behavior.and_then(Value::as_object_mut) else {
        return Ok(None);
    };
    let Some(capabilities) = behavior.get("capabilities").cloned() else {
        return Ok(None);
    };
    let multimodal = match &capabilities {
        // What the old runner saved for a behavior it did not default (an `echo` node): nothing.
        Value::Null => Value::Null,
        Value::Object(old) if old.keys().all(|key| key == "multimodal") => {
            old.get("multimodal").cloned().unwrap_or(Value::Null)
        }
        _ => return Ok(None),
    };
    // A value to carry is a bool, and it does not meet a `multimodal` already there.
    if !multimodal.is_null() && (!multimodal.is_boolean() || behavior.contains_key("multimodal")) {
        return Ok(None);
    }
    behavior.remove("capabilities");
    if multimodal.is_boolean() {
        behavior.insert("multimodal".to_string(), multimodal);
    }
    let content = serde_json::to_string_pretty(&root).map_err(|err| err.to_string())?;
    let original_copy = pre_conversion_copy_path(config_path);
    write_file_0600(&original_copy, raw.as_bytes()).map_err(|err| {
        format!(
            "cannot keep it as it was in {}: {err}",
            original_copy.display()
        )
    })?;
    write_file_0600(config_path, content.as_bytes())
        .map_err(|err| format!("cannot replace it: {err}"))?;
    Ok(Some(MultimodalKeyConversion {
        capabilities,
        original_copy,
    }))
}

async fn run_managed_node(
    node: NodeSection,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let node_name = node.name.clone();
    let nodes_root = Path::new(DEFAULT_MANAGED_NODES_ROOT);
    convert_persisted_multimodal_key(&node_name, nodes_root);
    let runtime_version = launched_runtime_version(&node_name, nodes_root);
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
        runtime_version,
        router: Some(dispatcher.clone()),
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

/// The canonical API host of each provider: the only one a managed `behavior.base_url` may name.
fn provider_api_host(provider: AiProvider) -> &'static str {
    match provider {
        AiProvider::OpenAi => "api.openai.com",
        AiProvider::Anthropic => "api.anthropic.com",
    }
}

/// Audit A1 (credential exfiltration): `behavior.base_url` arrives on the NON-secret
/// CONFIG_SET plane but the vault-resolved provider api_key is then sent to it in the clear.
/// An unvalidated base_url turns config-write access into plaintext key exfiltration and
/// defeats Model D's plaintext confinement. A managed base_url may therefore ONLY be https
/// pointing at the canonical API host of the node's provider, whose key goes there: so it needs
/// `behavior.provider` (agent-tools-v1 T8), and the hive's provider changing later can never send
/// one provider's key to the other's host. An operator can deliberately relax the host check for a
/// proxy/gateway via the process env `FLUXBEE_AI_ALLOW_INSECURE_BASE_URL` — env is set by
/// whoever launches the node, NOT by a CONFIG_SET writer, so it is not reachable from the
/// config plane.
fn validate_managed_base_url(
    provider: Option<AiProvider>,
    base_url: &Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(raw) = base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(()); // no override -> provider default endpoint, always safe
    };
    let Some(provider) = provider else {
        return Err(
            "behavior.base_url needs behavior.provider: the URL is that provider's \
             endpoint, and its key is sent there"
                .into(),
        );
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
    let allowed = provider_api_host(provider);
    if !host.eq_ignore_ascii_case(allowed) {
        return Err(format!(
            "behavior.base_url host '{host}' is not the endpoint of behavior.provider {provider} \
             (allowed: {allowed}); set FLUXBEE_AI_ALLOW_INSECURE_BASE_URL in the node process env \
             to override for a trusted gateway"
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
            // Without it, the hive's model for the node's provider, at each request (A-104).
            let model = behavior.model.clone();
            if let Some(model) = model.as_deref() {
                require_nonempty("behavior.model", model)?;
            }

            let model_settings = extract_model_settings_from_effective_config(behavior);
            let base_url = behavior.base_url.clone();
            validate_managed_base_url(behavior.provider, &base_url)?;
            let immediate_memory = config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.immediate_memory.clone())
                .unwrap_or_default();
            let multimodal = behavior
                .multimodal
                .unwrap_or_else(default_multimodal_for_runtime);

            Ok(NodeBehavior::AiChat(AiChatRuntime {
                model,
                model_settings,
                provider: behavior.provider,
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
    let behavior = config.get("behavior");
    if behavior
        .and_then(|behavior| behavior.get("vault_key"))
        .is_some()
    {
        return Err(
            "behavior.vault_key is not accepted: ai.generic reads its provider's key from \
             SY.vault by resource_type (owned by it, assigned to it, or its tenant's general one)"
                .into(),
        );
    }
    // agent-tools-v1 T9: "tools" is the AI node's word for what an agent can do, and whether the
    // model takes images is `behavior.multimodal`.
    if behavior
        .and_then(|behavior| behavior.get("capabilities"))
        .is_some()
    {
        return Err(
            "behavior.capabilities is not accepted: whether the model receives images and \
             documents is behavior.multimodal (true or false)"
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
    if config.behavior.kind.eq_ignore_ascii_case("ai_chat") && config.behavior.multimodal.is_none()
    {
        config.behavior.multimodal = Some(default_multimodal_for_runtime());
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

/// The control verbs ai.generic answers. Each one runs only from the node that sends it
/// (`control_request`).
const CONTROL_VERBS: [&str; 6] = [
    MSG_NODE_STATUS_GET,
    MSG_NODE_HELP,
    MSG_CONFIG_SET,
    MSG_CONFIG_GET,
    MSG_PING,
    MSG_STATUS,
];

/// A message for the control plane, never for the model: a `system` or an `admin` one. Which of
/// them run is `handle_control_plane`'s to say.
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
        "message": "SY.vault has no usable key for this node's AI provider (resource_type openai or anthropic): none owned by or assigned to this node, and no general one in its tenant.",
        "retryable": true,
        "details": { "reason": reason }
    })
}

#[derive(Debug, Deserialize)]
struct HiveAiFile {
    #[serde(default)]
    ai: Option<HiveAiConfig>,
}

/// What one request of the node runs on: the API it speaks and the Vault resource type of its
/// key (the provider), and the model it asks.
#[derive(Debug, Clone, PartialEq)]
struct AiEngine {
    provider: AiProvider,
    model: String,
}

/// Why a request of the node has no provider or no model.
#[derive(Debug, Clone, PartialEq)]
enum AiEngineError {
    /// hive.yaml cannot be read, and the node leaves its provider or its model to the hive.
    Hive(String),
    /// Neither `behavior.model` nor the hive's model for the node's provider (FINDINGS A-104).
    NoModel(AiProvider),
}

impl std::fmt::Display for AiEngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hive(reason) => f.write_str(reason),
            Self::NoModel(provider) => write!(
                f,
                "no model for the provider {provider}: behavior.model is not set, and hive.yaml \
                 has no ai.providers.{provider}.model"
            ),
        }
    }
}

/// The node's provider and model as they stand now, each with where it comes from: its own
/// `behavior.provider` and `behavior.model`, or else the hive's (agent-tools-v1 T8, FINDINGS
/// A-104).
struct AiEngineInEffect {
    provider: Result<(AiProvider, &'static str), AiEngineError>,
    model: Result<(String, &'static str), AiEngineError>,
}

impl AiEngineInEffect {
    /// What a request runs on, or why it cannot run.
    fn for_request(self) -> Result<AiEngine, AiEngineError> {
        let (provider, _) = self.provider?;
        let (model, _) = self.model?;
        Ok(AiEngine { provider, model })
    }

    /// CONFIG_GET's `ai_provider` and `ai_model` (`in_effect_json`).
    fn config_json(&self) -> (Value, Value) {
        let provider = self.provider.as_ref();
        let model = self.model.as_ref();
        (
            in_effect_json(provider.map(|(provider, source)| (provider.as_str(), *source))),
            in_effect_json(model.map(|(model, source)| (model.as_str(), *source))),
        )
    }
}

/// Why CONFIG_SET refuses an ai_chat config whose model, or provider, cannot be resolved now: every
/// message would fail with `missing_ai_model` (or `hive_ai_config_unreadable`).
fn unresolved_model_reason(err: &AiEngineError) -> String {
    match err {
        AiEngineError::NoModel(provider) => format!(
            "no model for the provider {provider}: behavior.model is not set, and this hive's \
             hive.yaml has no ai.providers.{provider}.model; set behavior.model to a model of \
             {provider}"
        ),
        AiEngineError::Hive(reason) => format!(
            "it leaves its provider or its model to the hive, and {reason}; set behavior.provider \
             and behavior.model"
        ),
    }
}

/// Resolves the node's provider, then its model: `behavior.provider`, or the hive's; then
/// `behavior.model`, or the hive's model for that provider. hive.yaml is read as it is at this
/// moment, once, and only for what the node leaves to the hive: a node that names both never
/// reads it.
fn effective_ai_engine(ai: &AiChatRuntime, config_dir: &Path) -> AiEngineInEffect {
    let mut section = None;
    let mut hive = || {
        section
            .get_or_insert_with(|| hive_ai_section(config_dir))
            .clone()
    };
    let provider = match ai.provider {
        Some(provider) => Ok((provider, "behavior.provider")),
        None => hive().map(|section| (hive_ai_provider(section.as_ref()), "hive")),
    };
    let model = match (&ai.model, &provider) {
        (Some(model), _) => Ok((model.clone(), "behavior.model")),
        (None, Ok((provider, _))) => hive().and_then(|section| {
            hive_ai_model(section.as_ref(), *provider)
                .map(|model| (model, "hive"))
                .ok_or(AiEngineError::NoModel(*provider))
        }),
        (None, Err(err)) => Err(err.clone()),
    };
    AiEngineInEffect { provider, model }
}

/// hive.yaml's `ai` section, the one every SY.* AI consumer reads; `None` when it has none.
fn hive_ai_section(config_dir: &Path) -> Result<Option<HiveAiConfig>, AiEngineError> {
    let path = config_dir.join("hive.yaml");
    let raw = fs::read_to_string(&path)
        .map_err(|err| AiEngineError::Hive(format!("cannot read {}: {err}", path.display())))?;
    let hive: HiveAiFile = serde_yaml::from_str(&raw)
        .map_err(|err| AiEngineError::Hive(format!("invalid {}: {err}", path.display())))?;
    Ok(hive.ai)
}

/// The hive's provider: `ai.default_provider`, or OpenAI without an `ai` section, as for the SY
/// nodes (`HiveAiConfig::fallback`).
fn hive_ai_provider(section: Option<&HiveAiConfig>) -> AiProvider {
    section.map_or(HiveAiConfig::fallback().provider, |ai| ai.default_provider)
}

/// The hive's model for `provider`: `ai.providers.<provider>.model`; without an `ai` section, the
/// SY nodes' fallback, gpt-5.5, which is OpenAI's. `None` when the hive has none for it.
fn hive_ai_model(section: Option<&HiveAiConfig>, provider: AiProvider) -> Option<String> {
    let Some(ai) = section else {
        let fallback = HiveAiConfig::fallback();
        return (fallback.provider == provider).then_some(fallback.model);
    };
    let configured = match provider {
        AiProvider::OpenAi => ai.providers.openai.as_ref(),
        AiProvider::Anthropic => ai.providers.anthropic.as_ref(),
    };
    configured
        .map(|config| config.model.trim())
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// One part of the engine in CONFIG_GET: `{name, source}`, or `{name: null, source: "hive",
/// error}` when the hive cannot give it.
fn in_effect_json(part: Result<(&str, &str), &AiEngineError>) -> Value {
    match part {
        Ok((name, source)) => json!({"name": name, "source": source}),
        Err(err) => json!({"name": Value::Null, "source": "hive", "error": err.to_string()}),
    }
}

/// What the sender gets for a request with no provider or no model. Either is the operator's to
/// fix, in the node's config or in hive.yaml, so neither is retryable.
fn ai_engine_error_payload(err: &AiEngineError) -> Value {
    match err {
        AiEngineError::NoModel(provider) => json!({
            "type": "error",
            "code": "missing_ai_model",
            "message": format!(
                "This AI node has no model for its provider, {provider}: set behavior.model in \
                 its config, or ai.providers.{provider}.model in the hive's hive.yaml."
            ),
            "retryable": false
        }),
        AiEngineError::Hive(reason) => json!({
            "type": "error",
            "code": "hive_ai_config_unreadable",
            "message": "This AI node takes its provider or its model from the hive, and the \
                hive's hive.yaml cannot be read.",
            "retryable": false,
            "details": { "reason": reason }
        }),
    }
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
                if (status == 400 || status == 404 || status == 422)
                    && extract_openai_error_param(msg)
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
    use base64::Engine as _;
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
        let openai = Some(AiProvider::OpenAi);
        let base_url = |url: &str| Some(url.to_string());
        assert!(validate_managed_base_url(openai, &None).is_ok());
        assert!(validate_managed_base_url(openai, &Some(String::new())).is_ok());
        assert!(validate_managed_base_url(openai, &base_url("https://api.openai.com/v1")).is_ok());
        assert!(validate_managed_base_url(
            Some(AiProvider::Anthropic),
            &base_url("https://api.anthropic.com")
        )
        .is_ok());
        assert!(validate_managed_base_url(openai, &base_url("http://api.openai.com")).is_err()); // not https
        assert!(
            validate_managed_base_url(openai, &base_url("https://attacker.example/v1")).is_err()
        );
        assert!(
            validate_managed_base_url(openai, &base_url("https://api.openai.com.evil.com"))
                .is_err()
        );
        // userinfo bypass attempts (the real host is the attacker) MUST be rejected:
        assert!(validate_managed_base_url(
            openai,
            &base_url("https://api.openai.com:x@evil.com/v1")
        )
        .is_err());
        assert!(
            validate_managed_base_url(openai, &base_url("https://api.openai.com@evil.com"))
                .is_err()
        );
        assert!(validate_managed_base_url(openai, &base_url("https://api.openai.com:443")).is_ok());
        // explicit port OK
    }

    /// T8: a base_url is the endpoint of the node's own provider, whose key is sent there: it
    /// needs `behavior.provider`, and the other provider's host is refused.
    #[test]
    fn a_base_url_is_the_endpoint_of_the_nodes_own_provider() {
        let anthropic_url = Some("https://api.anthropic.com/v1/messages".to_string());
        let err = validate_managed_base_url(None, &anthropic_url)
            .expect_err("a base_url without a provider");
        assert!(
            err.to_string()
                .contains("behavior.base_url needs behavior.provider"),
            "{err}"
        );
        let err = validate_managed_base_url(Some(AiProvider::OpenAi), &anthropic_url)
            .expect_err("anthropic's host for an openai node");
        assert!(
            err.to_string()
                .contains("is not the endpoint of behavior.provider openai"),
            "{err}"
        );
        assert!(validate_managed_base_url(Some(AiProvider::Anthropic), &anthropic_url).is_ok());
        let err = validate_managed_base_url(
            Some(AiProvider::Anthropic),
            &Some("https://api.openai.com/v1/responses".to_string()),
        )
        .expect_err("openai's host for an anthropic node");
        assert!(
            err.to_string().contains("allowed: api.anthropic.com"),
            "{err}"
        );

        // Through the config: the same rule, before the node takes the behavior.
        let config = |behavior: Value| {
            build_behavior_from_effective_config(
                &parse_effective_config_doc(&json!({ "behavior": behavior })).expect("parses"),
            )
        };
        assert!(config(json!({
            "kind": "ai_chat",
            "model": "claude-sonnet-5-5",
            "base_url": "https://api.anthropic.com/v1/messages"
        }))
        .is_err());
        assert!(config(json!({
            "kind": "ai_chat",
            "model": "claude-sonnet-5-5",
            "provider": "anthropic",
            "base_url": "https://api.anthropic.com/v1/messages"
        }))
        .is_ok());
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

    /// A NODE_STATUS_GET from the orchestrator of the node's hive, the one that sends it.
    fn sample_request() -> Message {
        Message {
            routing: Routing {
                src: "SY.orchestrator@motherbee".to_string(),
                src_l2_name: Some("SY.orchestrator@motherbee".to_string()),
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
            runtime_version: Some("0.1.86".to_string()),
            router: None,
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
                "limits": ["Do not offer tools you do not have."]
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

    /// A request from SY.admin@motherbee, the one that sends CONFIG_GET and CONFIG_SET.
    fn admin_request(verb: &str) -> Message {
        let mut msg = sample_request();
        msg.routing.src_l2_name = Some("SY.admin@motherbee".to_string());
        msg.meta.msg = Some(verb.to_string());
        msg
    }

    fn config_set_request(config: Value) -> Message {
        let mut msg = admin_request(MSG_CONFIG_SET);
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
    #[allow(clippy::await_holding_lock)] // the env lock spans the whole test, awaits included
    async fn ai_chat_missing_api_key_returns_error_payload_instead_of_fatal() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::remove_var("OPENAI_API_KEY_MISSING_FOR_TEST");
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            // Its own provider and model: the key is looked up without reading hive.yaml.
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::AiChat(AiChatRuntime {
                model: Some("gpt-5.5".to_string()),
                model_settings: ModelSettings::default(),
                provider: Some(AiProvider::OpenAi),
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
            .build_tool_registry(&BehaviorContext::default(), &Arc::default())
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
            .build_tool_registry(
                &BehaviorContext {
                    thread_id: Some("legacy-thread-1".to_string()),
                    src_ilk: Some("ilk:11111111-1111-4111-8111-111111111111".to_string()),
                    ..BehaviorContext::default()
                },
                &Arc::default(),
            )
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

    // --- The file generators and the reply (FINDINGS A-97, A-98, A-99) ---

    /// The one file a call kept for the reply.
    fn kept_file(files: &ReplyFiles) -> AiUserArtifact {
        let mut kept = files.take();
        assert_eq!(kept.len(), 1, "one call, one file");
        kept.remove(0)
    }

    /// A generator keeps its file for the reply and answers the model only its name, type and
    /// size.
    #[tokio::test]
    async fn generate_pdf_artifact_tool_keeps_a_pdf_for_the_reply() {
        let files = Arc::new(ReplyFiles::default());
        let answer = GeneratePdfArtifactTool {
            files: files.clone(),
        }
        .call(json!({
            "filename": "reporte.pdf",
            "title": "Reporte",
            "lines": ["Linea 1", "Linea 2"]
        }))
        .await
        .expect("pdf tool should succeed");
        let file = kept_file(&files);
        assert_eq!(file.filename, "reporte.pdf");
        assert_eq!(file.mime, "application/pdf");
        assert!(file.bytes.starts_with(b"%PDF-1.4"));
        assert_eq!(
            answer,
            json!({
                "status": "ok",
                "filename": "reporte.pdf",
                "mime": "application/pdf",
                "size_bytes": file.bytes.len()
            })
        );
    }

    #[tokio::test]
    async fn generate_textual_artifact_tools_keep_their_mime_and_filenames() {
        let files = Arc::new(ReplyFiles::default());
        GenerateTextArtifactTool {
            files: files.clone(),
        }
        .call(json!({"filename": "nota.txt", "content": "hola mundo"}))
        .await
        .expect("text tool should succeed");
        GenerateJsonArtifactTool {
            files: files.clone(),
        }
        .call(json!({"filename": "payload.json", "data": {"ok": true, "count": 2}}))
        .await
        .expect("json tool should succeed");
        GenerateMarkdownArtifactTool {
            files: files.clone(),
        }
        .call(json!({"filename": "readme.md", "content": "# Hola\n\n- uno\n- dos"}))
        .await
        .expect("markdown tool should succeed");
        GenerateHtmlArtifactTool {
            files: files.clone(),
        }
        .call(json!({"filename": "page.html", "content": "<h1>Hola</h1><p>Mundo</p>"}))
        .await
        .expect("html tool should succeed");

        let kept = files.take();
        let names = kept
            .iter()
            .map(|file| (file.filename.as_str(), file.mime.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("nota.txt", "text/plain"),
                ("payload.json", "application/json"),
                ("readme.md", "text/markdown"),
                ("page.html", "text/html"),
            ]
        );
        let text = |index: usize| String::from_utf8(kept[index].bytes.clone()).expect("utf8");
        assert_eq!(text(0), "hola mundo");
        assert!(text(1).contains("\"ok\": true"));
        assert!(text(2).contains("# Hola"));
        assert!(text(3).contains("<h1>Hola</h1>"));
    }

    /// Excel and LibreOffice run a quoted cell that starts with `=`, `+`, `-` or `@` as a formula:
    /// such a cell gets a `'` in front (OWASP, CSV injection), and so does one that starts with a
    /// tab or a carriage return; a plain negative number stays a number.
    #[tokio::test]
    async fn a_csv_cell_never_runs_as_a_formula() {
        let files = Arc::new(ReplyFiles::default());
        GenerateCsvArtifactTool {
            files: files.clone(),
        }
        .call(json!({
            "filename": "datos.csv",
            "headers": ["=HYPERLINK(\"http://x\")", "saldo"],
            "rows": [
                ["+1+1", "-12"],
                ["@SUM(A1)", "-1.234,56"],
                ["\tinvisible", "-12.5"],
                ["\rreturn", "-2+3+cmd|' /C calc'!A0"],
                ["-", "-.5"],
                ["plain", "say \"hi\""],
                ["a=b", "12-3"]
            ]
        }))
        .await
        .expect("csv tool");
        let csv = String::from_utf8(kept_file(&files).bytes).expect("utf8");
        let expected = [
            r#""'=HYPERLINK(""http://x"")","saldo""#,
            r#""'+1+1","-12""#,
            r#""'@SUM(A1)","-1.234,56""#,
            "\"'\tinvisible\",\"-12.5\"",
            "\"'\rreturn\",\"'-2+3+cmd|' /C calc'!A0\"",
            r#""'-","'-.5""#,
            r#""plain","say ""hi""""#,
            r#""a=b","12-3""#,
        ];
        assert_eq!(csv, format!("{}\n", expected.join("\n")));
    }

    #[tokio::test]
    async fn generate_xlsx_artifact_tool_keeps_a_zip_with_workbook_parts() {
        let files = Arc::new(ReplyFiles::default());
        GenerateXlsxArtifactTool {
            files: files.clone(),
        }
        .call(json!({
            "filename": "tabla.xlsx",
            "headers": ["nombre", "edad"],
            "rows": [["Ana", "30"], ["Luis", "41"]]
        }))
        .await
        .expect("xlsx tool should succeed");
        let file = kept_file(&files);
        assert_eq!(file.filename, "tabla.xlsx");
        assert_eq!(
            file.mime,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        let mut zip =
            zip::ZipArchive::new(Cursor::new(file.bytes)).expect("xlsx should be a zip archive");
        zip.by_name("[Content_Types].xml")
            .expect("xlsx content types");
        zip.by_name("xl/workbook.xml").expect("xlsx workbook");
        zip.by_name("xl/worksheets/sheet1.xml").expect("xlsx sheet");
    }

    #[tokio::test]
    async fn generate_docx_artifact_tool_keeps_a_zip_with_document_parts() {
        let files = Arc::new(ReplyFiles::default());
        GenerateDocxArtifactTool {
            files: files.clone(),
        }
        .call(json!({
            "filename": "nota.docx",
            "title": "Nota",
            "paragraphs": ["Parrafo 1", "Parrafo 2"],
            "bullets": ["Uno", "Dos"]
        }))
        .await
        .expect("docx tool should succeed");
        let file = kept_file(&files);
        assert_eq!(file.filename, "nota.docx");
        assert_eq!(
            file.mime,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        let mut zip =
            zip::ZipArchive::new(Cursor::new(file.bytes)).expect("docx should be a zip archive");
        zip.by_name("[Content_Types].xml")
            .expect("docx content types");
        zip.by_name("word/document.xml").expect("docx document");
        zip.by_name("word/styles.xml").expect("docx styles");
    }

    #[tokio::test]
    async fn generate_png_and_jpeg_artifact_tools_keep_decodable_images() {
        let files = Arc::new(ReplyFiles::default());
        GeneratePngArtifactTool {
            files: files.clone(),
        }
        .call(json!({
            "filename": "grafico.png",
            "bands": ["rojo", "verde", "azul"],
            "width": 320,
            "height": 180
        }))
        .await
        .expect("png tool should succeed");
        let png = kept_file(&files);
        GenerateJpegArtifactTool {
            files: files.clone(),
        }
        .call(json!({
            "filename": "grafico.jpg",
            "bands": ["uno", "dos"],
            "width": 320,
            "height": 180
        }))
        .await
        .expect("jpeg tool should succeed");
        let jpeg = kept_file(&files);

        let png = image::load_from_memory_with_format(&png.bytes, image::ImageFormat::Png)
            .expect("png should decode");
        let jpeg = image::load_from_memory_with_format(&jpeg.bytes, image::ImageFormat::Jpeg)
            .expect("jpeg should decode");
        assert_eq!((png.width(), png.height()), (320, 180));
        assert_eq!((jpeg.width(), jpeg.height()), (320, 180));
    }

    /// The content of each stream of a PDF `build_pdf_bytes` wrote, each checked against its
    /// `/Length`.
    fn pdf_streams(pdf: &str) -> Vec<&str> {
        pdf.match_indices("<< /Length ")
            .map(|(at, prefix)| {
                let (length, rest) = pdf[at + prefix.len()..]
                    .split_once(" >>\nstream\n")
                    .expect("a stream");
                let length: usize = length.parse().expect("its length");
                assert!(
                    rest[length..].starts_with("\nendstream"),
                    "the stream's /Length is its own"
                );
                &rest[..length]
            })
            .collect()
    }

    /// A-97: the font is Helvetica in WinAnsiEncoding, and each line goes in as its bytes there,
    /// every one from 0x80 an octal escape: the file stays ASCII, and the PDF prints Spanish.
    #[test]
    fn a_pdf_prints_spanish() {
        let pdf = build_pdf_bytes(
            Some("Información"),
            &["¿Año (nuevo)? ¡Sí! Ñandú, pingüino, €5 — «ok»".to_string()],
        );
        assert!(pdf.is_ascii());
        let pdf = String::from_utf8(pdf).expect("ASCII");
        assert!(pdf.contains(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
        ));
        let streams = pdf_streams(&pdf);
        assert_eq!(streams.len(), 1);
        assert!(
            streams[0].contains("(Informaci\\363n) Tj\nT*\n() Tj\n"),
            "{}",
            streams[0]
        );
        assert!(
            streams[0].contains(
                "(\\277A\\361o \\(nuevo\\)? \\241S\\355! \\321and\\372, ping\\374ino, \\2005 \
                 \\227 \\253ok\\273) Tj"
            ),
            "{}",
            streams[0]
        );

        let empty = String::from_utf8(build_pdf_bytes(None, &[])).expect("ASCII");
        assert!(pdf_streams(&empty)[0].contains("(Documento generado por AI) Tj"));
    }

    /// A character Windows-1252 does not have prints as `?`; a tab is a space, a control character
    /// a `?`, and a line break inside an entry starts a new line.
    #[test]
    fn a_character_outside_windows_1252_prints_as_a_question_mark() {
        assert_eq!(pdf_line_bytes("Привет 日本 ✓ 😀 ok"), b"?????? ?? ? ? ok");
        assert_eq!(pdf_line_bytes("a\tb\u{7}c\u{85}"), b"a b?c?");
        assert_eq!(
            pdf_line_bytes("“curly” ‘quotes’ – …"),
            b"\x93curly\x94 \x91quotes\x92 \x96 \x85"
        );

        let pdf = String::from_utf8(build_pdf_bytes(None, &["Привет 😀\nsegunda".to_string()]))
            .expect("ASCII");
        let stream = pdf_streams(&pdf)[0];
        assert!(
            stream.contains("(?????? ?) Tj\nT*\n(segunda) Tj"),
            "{stream}"
        );
    }

    /// A line wider than the page wraps at a space, measured with Helvetica's widths, and the text
    /// goes on to new pages: every page is in the page tree, and every object where the
    /// cross-reference table says.
    #[test]
    fn a_long_text_wraps_and_takes_several_pages() {
        let width = |line: &[u8]| {
            line.iter()
                .map(|byte| u32::from(HELVETICA_WIDTHS[usize::from(*byte)]))
                .sum::<u32>()
        };
        let words = (0..60)
            .map(|word| format!("palabra{word}"))
            .collect::<Vec<_>>()
            .join(" ");
        let wrapped = wrap_pdf_line(words.as_bytes());
        assert!(wrapped.len() > 1);
        for line in &wrapped {
            assert!(width(line) <= PDF_LINE_WIDTH_UNITS);
            assert!(!line.starts_with(b" ") && !line.ends_with(b" "));
        }
        assert_eq!(wrapped.join(&b' '), words.as_bytes());
        // Narrow letters fit more than wide ones: the widths, not a count of characters.
        assert!(wrap_pdf_line("i".repeat(150).as_bytes()).len() == 1);
        assert!(wrap_pdf_line("m".repeat(150).as_bytes()).len() > 1);
        // A word wider than a line is cut.
        let long = "m".repeat(200);
        let cut = wrap_pdf_line(long.as_bytes());
        assert!(cut.len() > 1);
        assert!(cut.iter().all(|line| width(line) <= PDF_LINE_WIDTH_UNITS));
        assert_eq!(cut.concat(), long.as_bytes());

        // A title, its blank line and 120 lines: 122 lines, on pages of 50.
        let lines = (1..=120)
            .map(|line| format!("Línea {line}"))
            .collect::<Vec<_>>();
        let pdf = String::from_utf8(build_pdf_bytes(Some("Informe"), &lines)).expect("ASCII");
        assert!(pdf.contains("<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R] /Count 3 >>"));
        let streams = pdf_streams(&pdf);
        let lines_per_page = streams
            .iter()
            .map(|stream| stream.matches(") Tj").count())
            .collect::<Vec<_>>();
        assert_eq!(lines_per_page, [PDF_LINES_PER_PAGE, PDF_LINES_PER_PAGE, 22]);
        assert!(streams[2].contains("(L\\355nea 120) Tj"));

        let xref = pdf.rfind("\nxref\n").expect("xref") + 1;
        assert!(pdf.ends_with(&format!("startxref\n{xref}\n%%EOF\n")));
        let offsets = pdf[xref..]
            .lines()
            .skip(3)
            .take_while(|line| line.ends_with(" n "))
            .map(|line| line[..10].parse::<usize>().expect("an offset"))
            .collect::<Vec<_>>();
        assert_eq!(offsets.len(), 3 + 2 * 3);
        for (index, offset) in offsets.iter().enumerate() {
            assert!(pdf[*offset..].starts_with(&format!("{} 0 obj\n", index + 1)));
        }

        // The card says what the code does.
        let card = GeneratePdfArtifactTool::card();
        assert!(card.limits[0].contains(&format!("{PDF_LINES_PER_PAGE} lines a page")));
    }

    /// A model that asks for `calls` in its first turn, with `call_text` alongside them, then ends
    /// with `final_text`; it keeps what each turn showed it.
    struct ScriptedModel {
        calls: Vec<(&'static str, Value)>,
        call_text: Option<&'static str>,
        final_text: Option<&'static str>,
        seen: Mutex<Vec<fluxbee_ai_sdk::FunctionModelTurnRequest>>,
    }

    impl ScriptedModel {
        fn new(calls: Vec<(&'static str, Value)>, final_text: Option<&'static str>) -> Self {
            Self {
                calls,
                call_text: None,
                final_text,
                seen: Mutex::default(),
            }
        }

        fn with_call_text(self, call_text: &'static str) -> Self {
            Self {
                call_text: Some(call_text),
                ..self
            }
        }

        /// What the model got back from its calls: the tool results of its second turn.
        fn results(&self) -> Vec<Value> {
            let seen = self.seen.lock().expect("seen");
            assert_eq!(seen.len(), 2, "one turn of calls, one to answer");
            seen[1]
                .items
                .iter()
                .filter_map(|item| match item {
                    fluxbee_ai_sdk::FunctionLoopItem::ToolResult { result } => {
                        Some(result.output.clone())
                    }
                    _ => None,
                })
                .collect()
        }
    }

    #[async_trait]
    impl FunctionCallingModel for ScriptedModel {
        async fn run_turn(
            &self,
            request: fluxbee_ai_sdk::FunctionModelTurnRequest,
        ) -> fluxbee_ai_sdk::Result<fluxbee_ai_sdk::FunctionModelTurnResponse> {
            let first = {
                let mut seen = self.seen.lock().expect("seen");
                seen.push(request);
                seen.len() == 1
            };
            let tool_calls = if first {
                self.calls
                    .iter()
                    .enumerate()
                    .map(
                        |(index, (name, arguments))| fluxbee_ai_sdk::FunctionToolCall {
                            call_id: format!("call-{index}"),
                            response_id: None,
                            name: name.to_string(),
                            arguments: arguments.clone(),
                        },
                    )
                    .collect()
            } else {
                Vec::new()
            };
            let text = if first {
                self.call_text
            } else {
                self.final_text
            };
            Ok(fluxbee_ai_sdk::FunctionModelTurnResponse {
                assistant_text: text.map(str::to_string),
                tool_calls,
                tokens_used: 0,
            })
        }
    }

    /// One message's tool loop with `model`, through the tools every message gets.
    async fn tool_loop_reply(model: &ScriptedModel) -> AiBehaviorOutput {
        let ctx = BehaviorContext::default();
        let files = Arc::new(ReplyFiles::default());
        let tools = test_node()
            .build_tool_registry(&ctx, &files)
            .expect("registry");
        run_tool_loop_with(model, &tools, &files)
            .await
            .expect("a reply")
    }

    /// One message's tool loop with `model`, through `tools`, whose generators keep their files in
    /// `files`.
    async fn run_tool_loop_with(
        model: &ScriptedModel,
        tools: &FunctionToolRegistry,
        files: &ReplyFiles,
    ) -> Option<AiBehaviorOutput> {
        test_node()
            .run_tool_loop(
                model,
                tools,
                files,
                &chat_runtime(None, None),
                &BehaviorContext::default(),
                "send both".to_string(),
                None,
                "send both",
            )
            .await
            .expect("the tool loop")
    }

    /// A-98, A-99: a turn that makes two files. The model gets back each file's name, type and
    /// size, never its bytes, and the reply is the model's own text with both files, in the order
    /// they were made, down to the text/v1 payload an IO node receives.
    #[tokio::test]
    async fn the_reply_is_the_models_text_with_every_file_and_no_bytes_reach_the_model() {
        let model = ScriptedModel::new(
            vec![
                (
                    "generate_csv_artifact",
                    json!({"filename": "ventas.csv", "rows": [["enero", "10"]]}),
                ),
                (
                    "generate_pdf_artifact",
                    json!({"filename": "informe.pdf", "title": "Informe", "lines": ["Año 2026"]}),
                ),
            ],
            Some("Aquí tienes la tabla y el informe."),
        );
        let output = tool_loop_reply(&model).await;
        let AiBehaviorOutput::Final(reply) = &output else {
            panic!("a reply with files: {output:?}");
        };
        assert_eq!(
            reply.text.as_deref(),
            Some("Aquí tienes la tabla y el informe.")
        );
        let names = reply
            .artifacts
            .iter()
            .map(|file| file.filename.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["ventas.csv", "informe.pdf"]);

        assert_eq!(
            model.results(),
            [
                json!({
                    "status": "ok",
                    "filename": "ventas.csv",
                    "mime": "text/csv",
                    "size_bytes": reply.artifacts[0].bytes.len()
                }),
                json!({
                    "status": "ok",
                    "filename": "informe.pdf",
                    "mime": "application/pdf",
                    "size_bytes": reply.artifacts[1].bytes.len()
                }),
            ]
        );
        let seen = serde_json::to_string(&*model.seen.lock().expect("seen")).expect("serialize");
        for file in &reply.artifacts {
            let base64 = base64::engine::general_purpose::STANDARD.encode(&file.bytes);
            assert!(
                !seen.contains(&base64),
                "{} reached the model",
                file.filename
            );
        }
        assert!(!seen.contains("%PDF") && !seen.contains("bytes_base64"));

        // Both files reach the payload an IO node gets, with the model's text.
        let blob_root = std::env::temp_dir().join(format!("ai-generic-reply-{}", Uuid::new_v4()));
        let payload = fluxbee_ai_sdk::build_ai_behavior_response_with_options(
            output.clone(),
            &fluxbee_ai_sdk::TextResponseOptions {
                blob_root: Some(blob_root.clone()),
                ..fluxbee_ai_sdk::TextResponseOptions::default()
            },
        )
        .expect("the reply's payload");
        let text = fluxbee_sdk::payload::TextV1Payload::from_value(&payload).expect("text/v1");
        assert_eq!(
            text.content.as_deref(),
            Some("Aquí tienes la tabla y el informe.")
        );
        let attachments = text
            .attachments
            .iter()
            .map(|blob| (blob.filename_original.as_str(), blob.mime.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            attachments,
            [
                ("ventas.csv", "text/csv"),
                ("informe.pdf", "application/pdf")
            ]
        );
        let _ = fs::remove_dir_all(blob_root);
    }

    /// A reply carries at most `REPLY_FILES_MAX` files, the IO nodes' limit for one message: a
    /// call past it makes no file and tells the model, and the reply keeps the first ones.
    #[tokio::test]
    async fn the_reply_files_cap_holds() {
        let calls = (0..=REPLY_FILES_MAX)
            .map(|index| {
                (
                    "generate_text_artifact",
                    json!({"filename": format!("nota-{index}.txt"), "content": "hola"}),
                )
            })
            .collect();
        let model = ScriptedModel::new(calls, Some("Listo."));
        let AiBehaviorOutput::Final(reply) = tool_loop_reply(&model).await else {
            panic!("a reply with files");
        };
        assert_eq!(reply.artifacts.len(), REPLY_FILES_MAX);
        assert_eq!(
            reply.artifacts.last().map(|file| file.filename.clone()),
            Some(format!("nota-{}.txt", REPLY_FILES_MAX - 1))
        );
        let results = model.results();
        assert!(results[..REPLY_FILES_MAX]
            .iter()
            .all(|result| result["status"] == json!("ok")));
        let refused = &results[REPLY_FILES_MAX];
        assert_eq!(refused["error_code"], json!("reply_files_full"));
        assert_eq!(refused["retryable"], json!(false));
    }

    /// A model that writes no text after its files gets a fixed line naming them; a loop that made
    /// no file leaves the reply to the model's text, and one with no text either has no reply.
    #[test]
    fn a_reply_without_the_models_text_names_its_files() {
        let file = |name: &str| AiUserArtifact::new(b"x".to_vec(), "text/plain", name);
        assert_eq!(
            tool_loop_output(Some("hola".to_string()), Vec::new(), &[]),
            Some(AiBehaviorOutput::text("hola"))
        );
        assert_eq!(
            tool_loop_output(Some(" ".to_string()), Vec::new(), &[]),
            None
        );
        assert_eq!(tool_loop_output(None, Vec::new(), &[]), None);
        let Some(AiBehaviorOutput::Final(one)) = tool_loop_output(None, vec![file("a.txt")], &[])
        else {
            panic!("a reply with its file");
        };
        assert_eq!(
            one.text.as_deref(),
            Some("Aca esta el archivo solicitado: a.txt")
        );
        let Some(AiBehaviorOutput::Final(two)) = tool_loop_output(
            Some("  ".to_string()),
            vec![file("a.txt"), file("b.txt")],
            &[],
        ) else {
            panic!("a reply with its files");
        };
        assert_eq!(
            two.text.as_deref(),
            Some("Aca estan los archivos solicitados: a.txt, b.txt")
        );
    }

    /// The reply is the model's final text, and text it writes alongside a tool call is not sent;
    /// but when the final text is blank, the last text it wrote alongside a call is the reply,
    /// before the fixed line, with files or without.
    #[tokio::test]
    async fn a_blank_final_text_falls_back_to_the_text_written_alongside_a_call() {
        let note = || {
            vec![(
                "generate_text_artifact",
                json!({"filename": "nota.txt", "content": "hola"}),
            )]
        };
        let text_of = |output: AiBehaviorOutput| match output {
            AiBehaviorOutput::Final(reply) => (reply.text, reply.artifacts.len()),
            AiBehaviorOutput::Text(text) => (Some(text), 0),
        };

        for final_text in [None, Some(""), Some("  \n")] {
            let model = ScriptedModel::new(note(), final_text).with_call_text("Aquí va tu nota.");
            assert_eq!(
                text_of(tool_loop_reply(&model).await),
                (Some("Aquí va tu nota.".to_string()), 1),
                "{final_text:?}"
            );
        }
        let model = ScriptedModel::new(note(), Some("Listo.")).with_call_text("Un momento.");
        assert_eq!(
            text_of(tool_loop_reply(&model).await),
            (Some("Listo.".to_string()), 1),
            "the final text is the reply"
        );
        let model = ScriptedModel::new(note(), None);
        assert_eq!(
            text_of(tool_loop_reply(&model).await),
            (
                Some("Aca esta el archivo solicitado: nota.txt".to_string()),
                1
            ),
            "no text at all: the fixed line"
        );

        // Without a file: a call that fails, and its text is the reply.
        let model = ScriptedModel::new(vec![("no_such_tool", json!({}))], None)
            .with_call_text("Lo busco y te aviso.");
        assert_eq!(
            text_of(tool_loop_reply(&model).await),
            (Some("Lo busco y te aviso.".to_string()), 0)
        );
        let model = ScriptedModel::new(vec![("no_such_tool", json!({}))], None);
        let ctx = BehaviorContext::default();
        let files = Arc::new(ReplyFiles::default());
        let tools = test_node()
            .build_tool_registry(&ctx, &files)
            .expect("registry");
        assert_eq!(
            run_tool_loop_with(&model, &tools, &files).await,
            None,
            "no text, no file, no link: the caller asks once more, without tools"
        );
    }

    /// Stands in for `publish_html_page`: answers each call with `answer`, as the tool does.
    struct PublishedPage {
        answer: Value,
    }

    #[async_trait]
    impl fluxbee_ai_sdk::FunctionTool for PublishedPage {
        fn definition(&self) -> fluxbee_ai_sdk::FunctionToolDefinition {
            PublishHtmlTool::card().model_definition()
        }

        async fn call(&self, _arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
            Ok(self.answer.clone())
        }
    }

    /// The link of a page published in the turn always reaches the person: a reply that does not
    /// carry it gets it on a line of its own, from what the tool answered; one that carries it is
    /// left as it is, and a failed publish adds nothing.
    #[tokio::test]
    async fn the_link_of_a_published_page_always_reaches_the_reply() {
        const LINK: &str = "https://fluxbee.example/public/k3y";
        let page = || {
            vec![(
                "publish_html_page",
                json!({"filename": "page.html", "content": "<html>hola</html>"}),
            )]
        };
        let reply = |answer: Value, model: ScriptedModel| async move {
            let mut tools = FunctionToolRegistry::new();
            tools
                .register(Arc::new(PublishedPage { answer }))
                .expect("register");
            let files = ReplyFiles::default();
            run_tool_loop_with(&model, &tools, &files).await
        };
        let published = json!({
            "status": "ok",
            "url": LINK,
            "publication_id": "pub-7",
            "expires_at": 1_790_000_000u64
        });
        let text = |text: String| Some(AiBehaviorOutput::text(text));

        assert_eq!(
            reply(
                published.clone(),
                ScriptedModel::new(page(), Some("Listo.\n"))
            )
            .await,
            text(format!("Listo.\n{LINK}"))
        );
        assert_eq!(
            reply(
                published.clone(),
                ScriptedModel::new(
                    page(),
                    Some("Tu página: https://fluxbee.example/public/k3y")
                )
            )
            .await,
            text(format!("Tu página: {LINK}"))
        );
        assert_eq!(
            reply(
                published.clone(),
                ScriptedModel::new(page(), None).with_call_text("La publico.")
            )
            .await,
            text(format!("La publico.\n{LINK}"))
        );
        assert_eq!(
            reply(published, ScriptedModel::new(page(), None)).await,
            text(LINK.to_string()),
            "no text: the link is the reply"
        );
        let failed = json!({
            "status": "error",
            "error_code": "publish_failed",
            "message": "publish_html_page: could not reach the publishing service",
            "retryable": true
        });
        assert_eq!(
            reply(
                failed,
                ScriptedModel::new(page(), Some("No pude publicarla."))
            )
            .await,
            text("No pude publicarla.".to_string())
        );

        // With files: after the fixed line, and each link once.
        let file = AiUserArtifact::new(b"x".to_vec(), "text/plain", "a.txt");
        let links = [LINK.to_string()];
        let Some(AiBehaviorOutput::Final(with_file)) = tool_loop_output(None, vec![file], &links)
        else {
            panic!("a reply with its file");
        };
        assert_eq!(
            with_file.text,
            Some(format!("Aca esta el archivo solicitado: a.txt\n{LINK}"))
        );
        let ok = |url: &str| FunctionLoopItem::ToolResult {
            result: fluxbee_ai_sdk::FunctionToolResult {
                call_id: "call-0".to_string(),
                response_id: None,
                name: "publish_html_page".to_string(),
                arguments: json!({}),
                output: json!({"status": "ok", "url": url}),
                is_error: false,
            },
        };
        assert_eq!(
            published_links(&[
                ok(LINK),
                ok(LINK),
                ok("https://fluxbee.example/public/other")
            ]),
            [LINK, "https://fluxbee.example/public/other"]
        );
    }

    /// A file named as an earlier one of the same reply takes its place, in the earlier one's
    /// position, and takes no other of the reply's `REPLY_FILES_MAX` places: the person never gets
    /// one name twice.
    #[tokio::test]
    async fn a_file_named_as_an_earlier_one_takes_its_place() {
        let text_file = |name: &str, content: &str| {
            (
                "generate_text_artifact",
                json!({"filename": name, "content": content}),
            )
        };
        let model = ScriptedModel::new(
            vec![
                text_file("a.txt", "uno"),
                (
                    "generate_csv_artifact",
                    json!({"filename": "b.csv", "rows": [["1"]]}),
                ),
                text_file("a.txt", "dos"),
            ],
            Some("Listo."),
        );
        let AiBehaviorOutput::Final(reply) = tool_loop_reply(&model).await else {
            panic!("a reply with files");
        };
        let files = reply
            .artifacts
            .iter()
            .map(|file| (file.filename.as_str(), file.bytes.as_slice()))
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], ("a.txt", b"dos".as_slice()));
        assert_eq!(files[1].0, "b.csv");
        assert!(model
            .results()
            .iter()
            .all(|result| result["status"] == json!("ok")));

        // A full reply still takes a file of a name it carries, and no other.
        let mut calls = (0..REPLY_FILES_MAX)
            .map(|index| {
                (
                    "generate_text_artifact",
                    json!({"filename": format!("nota-{index}.txt"), "content": "uno"}),
                )
            })
            .collect::<Vec<_>>();
        calls.push(text_file("nota-0.txt", "dos"));
        calls.push(text_file("otra.txt", "tres"));
        let model = ScriptedModel::new(calls, Some("Listo."));
        let AiBehaviorOutput::Final(reply) = tool_loop_reply(&model).await else {
            panic!("a reply with files");
        };
        assert_eq!(reply.artifacts.len(), REPLY_FILES_MAX);
        assert_eq!(reply.artifacts[0].filename, "nota-0.txt");
        assert_eq!(reply.artifacts[0].bytes, b"dos");
        let results = model.results();
        assert_eq!(results[REPLY_FILES_MAX]["status"], json!("ok"));
        assert_eq!(
            results[REPLY_FILES_MAX + 1]["error_code"],
            json!("reply_files_full")
        );
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

    /// A-104: `behavior.kind` is a complete ai_chat config. A model the node names is its own,
    /// and an empty one is refused.
    #[test]
    fn an_ai_chat_config_may_leave_its_model_to_the_hive_and_names_no_key() {
        let behavior = |behavior: Value| {
            build_behavior_from_effective_config(
                &parse_effective_config_doc(&json!({ "behavior": behavior })).expect("parses"),
            )
        };
        let Ok(NodeBehavior::AiChat(minimal)) = behavior(json!({"kind": "ai_chat"})) else {
            panic!("kind alone is a complete ai_chat config");
        };
        assert_eq!((minimal.provider, minimal.model), (None, None));
        let Ok(NodeBehavior::AiChat(own)) =
            behavior(json!({"kind": "ai_chat", "model": "gpt-5.5"}))
        else {
            panic!("an ai_chat config with its model");
        };
        assert_eq!(own.model.as_deref(), Some("gpt-5.5"));
        let err = behavior(json!({"kind": "ai_chat", "model": " "})).expect_err("an empty model");
        assert!(
            err.to_string().contains("behavior.model must not be empty"),
            "{err}"
        );

        let with_key_name = json!({
            "behavior": {"kind": "ai_chat", "model": "gpt-5.5", "vault_key": "ai/test"}
        });
        let err =
            parse_effective_config_doc(&with_key_name).expect_err("a Vault key name is not config");
        assert!(err
            .to_string()
            .contains("behavior.vault_key is not accepted"));

        // T8 (FINDINGS A-93): the node may name its provider, one of the two the SDK speaks.
        for provider in ["openai", "anthropic"] {
            let doc = parse_effective_config_doc(&json!({
                "behavior": {"kind": "ai_chat", "model": "a-model", "provider": provider}
            }))
            .expect("a provider is config");
            let Ok(NodeBehavior::AiChat(ai)) = build_behavior_from_effective_config(&doc) else {
                panic!("{provider}: an ai_chat behavior");
            };
            assert_eq!(ai.provider.map(AiProvider::as_str), Some(provider));
        }
        for provider in ["gemini", "OpenAI", ""] {
            let with_provider = json!({
                "behavior": {"kind": "ai_chat", "model": "a-model", "provider": provider}
            });
            assert!(
                parse_effective_config_doc(&with_provider).is_err(),
                "{provider}"
            );
        }

        let legacy = parse_effective_config_doc(&json!({
            "behavior": {"kind": "openai_chat", "model": "gpt-4.1-mini"}
        }))
        .expect("document shape parses");
        assert!(build_behavior_from_effective_config(&legacy).is_err());
    }

    /// T8, A-104: the provider is the node's or the hive's, and the model the node's or the hive's
    /// for that provider; without an `ai` section the hive's are OpenAI and gpt-5.5, as for the SY
    /// nodes. hive.yaml is read only for what the node leaves to it.
    #[test]
    fn the_model_is_the_nodes_or_the_hives_for_its_provider() {
        let dir = std::env::temp_dir().join(format!("ai-generic-hive-ai-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        let hive = |yaml: &str| {
            fs::write(dir.join("hive.yaml"), format!("hive_id: motherbee\n{yaml}"))
                .expect("write hive.yaml")
        };
        let engine = |provider: Option<AiProvider>, model: Option<&str>| {
            effective_ai_engine(&chat_runtime(provider, model), &dir).for_request()
        };
        let runs = |provider: AiProvider, model: &str| {
            Ok(AiEngine {
                provider,
                model: model.to_string(),
            })
        };

        hive("ai:\n  default_provider: anthropic\n  providers:\n    openai:\n      model: gpt-5.5\n    anthropic:\n      model: claude-sonnet-5-5\n");
        assert_eq!(
            engine(None, None),
            runs(AiProvider::Anthropic, "claude-sonnet-5-5")
        );
        assert_eq!(
            engine(Some(AiProvider::OpenAi), None),
            runs(AiProvider::OpenAi, "gpt-5.5")
        );
        assert_eq!(
            engine(None, Some("claude-opus-5")),
            runs(AiProvider::Anthropic, "claude-opus-5")
        );

        // The hive names its provider without a model for it.
        hive("ai:\n  default_provider: anthropic\n");
        assert_eq!(
            engine(None, None),
            Err(AiEngineError::NoModel(AiProvider::Anthropic))
        );
        assert_eq!(
            engine(None, Some("claude-opus-5")),
            runs(AiProvider::Anthropic, "claude-opus-5")
        );
        hive("ai:\n  default_provider: openai\n  providers:\n    openai:\n      model: \" \"\n");
        assert_eq!(
            engine(None, None),
            Err(AiEngineError::NoModel(AiProvider::OpenAi))
        );

        // No ai section: the SY nodes' OpenAI and gpt-5.5, and no model for Anthropic.
        hive("");
        assert_eq!(engine(None, None), runs(AiProvider::OpenAi, "gpt-5.5"));
        let err = engine(Some(AiProvider::Anthropic), None).expect_err("no model for anthropic");
        assert_eq!(err, AiEngineError::NoModel(AiProvider::Anthropic));
        assert_eq!(
            err.to_string(),
            "no model for the provider anthropic: behavior.model is not set, and hive.yaml has no \
             ai.providers.anthropic.model"
        );

        // Without hive.yaml, only what the node leaves to the hive fails.
        fs::remove_file(dir.join("hive.yaml")).expect("remove hive.yaml");
        assert!(matches!(
            engine(None, Some("gpt-5.5")),
            Err(AiEngineError::Hive(reason)) if reason.contains("hive.yaml")
        ));
        assert!(matches!(
            engine(Some(AiProvider::OpenAi), None),
            Err(AiEngineError::Hive(_))
        ));
        assert_eq!(
            engine(Some(AiProvider::OpenAi), Some("gpt-5.5")),
            runs(AiProvider::OpenAi, "gpt-5.5")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A-104: a request whose node names no model, on a hive with none for its provider, fails
    /// before any key or model call, with an error that names both settings.
    #[tokio::test]
    async fn a_request_without_a_model_names_both_settings() {
        let dir = std::env::temp_dir().join(format!("ai-generic-no-model-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(
            dir.join("hive.yaml"),
            "hive_id: motherbee\nai:\n  default_provider: openai\n  providers:\n    openai:\n      model: gpt-5.5\n",
        )
        .expect("write hive.yaml");
        let mut node = test_node();
        node.config_dir = dir.clone();
        node.control_plane.write().await.current_state = NodeLifecycleState::Configured;
        *node.behavior.write().await = Some(ai_chat(Some(AiProvider::Anthropic), None));

        let mut msg = sample_user_request_with_context(
            json!({ "thread_id": "sim-thread-1" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        msg.meta.thread_id = Some("thread:sim-thread-1".to_string());
        let reply = node
            .on_message(msg)
            .await
            .expect("on_message")
            .expect("a reply");
        assert_eq!(reply.payload["code"], json!("missing_ai_model"));
        assert_eq!(reply.payload["retryable"], json!(false));
        let message = reply.payload["message"].as_str().expect("message");
        assert!(
            message.contains("behavior.model") && message.contains("ai.providers.anthropic.model"),
            "{message}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    // --- The tools' cards and the catalog (docs/agent-tools-v1.md §3) ---

    /// How the repository keeps the catalog equal to the code: this command writes both files.
    const TOOL_CATALOG_REGENERATE: &str = "UPDATE_AGENT_TOOLS_CATALOG=1 cargo test -p \
        fluxbee-ai-nodes --bin ai_node_runner the_tool_catalog_matches_the_cards";

    /// docs/reference/agent-tools.json: the catalog file, as the package ships it but without a
    /// version.
    fn tool_catalog_json(catalog: &ToolCatalog) -> String {
        format!(
            "{}\n",
            serde_json::to_string_pretty(catalog).expect("serialize the catalog")
        )
    }

    /// docs/reference/agent-tools.md: the catalog as a page to read.
    fn tool_catalog_markdown(catalog: &ToolCatalog) -> String {
        let cell = |text: &str| text.replace('|', "\\|");
        let pretty = |value: &Value| serde_json::to_string_pretty(value).expect("serialize");
        let mut out = format!("# Tool catalog: `{}`\n\n", catalog.runtime);
        out.push_str(
            "Generated from the code; do not edit it. A test in `nodes/ai/ai-generic` checks this \
             page and [agent-tools.json](agent-tools.json) against the tools' cards, and CI runs \
             it. After changing a card, write both again:\n\n",
        );
        out.push_str(&format!("```bash\n{TOOL_CATALOG_REGENERATE}\n```\n\n"));
        out.push_str(
            "- **What it is:** every base tool of the runtime, as its card declares it \
             ([agent-tools-v1](../agent-tools-v1.md)). Every AI agent loads every base tool; a \
             message gets one when its *present when* holds.\n\
             - **The same cards** are what the model receives (the summary, when to use the \
             tool, its limits and what it returns, with the input schema), what an AI node \
             answers to `NODE_HELP`, and `dist/runtimes/ai.generic/<version>/tools.json` in an \
             installation. The repository's `agent-tools.json` has `runtime_version: null`: the \
             package writes the version it ships.\n\
             - **A failure** is a result `{\"status\": \"error\", \"error_code\", \"message\", \
             \"retryable\"}` with one of the tool's codes. Arguments that do not fit the input \
             schema, or a failure inside the tool, come back instead as an error result: a text \
             that names the problem.\n\n",
        );
        out.push_str("| Tool | What it does | Present when |\n| --- | --- | --- |\n");
        for card in &catalog.tools {
            out.push_str(&format!(
                "| [`{name}`](#{name}) | {} | {} |\n",
                cell(&card.summary),
                cell(&card.present_when),
                name = card.name,
            ));
        }
        for card in &catalog.tools {
            out.push_str(&format!("\n## `{}`\n\n{}\n\n", card.name, card.summary));
            out.push_str(&format!("- **When to use:** {}\n", card.when_to_use));
            out.push_str(&format!("- **Present when:** {}\n", card.present_when));
            if !card.limits.is_empty() {
                out.push_str("- **Limits:**\n");
                for limit in &card.limits {
                    out.push_str(&format!("  - {limit}\n"));
                }
            }
            if let Some(returns) = card.returns() {
                out.push_str(&format!("- **Returns:** {returns}\n"));
            }
            if card.errors.is_empty() {
                out.push_str("- **Errors:** no error code of its own.\n");
            } else {
                out.push_str("- **Errors:**\n");
                for error in &card.errors {
                    out.push_str(&format!("  - `{}`: {}\n", error.code, error.meaning));
                }
            }
            out.push_str(&format!(
                "\nInput:\n\n```json\n{}\n```\n\nOutput:\n\n```json\n{}\n```\n",
                pretty(&card.input),
                pretty(&card.output)
            ));
        }
        out
    }

    /// The catalog in the repository is the code's: CI's Rust tests are the parity check. With
    /// UPDATE_AGENT_TOOLS_CATALOG set, the test writes both files instead.
    #[test]
    fn the_tool_catalog_matches_the_cards() {
        let catalog = tool_catalog(None);
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let files = [
            (
                "docs/reference/agent-tools.json",
                tool_catalog_json(&catalog),
            ),
            (
                "docs/reference/agent-tools.md",
                tool_catalog_markdown(&catalog),
            ),
        ];
        if std::env::var_os("UPDATE_AGENT_TOOLS_CATALOG").is_some() {
            for (name, content) in &files {
                fs::write(repo.join(name), content).expect("write the catalog");
            }
            return;
        }
        for (name, expected) in &files {
            let written = fs::read_to_string(repo.join(name)).unwrap_or_default();
            if written == *expected {
                continue;
            }
            let line = written
                .lines()
                .zip(expected.lines())
                .position(|(written, expected)| written != expected)
                .unwrap_or_else(|| written.lines().count().min(expected.lines().count()))
                + 1;
            panic!(
                "{name} is not the tools' cards in the code (they differ at line {line}). Write \
                 it again, and commit it:\n  {TOOL_CATALOG_REGENERATE}"
            );
        }
    }

    /// Every card says all it has to, and a message that meets every `present_when` gets every
    /// base tool, each exactly as its card renders it: the catalog is what the model receives.
    #[tokio::test]
    async fn a_message_gets_each_base_tool_as_its_card_declares_it() {
        let cards = base_tool_cards();
        for card in &cards {
            assert_eq!(card.validate(), Ok(()), "{}", card.name);
        }

        let (dispatcher, _harness) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "AI.generic@motherbee",
            build_ai_generic_rpc_profile().expect("profile"),
        );
        let mut node = test_node();
        node.router = Some(dispatcher.clone());
        node.vault = vault_client_for(
            dispatcher.clone(),
            "AI.generic@motherbee",
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        node.thread_state_store = Some(Arc::new(LanceDbThreadStateStore::new(
            std::env::temp_dir().join(format!("ai-generic-tools-{}", Uuid::new_v4())),
        )));
        let every_condition = BehaviorContext {
            src_ilk: Some("ilk:22222222-2222-4222-8222-222222222222".to_string()),
            ..BehaviorContext::default()
        };
        let sorted = |mut definitions: Vec<fluxbee_ai_sdk::FunctionToolDefinition>| {
            definitions.sort_by(|a, b| a.name.cmp(&b.name));
            definitions
        };
        assert_eq!(
            sorted(
                node.build_tool_registry(&every_condition, &Arc::default())
                    .expect("registry")
                    .definitions()
            ),
            sorted(cards.iter().map(ToolCard::model_definition).collect())
        );

        // No interlocutor and no identity: the state tools and publish_html_page are not there;
        // the generators and get_current_time always are.
        let mut bare = test_node();
        bare.router = Some(dispatcher);
        let mut names = bare
            .build_tool_registry(&BehaviorContext::default(), &Arc::default())
            .expect("registry")
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        names.sort();
        let mut always = cards
            .iter()
            .filter(|card| card.present_when.starts_with("always"))
            .map(|card| card.name.clone())
            .collect::<Vec<_>>();
        always.sort();
        assert_eq!(always.len(), 11);
        assert_eq!(names, always);
    }

    #[tokio::test]
    async fn node_help_answers_the_tool_catalog_in_any_state() {
        let node = test_node();
        let mut request = sample_request();
        request.meta.msg = Some(MSG_NODE_HELP.to_string());
        let response = node
            .handle_control_plane(request.clone())
            .await
            .expect("control plane")
            .expect("NODE_HELP_RESPONSE");
        assert_eq!(response.meta.msg.as_deref(), Some(MSG_NODE_HELP_RESPONSE));
        assert!(matches!(
            response.routing.dst,
            Destination::Unicast(ref dst) if dst == &request.routing.src
        ));
        let help = &response.payload;
        assert_eq!(help["status"], json!("ok"));
        assert_eq!(help["node_name"], json!("AI.generic@motherbee"));
        assert_eq!(help["runtime"], json!("ai.generic"));
        assert_eq!(help["runtime_version"], json!("0.1.86"));
        let tools: Vec<ToolCard> =
            serde_json::from_value(help["tools"].clone()).expect("the cards");
        assert_eq!(tools, base_tool_cards());
        // The node is UNCONFIGURED, and the answer leaves room in one frame for the tools to come.
        let frame = serde_json::to_vec(&response).expect("serialize").len();
        assert!(
            frame < fluxbee_sdk::socket::connection::MAX_FRAME_SIZE / 2,
            "{frame} bytes"
        );
    }

    #[test]
    fn the_runtime_version_is_the_one_the_orchestrator_launched() {
        let root = std::env::temp_dir().join(format!("ai-generic-version-{}", Uuid::new_v4()));
        let node = "AI.sales@motherbee";
        let path = managed_node_config_path_with_root(node, &root).expect("path");
        assert_eq!(launched_runtime_version(node, &root), None);
        fs::create_dir_all(path.parent().expect("dir")).expect("node dir");
        fs::write(
            &path,
            r#"{"_system":{"runtime":"ai.generic","runtime_version":"0.1.86"},"behavior":{}}"#,
        )
        .expect("config.json");
        assert_eq!(
            launched_runtime_version(node, &root).as_deref(),
            Some("0.1.86")
        );
        let _ = fs::remove_dir_all(root);
    }

    // --- T8 and A-104: the AI provider and the model per node (FINDINGS A-93, A-104) ---

    fn chat_runtime(provider: Option<AiProvider>, model: Option<&str>) -> AiChatRuntime {
        AiChatRuntime {
            model: model.map(str::to_string),
            model_settings: ModelSettings::default(),
            provider,
            base_url: None,
            immediate_memory: ImmediateMemorySection::default(),
            multimodal: true,
        }
    }

    fn ai_chat(provider: Option<AiProvider>, model: Option<&str>) -> NodeBehavior {
        NodeBehavior::AiChat(chat_runtime(provider, model))
    }

    /// CONFIG_GET's `ai_provider` and `ai_model`.
    async fn engine_in_effect(node: &GenericAiNode) -> (Value, Value) {
        let config = node
            .handle_control_plane(admin_request(MSG_CONFIG_GET))
            .await
            .expect("control plane")
            .expect("CONFIG_RESPONSE")
            .payload;
        (config["ai_provider"].clone(), config["ai_model"].clone())
    }

    /// CONFIG_GET says which provider and which model the node uses now, and why: its own, or the
    /// hive's.
    #[tokio::test]
    async fn config_get_shows_the_provider_and_the_model_in_effect() {
        let dir = std::env::temp_dir().join(format!("ai-generic-provider-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(
            dir.join("hive.yaml"),
            "hive_id: motherbee\nai:\n  default_provider: anthropic\n  providers:\n    anthropic:\n      model: claude-sonnet-5-5\n",
        )
        .expect("write hive.yaml");
        let mut node = test_node();
        node.config_dir = dir.clone();
        let named = |name: &str, source: &str| json!({"name": name, "source": source});

        assert_eq!(
            engine_in_effect(&node).await,
            (Value::Null, Value::Null),
            "no behavior yet"
        );
        *node.behavior.write().await = Some(ai_chat(None, None));
        assert_eq!(
            engine_in_effect(&node).await,
            (
                named("anthropic", "hive"),
                named("claude-sonnet-5-5", "hive")
            )
        );
        *node.behavior.write().await = Some(ai_chat(Some(AiProvider::OpenAi), Some("gpt-5.5")));
        assert_eq!(
            engine_in_effect(&node).await,
            (
                named("openai", "behavior.provider"),
                named("gpt-5.5", "behavior.model")
            )
        );
        *node.behavior.write().await = Some(ai_chat(Some(AiProvider::OpenAi), None));
        let (provider, model) = engine_in_effect(&node).await;
        assert_eq!(provider, named("openai", "behavior.provider"));
        assert_eq!(
            model,
            json!({
                "name": null,
                "source": "hive",
                "error": "no model for the provider openai: behavior.model is not set, and \
                    hive.yaml has no ai.providers.openai.model"
            })
        );

        fs::write(dir.join("hive.yaml"), "hive_id: motherbee\nai: [\n").expect("break it");
        *node.behavior.write().await = Some(ai_chat(Some(AiProvider::OpenAi), Some("gpt-5.5")));
        assert_eq!(
            engine_in_effect(&node).await,
            (
                named("openai", "behavior.provider"),
                named("gpt-5.5", "behavior.model")
            ),
            "a node's own provider and model do not read hive.yaml"
        );
        *node.behavior.write().await = Some(ai_chat(None, None));
        let (provider, model) = engine_in_effect(&node).await;
        for broken in [provider, model] {
            assert_eq!(broken["name"], Value::Null);
            assert!(broken["error"].as_str().is_some(), "{broken}");
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// A hive whose hive.yaml lists only its default provider's model, as `add_hive` writes a
    /// worker's: Anthropic, with claude-sonnet-5-5.
    fn anthropic_only_hive(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ai-generic-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(
            dir.join("hive.yaml"),
            "hive_id: worker1\nrole: worker\nai:\n  default_provider: anthropic\n  providers:\n    anthropic:\n      model: \"claude-sonnet-5-5\"\n",
        )
        .expect("write hive.yaml");
        dir
    }

    /// A model the hive cannot supply is refused at CONFIG_SET, not at every message after it: a
    /// provider other than the hive's default without `behavior.model`, on a hive that lists only
    /// the default's model, or a config that leaves its model or its provider to an unreadable
    /// hive.yaml. Nothing is applied.
    #[tokio::test]
    async fn config_set_refuses_a_model_the_hive_cannot_supply() {
        let dir = anthropic_only_hive("unresolved-model");
        let mut node = test_node();
        node.config_dir = dir.clone();
        let refused = |response: Message| {
            assert_eq!(response.payload["ok"], json!(false), "{}", response.payload);
            assert_eq!(response.payload["error"]["code"], json!("invalid_config"));
            response.payload["error"]["message"]
                .as_str()
                .expect("message")
                .to_string()
        };

        let message = refused(
            control_answer(
                &node,
                config_set_request(json!({"behavior": {"kind": "ai_chat", "provider": "openai"}})),
            )
            .await,
        );
        assert!(
            message.contains("behavior.model") && message.contains("ai.providers.openai.model"),
            "{message}"
        );

        fs::remove_file(dir.join("hive.yaml")).expect("remove hive.yaml");
        for behavior in [
            json!({"kind": "ai_chat", "provider": "openai"}),
            json!({"kind": "ai_chat", "model": "gpt-5.5"}),
        ] {
            let message = refused(
                control_answer(&node, config_set_request(json!({ "behavior": behavior }))).await,
            );
            assert!(
                message.contains("behavior.model") && message.contains("hive.yaml"),
                "{message}"
            );
        }
        assert!(node.behavior.read().await.is_none());
        assert_eq!(
            node.control_plane.read().await.current_state,
            NodeLifecycleState::Unconfigured
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// CONFIG_SET's answer to a config it holds shows the provider and the model it runs on, as
    /// CONFIG_GET does.
    #[tokio::test]
    async fn config_set_answers_the_provider_and_the_model_in_effect() {
        let dir = anthropic_only_hive("config-set-engine");
        let mut node = test_node();
        node.config_dir = dir.clone();
        let config = json!({"behavior": {"kind": "ai_chat"}});
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
            state.config_version = 2;
            state.effective_config = Some(config.clone());
        }
        *node.behavior.write().await = Some(ai_chat(None, None));

        // The same config_version applies nothing, and answers what holds (writing a new one
        // would write the node's config.json under /var/lib/fluxbee).
        let answer = control_answer(&node, config_set_request(config))
            .await
            .payload;
        assert_eq!(answer["ok"], json!(true), "{answer}");
        let in_effect = (answer["ai_provider"].clone(), answer["ai_model"].clone());
        assert_eq!(
            in_effect,
            (
                json!({"name": "anthropic", "source": "hive"}),
                json!({"name": "claude-sonnet-5-5", "source": "hive"})
            )
        );
        assert_eq!(in_effect, engine_in_effect(&node).await);
        let _ = fs::remove_dir_all(dir);
    }

    /// The key is the node's provider's, looked up only in the node's tenant.
    #[tokio::test]
    async fn the_key_is_looked_up_for_the_nodes_provider_in_its_tenant() {
        let (dispatcher, mut harness) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "AI.generic@motherbee",
            build_ai_generic_rpc_profile().expect("profile"),
        );
        let mut node = test_node();
        node.self_tenant_id = Some("tnt:33333333-3333-4333-8333-333333333333".to_string());
        node.vault = vault_client_for(
            dispatcher,
            "AI.generic@motherbee",
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        let node = Arc::new(node);
        let lookup = tokio::spawn({
            let node = node.clone();
            async move { node.resolve_ai_credential(AiProvider::Anthropic).await }
        });
        let request = harness
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("a vault request");
        lookup.abort();
        assert_eq!(request.meta.msg.as_deref(), Some("VAULT_LIST"));
        assert_eq!(
            request.payload["filter"]["resource_type"],
            json!("anthropic")
        );
        assert_eq!(
            request.payload["filter"]["tenant_id"],
            json!("tnt:33333333-3333-4333-8333-333333333333")
        );
    }

    // --- get_current_time: SY.timer of the agent's own hive ---

    /// What SY.timer, or the router for it, sends back to `request`.
    fn timer_reply(request: &Message, msg: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "uuid-of-sy-timer".to_string(),
                src_l2_name: Some("SY.timer@worker1".to_string()),
                dst: Destination::Unicast(request.routing.src.clone()),
                ttl: 16,
                trace_id: request.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: fluxbee_sdk::protocol::SYSTEM_KIND.to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    /// The tool asked with `arguments`, and its answer once a stubbed SY.timer has answered its
    /// request with `answer`, a TIMER_RESPONSE or the router's UNREACHABLE; with the request.
    async fn time_asked(arguments: Value, msg: &str, answer: Value) -> (Value, Message) {
        let (router, mut timer) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "AI.sales@worker1",
            build_ai_generic_rpc_profile().expect("profile"),
        );
        let tool = GetCurrentTimeTool::for_node(router, "AI.sales@worker1").expect("the tool");
        let sy_timer = async {
            let request = timer.next_outgoing().await.expect("a request to SY.timer");
            timer
                .inject(timer_reply(&request, msg, answer))
                .await
                .expect("SY.timer's answer");
            request
        };
        let (asked, request) = tokio::join!(tool.call(arguments), sy_timer);
        (asked.expect("get_current_time"), request)
    }

    /// The tool asks SY.timer of the agent's own hive with TIMER_NOW_IN, through the SDK's
    /// TimerClient, and gives the time in UTC and in the zone, the zone and the local weekday;
    /// without a zone, UTC.
    #[tokio::test]
    async fn get_current_time_asks_sy_timer_of_its_hive() {
        let (asked, request) = time_asked(
            json!({"timezone": "America/Argentina/Buenos_Aires"}),
            fluxbee_sdk::MSG_TIMER_RESPONSE,
            json!({
                "ok": true,
                "verb": "TIMER_NOW_IN",
                "now_utc_ms": 1_793_113_445_123_i64,
                "tz": "America/Argentina/Buenos_Aires",
                "now_local_iso": "2026-10-27T12:04:05.123-03:00",
                "tz_offset_seconds": -10_800,
                "tz_abbrev": "-03"
            }),
        )
        .await;
        assert!(matches!(
            request.routing.dst,
            Destination::Unicast(ref dst) if dst == "SY.timer@worker1"
        ));
        assert_eq!(request.meta.msg_type, "system");
        assert_eq!(request.meta.msg.as_deref(), Some("TIMER_NOW_IN"));
        assert_eq!(
            request.payload["tz"],
            json!("America/Argentina/Buenos_Aires")
        );
        assert_eq!(
            asked,
            json!({
                "status": "ok",
                "utc": "2026-10-27T15:04:05.123Z",
                "local": "2026-10-27T12:04:05.123-03:00",
                "timezone": "America/Argentina/Buenos_Aires",
                "abbreviation": "-03",
                "weekday": "Tuesday"
            })
        );

        let (asked, request) = time_asked(
            json!({}),
            fluxbee_sdk::MSG_TIMER_RESPONSE,
            json!({
                "ok": true,
                "verb": "TIMER_NOW_IN",
                "now_utc_ms": 1_793_113_445_123_i64,
                "tz": "UTC",
                "now_local_iso": "2026-10-27T15:04:05.123Z",
                "tz_offset_seconds": 0,
                "tz_abbrev": "UTC"
            }),
        )
        .await;
        assert_eq!(request.payload["tz"], json!("UTC"));
        assert_eq!(asked["local"], json!("2026-10-27T15:04:05.123Z"));
        assert_eq!(asked["weekday"], json!("Tuesday"));
    }

    /// A zone SY.timer does not know is `invalid_timezone`; a SY.timer that does not answer, as on
    /// a hive without one, is `time_unavailable`, which may be retried.
    #[tokio::test]
    async fn get_current_time_reports_a_bad_zone_and_a_missing_sy_timer() {
        let (asked, _) = time_asked(
            json!({"timezone": "Mars/Olympus"}),
            fluxbee_sdk::MSG_TIMER_RESPONSE,
            json!({
                "ok": false,
                "verb": "TIMER_NOW_IN",
                "error": {"code": "TIMER_INVALID_TIMEZONE", "message": "unknown time zone Mars/Olympus"}
            }),
        )
        .await;
        assert_eq!(
            asked,
            json!({
                "status": "error",
                "error_code": "invalid_timezone",
                "message": "get_current_time: 'Mars/Olympus' is not an IANA time zone name",
                "retryable": false
            })
        );

        let (asked, _) = time_asked(
            json!({"timezone": "Europe/Madrid"}),
            fluxbee_sdk::protocol::MSG_UNREACHABLE,
            json!({"original_dst": "SY.timer@worker1", "reason": "NODE_NOT_FOUND"}),
        )
        .await;
        assert_eq!(asked["error_code"], json!("time_unavailable"));
        assert_eq!(asked["retryable"], json!(true));
    }

    // --- T9: "tools" is the AI node's only word (FINDINGS A-94) ---

    #[tokio::test]
    async fn config_set_refuses_the_old_multimodal_key() {
        let node = test_node();
        let response = node
            .handle_control_plane(config_set_request(json!({
                "behavior": {"kind": "ai_chat", "model": "gpt-5.5", "capabilities": {"multimodal": false}}
            })))
            .await
            .expect("control plane")
            .expect("CONFIG_RESPONSE");
        assert_eq!(response.payload["ok"], json!(false));
        assert_eq!(response.payload["error"]["code"], json!("invalid_config"));
        let message = response.payload["error"]["message"]
            .as_str()
            .expect("message");
        assert!(
            message.contains("behavior.capabilities is not accepted")
                && message.contains("behavior.multimodal"),
            "{message}"
        );
        assert!(node.behavior.read().await.is_none());

        // The new key, with its default.
        let doc = |behavior: Value| {
            materialize_effective_defaults(
                "AI.sales@motherbee",
                parse_effective_config_doc(&json!({ "behavior": behavior })).expect("parses"),
            )
        };
        let off = doc(json!({"kind": "ai_chat", "model": "gpt-5.5", "multimodal": false}));
        assert_eq!(off.behavior.multimodal, Some(false));
        let Ok(NodeBehavior::AiChat(ai)) = build_behavior_from_effective_config(&off) else {
            panic!("an ai_chat behavior");
        };
        assert!(!ai.multimodal);
        let default = doc(json!({"kind": "ai_chat", "model": "gpt-5.5"}));
        assert_eq!(
            serde_json::to_value(&default).expect("serialize")["behavior"]["multimodal"],
            json!(true)
        );
    }

    fn file_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    /// The names in `dir`, sorted.
    fn dir_names(dir: &Path) -> Vec<String> {
        let mut names = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    /// A config.json from before T9 is rewritten once at boot with the new key, and then the node
    /// boots on it. The file as it was stays beside it for a rollback to 0.1.85. Any other shape
    /// is left for the contract to refuse.
    #[test]
    fn an_old_config_json_is_converted_once_at_boot() {
        let root = std::env::temp_dir().join(format!("ai-generic-convert-{}", Uuid::new_v4()));
        let node = "AI.sales@motherbee";
        let path = managed_node_config_path_with_root(node, &root).expect("path");
        let dir = path.parent().expect("dir").to_path_buf();
        fs::create_dir_all(&dir).expect("node dir");
        let write = |config: Value| {
            fs::write(&path, serde_json::to_string_pretty(&config).expect("json")).expect("write")
        };
        let read = || -> Value {
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json")
        };
        let converted_capabilities = || {
            convert_capabilities_multimodal(&path)
                .expect("converted")
                .map(|conversion| conversion.capabilities)
        };
        let system = json!({"config_version": 7, "runtime_version": "0.1.85"});

        write(json!({
            "_system": system,
            "tenant_id": "tnt:33333333-3333-4333-8333-333333333333",
            "behavior": {"kind": "ai_chat", "model": "gpt-5.5", "capabilities": {"multimodal": false}}
        }));
        let original = fs::read_to_string(&path).expect("read");
        let conversion = convert_capabilities_multimodal(&path)
            .expect("converted")
            .expect("a conversion");
        assert_eq!(conversion.capabilities, json!({"multimodal": false}));
        // The file as it was, 0600, beside the converted one, which is 0600 too; no temp file.
        assert_eq!(conversion.original_copy, dir.join("config.json.pre-0.1.86"));
        assert_eq!(
            fs::read_to_string(&conversion.original_copy).expect("the copy"),
            original
        );
        assert_eq!(file_mode(&conversion.original_copy), 0o600);
        assert_eq!(file_mode(&path), 0o600);
        assert_eq!(dir_names(&dir), ["config.json", "config.json.pre-0.1.86"]);
        let converted = read();
        assert_eq!(
            converted["behavior"],
            json!({"kind": "ai_chat", "model": "gpt-5.5", "multimodal": false})
        );
        assert_eq!(converted["_system"], system);
        assert_eq!(
            converted["tenant_id"],
            json!("tnt:33333333-3333-4333-8333-333333333333")
        );
        assert_eq!(convert_capabilities_multimodal(&path), Ok(None), "once");
        let contract = AiGenericContract {
            node_name: node.to_string(),
        };
        let state = fluxbee_sdk::managed_control_plane::bootstrap_managed_control_plane_with_root(
            node, &contract, &root,
        )
        .expect("bootstrap");
        assert_eq!(state.current_state, NodeLifecycleState::Configured);
        assert_eq!(state.config_version, 7);
        assert_eq!(
            state.effective_config.expect("effective")["behavior"]["multimodal"],
            json!(false)
        );

        // The wrapped shape, and a capabilities that said nothing. A later conversion keeps the
        // file it converted, over the older copy.
        write(json!({
            "_system": system,
            "config": {"behavior": {"kind": "ai_chat", "model": "gpt-5.5", "capabilities": {}}}
        }));
        let original = fs::read_to_string(&path).expect("read");
        assert_eq!(converted_capabilities(), Some(json!({})));
        assert_eq!(
            read()["config"]["behavior"],
            json!({"kind": "ai_chat", "model": "gpt-5.5"})
        );
        assert_eq!(
            fs::read_to_string(dir.join("config.json.pre-0.1.86")).expect("the copy"),
            original
        );

        // What the old runner saved for an echo node, which it gave no default.
        write(json!({
            "_system": system,
            "behavior": {"kind": "echo", "model": null, "model_settings": null, "base_url": null, "capabilities": null}
        }));
        assert_eq!(converted_capabilities(), Some(Value::Null));
        assert!(read()["behavior"].get("capabilities").is_none());
        let state = fluxbee_sdk::managed_control_plane::bootstrap_managed_control_plane_with_root(
            node, &contract, &root,
        )
        .expect("bootstrap");
        assert_eq!(state.current_state, NodeLifecycleState::Configured);

        // Anything else stays, and the contract refuses it with the reason.
        for behavior in [
            json!({"kind": "ai_chat", "model": "gpt-5.5", "capabilities": {"multimodal": true, "vision": true}}),
            json!({"kind": "ai_chat", "model": "gpt-5.5", "multimodal": true, "capabilities": {"multimodal": false}}),
        ] {
            write(json!({"_system": system, "behavior": behavior}));
            assert_eq!(convert_capabilities_multimodal(&path), Ok(None));
            assert_eq!(read()["behavior"], behavior);
            let state =
                fluxbee_sdk::managed_control_plane::bootstrap_managed_control_plane_with_root(
                    node, &contract, &root,
                )
                .expect("bootstrap");
            assert_eq!(state.current_state, NodeLifecycleState::FailedConfig);
            assert!(state
                .last_error
                .expect("its reason")
                .message
                .contains("behavior.capabilities is not accepted"));
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Without its copy of the file as it was, the conversion converts nothing; and a write that
    /// fails leaves no temp file behind.
    #[test]
    fn a_config_json_whose_original_cannot_be_kept_is_not_converted() {
        let root = std::env::temp_dir().join(format!("ai-generic-convert-{}", Uuid::new_v4()));
        let path = managed_node_config_path_with_root("AI.sales@motherbee", &root).expect("path");
        let dir = path.parent().expect("dir").to_path_buf();
        // A directory where the copy goes: the rename onto it fails.
        fs::create_dir_all(dir.join("config.json.pre-0.1.86").join("in-the-way")).expect("dirs");
        let old = json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5", "capabilities": {"multimodal": true}}})
            .to_string();
        fs::write(&path, &old).expect("write");

        let err = convert_capabilities_multimodal(&path).expect_err("no copy, no conversion");
        assert!(err.contains("cannot keep it as it was"), "{err}");
        assert_eq!(fs::read_to_string(&path).expect("read"), old);
        assert_eq!(dir_names(&dir), ["config.json", "config.json.pre-0.1.86"]);
        let _ = fs::remove_dir_all(root);
    }

    // --- The tools' descriptions fit the model APIs ---

    /// OpenAI refuses a tool whose description passes 1024 characters. Every card's fits: the
    /// rules of a file's name are in the input schema, whose descriptions do not count.
    #[test]
    fn every_base_tool_description_fits_the_providers_limit() {
        for card in base_tool_cards() {
            let length = card.model_definition().description.chars().count();
            assert!(
                length <= fluxbee_ai_sdk::TOOL_DESCRIPTION_MAX_CHARS,
                "{}: {length} characters",
                card.name
            );
            assert_eq!(card.validate(), Ok(()), "{}", card.name);
        }
        for card in [GeneratePdfArtifactTool::card(), PublishHtmlTool::card()] {
            let filename = card.input["properties"]["filename"]["description"]
                .as_str()
                .expect("the filename's description");
            assert!(filename.contains("at most 128 bytes"), "{filename}");
            assert!(!card.model_definition().description.contains("128 bytes"));
        }
    }

    // --- Who runs the control plane ---

    async fn control_answer(node: &GenericAiNode, msg: Message) -> Message {
        node.handle_control_plane(msg)
            .await
            .expect("control plane")
            .expect("an answer")
    }

    /// The config from SY.admin@motherbee, the status and the help from the orchestrator of the
    /// node's hive: each verb runs from the node that sends it.
    #[tokio::test]
    async fn the_control_plane_runs_each_verb_from_the_node_that_sends_it() {
        let node = test_node();
        let config_get = control_answer(&node, admin_request(MSG_CONFIG_GET)).await;
        assert_eq!(config_get.meta.msg.as_deref(), Some(MSG_CONFIG_RESPONSE));
        assert_eq!(
            config_get.payload["contract"]["node_kind"],
            json!("ai.generic")
        );

        let status = control_answer(&node, sample_request()).await;
        assert_eq!(
            status.meta.msg.as_deref(),
            Some(MSG_NODE_STATUS_GET_RESPONSE)
        );
        assert_eq!(status.payload["status"], json!("ok"));

        let mut help = sample_request();
        help.meta.msg = Some(MSG_NODE_HELP.to_string());
        let help = control_answer(&node, help).await;
        assert_eq!(help.payload["status"], json!("ok"));

        // CONFIG_SET runs: it judges the config (applying a valid one would write the node's
        // config.json under /var/lib/fluxbee).
        let config = json!({"behavior": {"kind": "ai_chat", "model": "gpt-5.5", "params": {}}});
        let set = control_answer(&node, config_set_request(config)).await;
        assert_eq!(
            set.payload["error"]["code"],
            json!("invalid_config"),
            "{}",
            set.payload
        );
    }

    /// A node, an `admin` message, a verb in another case and the orchestrator of another hive are
    /// refused: a WARN, an `unauthorized` answer under the verb's own response, and nothing
    /// changes. A system message that is no control verb is answered as before.
    #[tokio::test]
    async fn the_control_plane_refuses_every_other_caller() {
        let node = test_node();
        let config = json!({"behavior": {"kind": "echo"}});
        let from_a_node = {
            let mut msg = config_set_request(config.clone());
            msg.routing.src_l2_name = Some("AI.other@motherbee".to_string());
            msg
        };
        let as_admin = {
            let mut msg = config_set_request(config.clone());
            msg.meta.msg_type = "admin".to_string();
            msg
        };
        let lowercase = {
            let mut msg = config_set_request(config.clone());
            msg.meta.msg = Some("config_set".to_string());
            msg
        };
        let (logs, _guard) = LogCapture::start();
        for msg in [from_a_node, as_admin, lowercase] {
            let answer = control_answer(&node, msg).await;
            assert_eq!(answer.meta.msg.as_deref(), Some(MSG_CONFIG_RESPONSE));
            assert_eq!(answer.payload["ok"], json!(false));
            assert_eq!(
                answer.payload["error"]["code"],
                json!("unauthorized"),
                "{}",
                answer.payload
            );
        }
        assert!(node.behavior.read().await.is_none(), "nothing applied");
        assert_eq!(
            node.control_plane.read().await.current_state,
            NodeLifecycleState::Unconfigured
        );

        // The orchestrator of another hive asks for a node of the motherbee.
        for (verb, response) in [
            (MSG_NODE_STATUS_GET, MSG_NODE_STATUS_GET_RESPONSE),
            (MSG_NODE_HELP, MSG_NODE_HELP_RESPONSE),
        ] {
            let mut msg = sample_request();
            msg.routing.src_l2_name = Some("SY.orchestrator@worker1".to_string());
            msg.meta.msg = Some(verb.to_string());
            let answer = control_answer(&node, msg).await;
            assert_eq!(answer.meta.msg.as_deref(), Some(response));
            assert_eq!(answer.payload["status"], json!("error"));
            assert_eq!(answer.payload["error_code"], json!("unauthorized"));
        }
        let text = logs.text();
        assert_eq!(
            text.matches("control request refused (unauthorized)")
                .count(),
            5,
            "{text}"
        );
        assert!(text.contains("WARN"), "{text}");

        // Not a control verb: answered as before, whoever sends it.
        let mut notice = sample_request();
        notice.routing.src_l2_name = None;
        notice.meta.msg = Some("VAULT_SECRET_CHANGED".to_string());
        let answer = control_answer(&node, notice).await;
        assert_eq!(answer.payload["error"]["code"], json!("unknown_system_msg"));
    }

    // --- A-95: a published page's link never reaches the journal ---

    /// Collects what the fmt layer writes on this thread while its guard lives.
    #[derive(Clone, Default)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);

    thread_local! {
        static THREAD_CAPTURE: std::cell::RefCell<Option<LogCapture>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The writer of the one global test subscriber: this thread's capture, if any.
    struct ThreadCaptureWriter;

    impl std::io::Write for ThreadCaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            THREAD_CAPTURE.with(|capture| {
                if let Some(capture) = capture.borrow().as_ref() {
                    capture
                        .0
                        .lock()
                        .expect("log capture")
                        .extend_from_slice(buf);
                }
            });
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Ends this thread's capture.
    struct LogCaptureGuard;

    impl Drop for LogCaptureGuard {
        fn drop(&mut self) {
            THREAD_CAPTURE.with(|capture| *capture.borrow_mut() = None);
        }
    }

    impl LogCapture {
        fn start() -> (Self, LogCaptureGuard) {
            static GLOBAL_SUBSCRIBER: OnceLock<()> = OnceLock::new();
            GLOBAL_SUBSCRIBER.get_or_init(|| {
                let subscriber = tracing_subscriber::fmt()
                    .with_writer(|| ThreadCaptureWriter)
                    .with_ansi(false)
                    .with_max_level(tracing::Level::TRACE)
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .expect("the test log subscriber is the only global one");
            });
            tracing::callsite::rebuild_interest_cache();
            let capture = Self::default();
            THREAD_CAPTURE.with(|slot| *slot.borrow_mut() = Some(capture.clone()));
            (capture, LogCaptureGuard)
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("log capture")).into_owned()
        }
    }

    /// The link `publish_html_page` returns is the key to the page. The model gets it, and the
    /// person gets it in the reply, which carries a file too, even when the model's text leaves it
    /// out; no line of the journal on the way carries it: the publish, the file, the reply.
    #[tokio::test]
    async fn a_published_link_reaches_the_reply_but_never_the_journal() {
        const KEY: &str = "k3y-of-the-published-page";
        let link = format!("https://fluxbee.example/public/{KEY}");
        let blob_root = std::env::temp_dir().join(format!("ai-generic-publish-{}", Uuid::new_v4()));
        let (dispatcher, mut harness) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "AI.sales@motherbee",
            build_ai_generic_rpc_profile().expect("profile"),
        );
        let files = Arc::new(ReplyFiles::default());
        let mut tools = FunctionToolRegistry::new();
        tools
            .register(Arc::new(PublishHtmlTool {
                vault: vault_client_for(
                    dispatcher,
                    "AI.sales@motherbee",
                    Some("ilk:11111111-1111-4111-8111-111111111111"),
                )
                .expect("vault client"),
                blob: fluxbee_sdk::blob::BlobConfig {
                    blob_root: blob_root.clone(),
                    ..fluxbee_sdk::blob::BlobConfig::default()
                },
            }))
            .expect("register");
        tools
            .register(Arc::new(GenerateTextArtifactTool {
                files: files.clone(),
            }))
            .expect("register");
        let model = ScriptedModel::new(
            vec![
                (
                    "publish_html_page",
                    json!({"filename": "page.html", "content": "<html>hola</html>"}),
                ),
                (
                    "generate_text_artifact",
                    json!({"filename": "notas.txt", "content": "notas"}),
                ),
            ],
            Some("Your page is ready."),
        );
        let (logs, _guard) = LogCapture::start();

        let admin = async {
            let request = harness.next_outgoing().await.expect("publish_artifact");
            assert_eq!(request.meta.action.as_deref(), Some("publish_artifact"));
            let answer = Message {
                routing: Routing {
                    src: "uuid-of-sy-admin".to_string(),
                    src_l2_name: Some("SY.admin@motherbee".to_string()),
                    dst: Destination::Unicast(harness.sender_uuid().to_string()),
                    ttl: 16,
                    trace_id: request.routing.trace_id.clone(),
                },
                meta: Meta {
                    msg_type: fluxbee_sdk::ADMIN_KIND.to_string(),
                    msg: Some(fluxbee_sdk::MSG_ADMIN_COMMAND_RESPONSE.to_string()),
                    ..Meta::default()
                },
                payload: json!({
                    "status": "ok",
                    "action": "publish_artifact",
                    "payload": {
                        "public_url": link,
                        "publication_id": "pub-7",
                        "expires_at": 1_790_000_000u64
                    }
                }),
            };
            harness.inject(answer).await.expect("SY.admin's answer");
        };
        let (output, ()) = tokio::join!(run_tool_loop_with(&model, &tools, &files), admin);
        let published = &model.results()[0];
        assert_eq!(published["status"], json!("ok"), "{published}");
        assert_eq!(published["url"], json!(link));

        let Some(AiBehaviorOutput::Final(reply)) = output else {
            panic!("a reply with its file: {output:?}");
        };
        let reply_text = format!("Your page is ready.\n{link}");
        assert_eq!(reply.text.as_deref(), Some(reply_text.as_str()));
        assert_eq!(reply.artifacts.len(), 1);

        let text = logs.text();
        assert!(text.contains("published HTML page"), "{text}");
        assert!(text.contains("pub-7"), "{text}");
        assert!(text.contains("notas.txt"), "{text}");
        assert!(
            text.contains(&format!("text_len={}", reply_text.len())),
            "{text}"
        );
        assert!(!text.contains(KEY), "the link reached the journal:\n{text}");
        let _ = fs::remove_dir_all(blob_root);
    }
}
