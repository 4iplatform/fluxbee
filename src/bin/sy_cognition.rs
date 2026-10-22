use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Mutex};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_ai_sdk::{AiAccount, AiProvider, HiveAiConfig, TenantSource, UsageRecorder};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, EpisodeSummary, MemoryContextSummary, MemoryPackage,
    MemoryPackageTruncated, MemoryReasonSummary, MemorySummary, Message, Meta, Routing,
    VaultSecretChangedPayload, VaultSecretInterest, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::rpc::SystemRpcRequest;
use fluxbee_sdk::stream::{
    is_stream_node_name, stream_hive, StreamBatch, MSG_STREAM_ACK, MSG_STREAM_BATCH,
    SUBJECT_COGNITION_TURNS, SUBJECT_STORAGE_COGNITION_BATCH,
};
use fluxbee_sdk::{
    build_node_config_response_message, managed_node_config_path, managed_node_instance_dir,
    managed_node_name, try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, VaultCallerOwned,
    VaultClient, NODE_CONFIG_APPLY_MODE_REPLACE,
};
use fluxbee_sdk::{
    CognitionCommunicationRef, CognitionContextData, CognitionCooccurrenceData,
    CognitionDurableEntity, CognitionDurableEnvelope, CognitionDurableOp, CognitionEnvelopeBatch,
    CognitionEpisodeData, CognitionIlkProfile, CognitionMemoryData, CognitionReasonData,
    CognitionScopeData, CognitionScopeInstanceData, CognitionStateGet, CognitionStateResponse,
    CognitionThreadData, MSG_COGNITION_STATE_GET, MSG_COGNITION_STATE_RESPONSE,
    SUBJECT_STORAGE_COGNITION_CONTEXTS, SUBJECT_STORAGE_COGNITION_COOCCURRENCES,
    SUBJECT_STORAGE_COGNITION_EPISODES, SUBJECT_STORAGE_COGNITION_MEMORIES,
    SUBJECT_STORAGE_COGNITION_REASONS, SUBJECT_STORAGE_COGNITION_SCOPES,
    SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES, SUBJECT_STORAGE_COGNITION_THREADS,
};
use json_router::shm::{
    memory_shm_name_for_hive, MemoryRegionWriter, MemoryShmSnapshot, MemoryShmThreadEntry,
    MEMORY_MAX_DATA_SIZE,
};

#[path = "sy_cognition/narrative_summarizer_ai.rs"]
mod narrative_summarizer_ai;
#[path = "sy_cognition/semantic_tagger_ai.rs"]
mod semantic_tagger_ai;

use narrative_summarizer_ai::{
    run_narrative_summarizer_ai, EpisodeNarrative, NarrativeSummaries, NarrativeSummarizerAiInput,
};
use semantic_tagger_ai::run_semantic_tagger_ai;

type CognitionError = Box<dyn std::error::Error + Send + Sync>;

const COGNITION_NODE_BASE_NAME: &str = "SY.cognition";
const COGNITION_NODE_VERSION: &str = "2.0";
const COGNITION_CONFIG_SCHEMA_VERSION: u32 = 1;
/// The most bytes of envelopes one `storage.cognition.batch` append carries: under the stream's
/// 96 KB record limit with room for the record's own fields (docs/router-stream-v1.md §6.2).
const COGNITION_APPEND_MAX_BYTES: usize = 64 * 1024;
/// How long one page of a communication's state may take to come back from SY.storage.
const COGNITION_STATE_PAGE_TIMEOUT: Duration = Duration::from_secs(5);
/// How many communications the startup reload asks SY.storage for at once.
const COGNITION_RELOAD_CONCURRENCY: usize = 4;
/// How long an agent node's ILK and tenant are trusted when the identity shared memory's sequence
/// cannot be read; while it can, they are trusted until it changes.
const COGNITION_AGENT_ILK_TTL: Duration = Duration::from_secs(60);
/// A communication held but idle this long is checked against SY.storage before its next turn:
/// its agent may have moved to another hive and back meanwhile (§6.2).
const COGNITION_HELD_STALE_AFTER: Duration = Duration::from_secs(300);
const COGNITION_DEFAULT_CONTEXT_CLOSE_THRESHOLD: f64 = 0.25;
const COGNITION_DEFAULT_REASON_CLOSE_THRESHOLD: f64 = 0.25;
const COGNITION_DEFAULT_SEMANTIC_TAGGER_TIMEOUT_MS: u64 = 8_000;
const COGNITION_CONTEXT_DECAY_FACTOR: f64 = 0.85;
const COGNITION_REASON_DECAY_FACTOR: f64 = 0.75;
const COGNITION_COOCCURRENCE_DECAY_FACTOR: f64 = 0.80;
const COGNITION_CONTEXT_EMA_ALPHA: f64 = 0.25;
const COGNITION_REASON_EMA_ALPHA: f64 = 0.30;
const COGNITION_COOCCURRENCE_EMA_ALPHA: f64 = 0.35;
const COGNITION_SCOPE_ENERGY_ALPHA: f64 = 0.25;
const COGNITION_SCOPE_UNBIND_THRESHOLD: f64 = 0.35;
const COGNITION_SCOPE_SUSTAIN_COUNT: u32 = 2;
const COGNITION_MEMORY_DECAY_FACTOR: f64 = 0.97;
const COGNITION_EPISODE_MIN_INTENSITY: f64 = 7.0;
const COGNITION_EPISODE_MIN_EVIDENCE_STRENGTH: f64 = 8.0;
const COGNITION_MAX_TAGS: usize = 12;
const COGNITION_MAX_REASON_SIGNALS: usize = 4;
const COGNITION_REASON_CANONICAL_SIGNALS: [&str; 8] = [
    "resolve",
    "inform",
    "protect",
    "connect",
    "challenge",
    "confirm",
    "request",
    "abandon",
];

#[derive(Debug, Clone, Default)]
struct SemanticTaggerOutput {
    tags: Vec<String>,
    reason_signals_canonical: Vec<String>,
    reason_signals_extra: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct NarrativeOutcome {
    called: bool,
    failed: bool,
    invalid_output: bool,
}

#[derive(Debug, Clone, Default)]
struct ThreadUpdateResult {
    envelopes: Vec<(&'static str, Vec<u8>)>,
    narrative: NarrativeOutcome,
}

/// The narrative step of a turn (memory and episode text). Production asks the AI; tests
/// answer from the input, so the whole turn path runs without a network.
trait NarrativeSummarizer {
    fn summarize<'a>(
        &'a self,
        input: NarrativeSummarizerAiInput<'a>,
    ) -> impl Future<Output = Result<NarrativeSummaries, fluxbee_ai_sdk::AiSdkError>> + Send + 'a;
}

struct AiNarrativeSummarizer;

impl NarrativeSummarizer for AiNarrativeSummarizer {
    fn summarize<'a>(
        &'a self,
        input: NarrativeSummarizerAiInput<'a>,
    ) -> impl Future<Output = Result<NarrativeSummaries, fluxbee_ai_sdk::AiSdkError>> + Send + 'a
    {
        run_narrative_summarizer_ai(input)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CognitionAiSecretSource {
    /// `openai_api_key_ref` persisted locally; resolution to plaintext via
    /// SY.vault happens lazily on each call site. The variant name keeps
    /// "LocalFile" for backward compatibility with already-persisted state
    /// files; the actual meaning is now "vault ref present locally".
    LocalFile,
    Missing,
}

impl CognitionAiSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "vault",
            Self::Missing => "missing",
        }
    }
}

/// `config.thresholds`. An open context or reason that a turn does not reinforce decays, and
/// closes once its weight is under its close threshold; a co-occurrence closes under the
/// average of the two. Nothing gates opening: every candidate the tagger names opens (score
/// 1.0). The keys were `context_open`/`reason_open` until 2026-10-02, and their value, halved,
/// was the close threshold: the 0.25 defaults keep what their 0.5 defaults did.
#[derive(Debug, Clone)]
struct CognitionThresholds {
    context_close: f64,
    reason_close: f64,
}

impl Default for CognitionThresholds {
    fn default() -> Self {
        Self {
            context_close: COGNITION_DEFAULT_CONTEXT_CLOSE_THRESHOLD,
            reason_close: COGNITION_DEFAULT_REASON_CLOSE_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CognitionSemanticTaggerConfig {
    #[serde(skip, default)]
    provider: AiProvider,
    #[serde(skip, default = "default_cognition_model")]
    model: String,
    timeout_ms: u64,
    max_tags: usize,
    max_reason_signals: usize,
}

fn default_cognition_model() -> String {
    fluxbee_ai_sdk::DEFAULT_HIVE_OPENAI_MODEL.to_string()
}

impl Default for CognitionSemanticTaggerConfig {
    fn default() -> Self {
        Self {
            provider: AiProvider::OpenAi,
            model: default_cognition_model(),
            timeout_ms: COGNITION_DEFAULT_SEMANTIC_TAGGER_TIMEOUT_MS,
            max_tags: COGNITION_MAX_TAGS,
            max_reason_signals: COGNITION_MAX_REASON_SIGNALS,
        }
    }
}

#[derive(Debug, Clone)]
struct CognitionControlState {
    schema_version: u32,
    config_version: u64,
    ai_secret_source: CognitionAiSecretSource,
    thresholds: CognitionThresholds,
    semantic_tagger: CognitionSemanticTaggerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CognitionConfigStateFile {
    schema_version: u32,
    config_version: u64,
    node_name: String,
    config: Value,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct RuntimePaths {
    state_dir: PathBuf,
    shm_dir: PathBuf,
    cache_dir: PathBuf,
    memory_lance_path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize)]
struct CognitionRuntimeState {
    processed_turns_total: u64,
    invalid_turns_total: u64,
    /// Turns dropped because the queue between their delivery and their processing was full.
    dropped_turns_queue_full_total: u64,
    published_entities_total: u64,
    publish_errors_total: u64,
    last_trace_id: Option<String>,
    last_thread_id: Option<String>,
    last_src_ilk: Option<String>,
    last_ich: Option<String>,
    last_tags: Vec<String>,
    last_reason_signals_canonical: Vec<String>,
    last_reason_signals_extra: Vec<String>,
    open_contexts_total: u64,
    open_reasons_total: u64,
    open_cooccurrences_total: u64,
    active_threads_total: u64,
    active_scopes_total: u64,
    active_memories_total: u64,
    active_episodes_total: u64,
    rebuild_attempts_total: u64,
    rebuild_successes_total: u64,
    rebuild_failures_total: u64,
    rebuilt_threads_total: u64,
    last_rebuild_status: Option<String>,
    last_rebuild_source: Option<String>,
    last_rebuild_at: Option<String>,
    /// Turns whose agent node has no ILK in the identity shared memory: no memory for them.
    no_agent_ilk_total: u64,
    /// Records of a stream batch at or below the last seq taken for that stream and epoch.
    stream_repeats_skipped_total: u64,
    /// `COGNITION_STATE_GET` exchanges with SY.storage (one per communication loaded), and the
    /// ones that failed; a turn whose communication could not load is skipped.
    state_fetches_total: u64,
    state_fetch_failures_total: u64,
    shm_hot_threads_total: u64,
    shm_pruned_threads_total: u64,
    shm_payload_bytes: u64,
    last_shm_sync_status: Option<String>,
    last_shm_sync_at: Option<String>,
    semantic_tagger_calls_total: u64,
    semantic_tagger_failures_total: u64,
    semantic_tagger_invalid_outputs_total: u64,
    last_semantic_model: Option<String>,
    last_semantic_impl: Option<String>,
    narrative_summarizer_calls_total: u64,
    narrative_summarizer_failures_total: u64,
    narrative_summarizer_invalid_outputs_total: u64,
    last_narrative_model: Option<String>,
}

struct CognitionAppState {
    hive_id: String,
    node_name: String,
    self_ilk_id: String,
    runtime_paths: RuntimePaths,
    control_state: Arc<Mutex<CognitionControlState>>,
    runtime_state: Arc<Mutex<CognitionRuntimeState>>,
    /// The communications this cognition holds, each owned by one of its hive's agents (§6.1).
    thread_states: Arc<Mutex<HashMap<CommunicationKey, ThreadCognitionState>>>,
    memory_region: Arc<Mutex<Option<MemoryRegionWriter>>>,
    vault: VaultClient,
    dispatcher: Arc<RouterDispatcher>,
    /// Agent node → its ILK and tenant, read from the identity shared memory.
    agent_ilks: Mutex<HashMap<String, AgentIdentity>>,
    /// Where each model call's usage record goes (docs/router-stream-v1.md §7.2).
    usage: UsageRecorder,
    /// The last record seq taken from each stream, by stream name and epoch.
    stream_seen: Mutex<HashMap<(String, u64), u64>>,
}

/// A communication (docs/router-stream-v1.md §6.2): the thread between its interlocutors, owned by
/// the agent that serves it. Memory and its references are per communication, never per hive or
/// node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CommunicationKey {
    thread_id: String,
    agent_ilk: String,
}

/// A copy of a turn its router made for this cognition: the message, and the local agent node that
/// owns its communication.
#[derive(Debug)]
struct CognitionTurn {
    message: Message,
    agent_node: String,
}

#[derive(Debug, Clone, Default)]
struct ThreadCognitionState {
    /// The agent node its router delivers to, where the memory package is read (mechanism only).
    agent_node: String,
    first_seen_at: Option<String>,
    last_seen_at: Option<String>,
    turn_count: u64,
    contexts: HashMap<String, ContextState>,
    reasons: HashMap<String, ReasonState>,
    cooccurrences: HashMap<String, CooccurrenceState>,
    active_scope: Option<ScopeBindingState>,
    memories: HashMap<String, MemoryState>,
    episodes: HashMap<String, EpisodeState>,
}

#[derive(Debug, Clone)]
struct ContextState {
    context_id: String,
    label: String,
    weight: f64,
    weight_avg_cumulative: f64,
    weight_avg_ema: f64,
    weight_samples: u64,
    tags: Vec<String>,
    ilk_weights: BTreeMap<String, f64>,
    ilk_profile: BTreeMap<String, CognitionIlkProfile>,
    opened_at: String,
    last_seen_at: String,
    closed_at: Option<String>,
    status: String,
}

#[derive(Debug, Clone)]
struct ReasonState {
    reason_id: String,
    label: String,
    weight: f64,
    weight_avg_cumulative: f64,
    weight_avg_ema: f64,
    weight_samples: u64,
    signals_canonical: Vec<String>,
    signals_extra: Vec<String>,
    ilk_weights: BTreeMap<String, f64>,
    ilk_profile: BTreeMap<String, CognitionIlkProfile>,
    opened_at: String,
    last_seen_at: String,
    closed_at: Option<String>,
    status: String,
}

#[derive(Debug, Clone)]
struct CooccurrenceState {
    cooccurrence_id: String,
    context_id: String,
    context_label: String,
    reason_id: String,
    reason_label: String,
    weight: f64,
    weight_avg_cumulative: f64,
    weight_avg_ema: f64,
    weight_samples: u64,
    occurrences: u64,
    opened_at: String,
    last_seen_at: String,
    closed_at: Option<String>,
    status: String,
}

#[derive(Debug, Clone)]
struct ScopeBindingState {
    scope_id: String,
    scope_instance_id: String,
    label: String,
    dominant_context_id: String,
    dominant_context_label: String,
    dominant_context_tags: Vec<String>,
    dominant_reason_id: String,
    dominant_reason_label: String,
    dominant_reason_signals: Vec<String>,
    ilk_weights: BTreeMap<String, f64>,
    binding_energy_ema: f64,
    opened_at: String,
    last_seen_at: String,
    start_turn: Option<u64>,
    unbind_streak: u32,
}

#[derive(Debug, Clone)]
struct MemoryState {
    memory_id: String,
    scope_id: String,
    summary: String,
    weight: f64,
    occurrences: u64,
    dominant_context_id: String,
    dominant_reason_id: String,
    ilk_weights: BTreeMap<String, f64>,
    created_at: String,
    last_seen_at: String,
}

#[derive(Debug, Clone)]
struct EpisodeState {
    episode_id: String,
    scope_id: String,
    scope_instance_id: String,
    affect_id: String,
    title: String,
    summary: String,
    base_intensity: f64,
    evidence_strength: f64,
    evidence_context_ids: Vec<String>,
    evidence_reason_ids: Vec<String>,
    evidence_signals: Vec<String>,
    intensity: f64,
    reason: String,
    created_at: String,
}

/// The deterministic half of an episode. Its summary and reason come from the narrative
/// summarizer, which must return both whenever a candidate exists.
#[derive(Debug, Clone)]
struct EpisodeCandidate {
    affect_id: String,
    title: String,
    base_intensity: f64,
    evidence_strength: f64,
    evidence_signals: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct MemoryHotSetStats {
    selected_threads_total: u64,
    pruned_threads_total: u64,
    payload_bytes: u64,
}

#[derive(Debug)]
struct MemoryHotSetBuild {
    snapshot: MemoryShmSnapshot,
    selected_thread_ids: HashSet<CommunicationKey>,
    stats: MemoryHotSetStats,
}

#[derive(Debug)]
struct MemoryHotThreadCandidate {
    key: CommunicationKey,
    entry: MemoryShmThreadEntry,
    serialized_bytes: usize,
    updated_by_this_turn: bool,
    has_active_scope: bool,
    live_entities_total: usize,
    last_seen_epoch_ms: i64,
    turn_count: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ScopeInstancePayload {
    scope_id: String,
    #[serde(default)]
    dominant_context_id: Option<String>,
    #[serde(default)]
    dominant_reason_id: Option<String>,
    #[serde(default)]
    start_turn: Option<u64>,
    #[serde(default)]
    opened_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
}

#[derive(Debug, Clone)]
struct ContextCandidate {
    label: String,
    tags: Vec<String>,
    score: f64,
}

#[derive(Debug, Clone)]
struct ReasonCandidate {
    label: String,
    score: f64,
    signals_canonical: Vec<String>,
    signals_extra: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    #[serde(default)]
    ai: Option<HiveAiConfig>,
}

#[tokio::main]
async fn main() -> Result<(), CognitionError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_cognition supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let hive = load_hive(&config_dir)?;
    let ai_engine = hive
        .ai
        .as_ref()
        .map(HiveAiConfig::effective)
        .transpose()
        .map_err(|err| -> CognitionError { err.into() })?
        .unwrap_or_else(HiveAiConfig::fallback);
    // Model D': self-ILK is deterministic from L2 name (no SHM wait).
    let self_ilk_id =
        fluxbee_sdk::deterministic_system_ilk_id(&format!("SY.cognition@{}", hive.hive_id));
    tracing::info!(self_ilk_id = %self_ilk_id, "self system ILK computed deterministically");
    let node_base_name = managed_node_name(COGNITION_NODE_BASE_NAME, &["SY_COGNITION_NODE_NAME"]);
    let node_name = ensure_l2_name(&node_base_name, &hive.hive_id);
    let runtime_paths = ensure_runtime_paths(&node_name)?;
    // Model D' — vault is the source of truth for the openai secret. We
    // can't know synchronously whether vault has it (probe needs an
    // ephemeral router connection). Boot starts in `Missing`; the state
    // flips to `LocalFile` after the first successful resolve in a
    // semantic call. Operator can probe with a CONFIG_GET after boot.
    let mut initial_control_state =
        bootstrap_cognition_control_state(&node_name, CognitionAiSecretSource::Missing)?;
    initial_control_state.semantic_tagger.provider = ai_engine.provider;
    initial_control_state.semantic_tagger.model = ai_engine.model;
    let control_state = Arc::new(Mutex::new(initial_control_state));
    let runtime_state = Arc::new(Mutex::new(CognitionRuntimeState::default()));

    let node_config = NodeConfig {
        name: node_base_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: COGNITION_NODE_VERSION.to_string(),
    };
    let profile = build_cognition_rpc_profile()
        .map_err(|err| format!("sy.cognition rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    tracing::info!(node_name = %sender.full_name(), "sy.cognition connected to router");

    // SY.cognition also runs on workers; the vault only on the motherbee.
    let vault_client = VaultClient::for_primary(
        dispatcher.clone(),
        VaultCallerOwned::new(self_ilk_id.clone(), node_name.clone()),
    );

    let app_state = Arc::new(CognitionAppState {
        hive_id: hive.hive_id.clone(),
        node_name: node_name.clone(),
        self_ilk_id: self_ilk_id.clone(),
        runtime_paths: runtime_paths.clone(),
        control_state: Arc::clone(&control_state),
        runtime_state: Arc::clone(&runtime_state),
        thread_states: Arc::new(Mutex::new(HashMap::new())),
        memory_region: Arc::new(Mutex::new(None)),
        vault: vault_client,
        dispatcher: Arc::clone(&dispatcher),
        agent_ilks: Mutex::new(HashMap::new()),
        usage: UsageRecorder::start(Arc::clone(&dispatcher)),
        stream_seen: Mutex::new(HashMap::new()),
    });

    if let Ok(owner_uuid) = Uuid::parse_str(sender.uuid()) {
        let shm_name = memory_shm_name_for_hive(&hive.hive_id)?;
        match MemoryRegionWriter::open_or_create(&shm_name, owner_uuid, &hive.hive_id) {
            Ok(writer) => {
                *app_state.memory_region.lock().await = Some(writer);
            }
            Err(err) => {
                tracing::warn!(shm = %shm_name, error = %err, "failed to initialize jsr-memory writer");
            }
        }
    }

    // The worker's task first resolves the AI key and brings back the communications the shared
    // memory holds, then takes turns. Meanwhile the main loop keeps acking its routers' batches and
    // the turns wait in the queue, inside the streams' window (§6.2).
    let (turn_tx, turn_rx) = mpsc::channel::<CognitionTurn>(COGNITION_TURN_QUEUE);
    let worker_state = Arc::clone(&app_state);
    std::mem::drop(tokio::spawn(async move {
        // Resolved once here, as SY.architect and SY.frontdesk.gov do at startup, so the state this
        // node reports is true from the start (A-69). Each turn still resolves it again.
        if resolve_cognition_ai_api_key(&worker_state, VaultWait::UntilReachable)
            .await
            .is_some()
        {
            worker_state.control_state.lock().await.ai_secret_source =
                CognitionAiSecretSource::LocalFile;
        }
        let ai_secret_source = worker_state.control_state.lock().await.ai_secret_source;
        tracing::info!(
            ai_secret_source = %ai_secret_source.as_str(),
            "sy.cognition looked up its AI key"
        );
        // Before the first turn, so a restart neither blanks the packages the agents read nor
        // starts them over.
        reload_communications_from_shared_memory(&worker_state).await;
        run_turn_worker(turn_rx, worker_state).await;
    }));

    // Vault state changes arrive event-driven via VAULT_SECRET_CHANGED
    // broadcasts from SY.vault — no polling loop needed. See
    // `handle_vault_secret_changed_cognition` in process_router_message.

    tracing::info!(
        hive = %hive.hive_id,
        node_name = %node_name,
        state_dir = %runtime_paths.state_dir.display(),
        "sy.cognition started"
    );

    let mut heartbeat = time::interval(Duration::from_secs(5));
    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| format!("sy.cognition system receiver: {err}"))?;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let snapshot = control_state.lock().await.clone();
                if let Some(memory_region) = app_state.memory_region.lock().await.as_mut() {
                    memory_region.update_heartbeat();
                }
                tracing::debug!(
                    node_name = %sender.full_name(),
                    config_version = snapshot.config_version,
                    ai_secret_source = %snapshot.ai_secret_source.as_str(),
                    "sy.cognition heartbeat"
                );
            }
            maybe_msg = system_rx.recv() => {
                let Some(msg) = maybe_msg else {
                    tracing::warn!("sy.cognition system channel closed; exiting main loop");
                    return Ok(());
                };
                if msg.meta.msg.as_deref() == Some(MSG_STREAM_BATCH) {
                    take_stream_batch(&app_state, &turn_tx, &msg).await;
                    continue;
                }
                let sender = dispatcher.sender_snapshot();
                if let Err(err) = process_router_message(
                    &sender,
                    &msg,
                    Arc::clone(&app_state),
                ).await {
                    tracing::warn!(error = %err, action = ?msg.meta.msg, "failed to process sy.cognition system message");
                }
            }
        }
    }
}

/// How many delivered turns may wait for their processing. Past it, a turn is dropped and counted.
const COGNITION_TURN_QUEUE: usize = 256;

/// Takes the turns its router copied for it (`cognition.turns`, §6.2), in order, one at a time.
async fn run_turn_worker(mut rx: mpsc::Receiver<CognitionTurn>, app_state: Arc<CognitionAppState>) {
    while let Some(turn) = rx.recv().await {
        handle_turn(turn, Arc::clone(&app_state)).await;
    }
}

/// A `STREAM_BATCH` from one of its hive's routers: each new record goes to the turn queue, and the
/// batch is acked at once (best effort, §2.7). A record at or below the last one taken from that
/// stream and epoch is a repeat; a full queue drops a turn and counts it.
async fn take_stream_batch(
    app_state: &CognitionAppState,
    turn_tx: &mpsc::Sender<CognitionTurn>,
    msg: &Message,
) {
    let from = msg.routing.src_l2_name.as_deref().unwrap_or("");
    if !is_stream_node_name(from) || stream_hive(from) != Some(app_state.hive_id.as_str()) {
        tracing::warn!(
            from,
            "sy.cognition dropped a STREAM_BATCH not from its hive's routers"
        );
        return;
    }
    let batch: StreamBatch = match serde_json::from_value(msg.payload.clone()) {
        Ok(batch) => batch,
        Err(err) => {
            tracing::warn!(from, error = %err, "sy.cognition dropped an unreadable STREAM_BATCH");
            return;
        }
    };
    let Some(last_seq) = batch.records.last().map(|record| record.seq) else {
        return;
    };
    let seen_key = (from.to_string(), batch.epoch);
    let mut seen = app_state
        .stream_seen
        .lock()
        .await
        .get(&seen_key)
        .copied()
        .unwrap_or(0);
    let (mut repeats, mut dropped) = (0u64, 0u64);
    for record in batch.records {
        if record.seq <= seen {
            repeats += 1;
            continue;
        }
        seen = record.seq;
        if record.subject != SUBJECT_COGNITION_TURNS {
            continue;
        }
        let message: Message = match serde_json::from_value(record.payload) {
            Ok(message) => message,
            Err(_) => {
                app_state.runtime_state.lock().await.invalid_turns_total += 1;
                continue;
            }
        };
        let turn = CognitionTurn {
            message,
            agent_node: record.producer,
        };
        match turn_tx.try_send(turn) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => dropped += 1,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!("sy.cognition turn worker is gone; exiting so systemd restarts it");
                std::process::exit(1);
            }
        }
    }
    app_state.stream_seen.lock().await.insert(seen_key, seen);
    if repeats > 0 || dropped > 0 {
        let mut state = app_state.runtime_state.lock().await;
        state.stream_repeats_skipped_total += repeats;
        state.dropped_turns_queue_full_total += dropped;
    }
    let sender = app_state.dispatcher.sender_snapshot();
    let ack = build_system_reply(
        &sender,
        msg,
        MSG_STREAM_ACK,
        json!({ "up_to_seq": last_seq }),
    );
    if let Err(err) = sender.send(ack).await {
        tracing::warn!(error = %err, "sy.cognition could not ack a stream batch");
    }
}

/// One turn of a communication (§6.2): the ILK of the agent that owns it, its state (loaded from
/// SY.storage the first time this cognition sees it), the tagger and the derivations, the envelopes
/// appended to its router's stream for SY.storage, and the agent's memory package.
async fn handle_turn(turn: CognitionTurn, app_state: Arc<CognitionAppState>) {
    let msg = turn.message;
    let Some(thread_id) = msg
        .meta
        .thread_id
        .clone()
        .filter(|thread_id| !thread_id.trim().is_empty())
    else {
        let mut state = app_state.runtime_state.lock().await;
        state.invalid_turns_total = state.invalid_turns_total.saturating_add(1);
        state.last_trace_id = Some(msg.routing.trace_id.clone());
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            "sy.cognition skipping turn without thread_id"
        );
        return;
    };
    let Some(agent) = agent_identity_of(&app_state, &turn.agent_node).await else {
        app_state.runtime_state.lock().await.no_agent_ilk_total += 1;
        tracing::debug!(
            agent_node = %turn.agent_node,
            thread_id = %thread_id,
            "sy.cognition skipping a turn of an agent without an ILK in the identity shared memory"
        );
        return;
    };
    let agent_ilk = agent.ilk.clone();
    let key = CommunicationKey {
        thread_id: thread_id.clone(),
        agent_ilk: agent_ilk.clone(),
    };
    let held_turn = app_state
        .thread_states
        .lock()
        .await
        .get(&key)
        .map(|state| (state.turn_count, thread_last_seen_epoch_ms(state)));
    match held_turn {
        None => {
            // Its state may be in SY.storage: after a restart, or when its agent moved here.
            match fetch_communication_state(&app_state, &key).await {
                Ok(state) => {
                    let mut state = state.unwrap_or_default();
                    state.agent_node = turn.agent_node.clone();
                    app_state
                        .thread_states
                        .lock()
                        .await
                        .insert(key.clone(), state);
                }
                Err(err) => {
                    tracing::warn!(
                        thread_id = %thread_id,
                        agent_ilk = %agent_ilk,
                        error = %err,
                        "sy.cognition could not load a communication from SY.storage; turn skipped"
                    );
                    return;
                }
            }
        }
        Some((held_turns, last_seen_ms))
            if chrono::Utc::now().timestamp_millis() - last_seen_ms
                > COGNITION_HELD_STALE_AFTER.as_millis() as i64 =>
        {
            // Held but idle: its agent may have moved away and back meanwhile, and another
            // hive's cognition carried it on. The stored state wins when it is further along.
            if let Ok(Some(mut stored)) = fetch_communication_state(&app_state, &key).await {
                if stored.turn_count > held_turns {
                    stored.agent_node = turn.agent_node.clone();
                    app_state
                        .thread_states
                        .lock()
                        .await
                        .insert(key.clone(), stored);
                }
            }
        }
        Some(_) => {}
    }

    let text = extract_turn_text(&msg.payload).unwrap_or_default();
    let (thresholds, semantic_tagger_config) = {
        let control_state = app_state.control_state.lock().await;
        (
            control_state.thresholds.clone(),
            control_state.semantic_tagger.clone(),
        )
    };
    let ts = chrono::Utc::now().to_rfc3339();
    let src_ilk = msg.meta.src_ilk.clone();
    let dst_ilk = msg.meta.dst_ilk.clone();
    let ich = msg.meta.ich.clone();
    {
        let mut state = app_state.runtime_state.lock().await;
        state.semantic_tagger_calls_total = state.semantic_tagger_calls_total.saturating_add(1);
        state.last_semantic_model = Some(semantic_tagger_config.model.clone());
        state.last_semantic_impl = Some(format!("{}_sdk", semantic_tagger_config.provider));
    }

    let Some(api_key) = resolve_cognition_ai_api_key(&app_state, VaultWait::Once).await else {
        {
            let mut control = app_state.control_state.lock().await;
            control.ai_secret_source = CognitionAiSecretSource::Missing;
        }
        let mut state = app_state.runtime_state.lock().await;
        state.semantic_tagger_failures_total =
            state.semantic_tagger_failures_total.saturating_add(1);
        state.last_trace_id = Some(msg.routing.trace_id.clone());
        state.last_thread_id = Some(thread_id.clone());
        state.last_src_ilk = src_ilk.clone();
        state.last_ich = ich.clone();
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            provider = %semantic_tagger_config.provider,
            "sy.cognition semantic tagger skipped turn because AI api key is not resolvable from vault"
        );
        return;
    };
    {
        let mut control = app_state.control_state.lock().await;
        control.ai_secret_source = CognitionAiSecretSource::LocalFile;
    }

    // The communication's agent's tenant pays for the turn's calls (§7.3).
    let account = AiAccount::charge(
        app_state.usage.clone(),
        &app_state.node_name,
        agent.tenant.clone(),
        TenantSource::Agent,
    )
    .with_node_ilk(Some(app_state.self_ilk_id.clone()))
    .with_counterpart(Some(agent_ilk.clone()))
    .with_trace(Some(msg.routing.trace_id.clone()))
    .with_thread(Some(thread_id.clone()));
    let tagger = match run_semantic_tagger_ai(semantic_tagger_ai::SemanticTaggerAiInput {
        api_key: &api_key,
        account: &account,
        text: &text,
        src_ilk: src_ilk.as_deref(),
        dst_ilk: dst_ilk.as_deref(),
        ich: ich.as_deref(),
        config: &semantic_tagger_config,
    })
    .await
    {
        Ok(output) => output,
        Err(err) => {
            let is_invalid_output = matches!(
                err,
                fluxbee_ai_sdk::AiSdkError::Json(_) | fluxbee_ai_sdk::AiSdkError::Protocol(_)
            );
            let mut state = app_state.runtime_state.lock().await;
            if is_invalid_output {
                state.semantic_tagger_invalid_outputs_total = state
                    .semantic_tagger_invalid_outputs_total
                    .saturating_add(1);
            } else {
                state.semantic_tagger_failures_total =
                    state.semantic_tagger_failures_total.saturating_add(1);
            }
            state.last_trace_id = Some(msg.routing.trace_id.clone());
            state.last_thread_id = Some(thread_id.clone());
            state.last_src_ilk = src_ilk.clone();
            state.last_ich = ich.clone();
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                error = %err,
                "sy.cognition semantic tagger failed; turn skipped"
            );
            return;
        }
    };

    let update_result = {
        let mut threads = app_state.thread_states.lock().await;
        let thread_state = threads.entry(key.clone()).or_default();
        thread_state.agent_node = turn.agent_node.clone();
        update_thread_state_and_build_envelopes(
            &app_state.hive_id,
            &app_state.node_name,
            &thread_id,
            &agent_ilk,
            src_ilk.as_deref(),
            dst_ilk.as_deref(),
            ich.as_deref(),
            &tagger,
            &thresholds,
            &api_key,
            &account,
            &semantic_tagger_config,
            &ts,
            thread_state,
            &AiNarrativeSummarizer,
        )
        .await
    };
    let (published, publish_errors) =
        append_envelopes(&app_state, update_result.envelopes, &msg.routing.trace_id).await;

    if let Err(err) = sync_memory_shm(&app_state, Some(&key)).await {
        tracing::warn!(
            error = %err,
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            "sy.cognition failed to sync jsr-memory"
        );
    }

    let (
        active_threads_total,
        open_contexts_total,
        open_reasons_total,
        open_cooccurrences_total,
        active_scopes_total,
        active_memories_total,
        active_episodes_total,
    ) = {
        let threads = app_state.thread_states.lock().await;
        let active_threads = threads.len() as u64;
        let open_contexts = threads
            .values()
            .map(|thread| {
                thread
                    .contexts
                    .values()
                    .filter(|context| context.status == "open")
                    .count() as u64
            })
            .sum();
        let open_reasons = threads
            .values()
            .map(|thread| {
                thread
                    .reasons
                    .values()
                    .filter(|reason| reason.status == "open")
                    .count() as u64
            })
            .sum();
        let open_cooccurrences = threads
            .values()
            .map(|thread| {
                thread
                    .cooccurrences
                    .values()
                    .filter(|cooccurrence| cooccurrence.status == "open")
                    .count() as u64
            })
            .sum();
        let active_scopes = threads
            .values()
            .filter(|thread| thread.active_scope.is_some())
            .count() as u64;
        let active_memories = threads
            .values()
            .map(|thread| thread.memories.len() as u64)
            .sum();
        let active_episodes = threads
            .values()
            .map(|thread| thread.episodes.len() as u64)
            .sum();
        (
            active_threads,
            open_contexts,
            open_reasons,
            open_cooccurrences,
            active_scopes,
            active_memories,
            active_episodes,
        )
    };

    let mut state = app_state.runtime_state.lock().await;
    state.processed_turns_total = state.processed_turns_total.saturating_add(1);
    state.published_entities_total = state.published_entities_total.saturating_add(published);
    state.publish_errors_total = state.publish_errors_total.saturating_add(publish_errors);
    state.last_trace_id = Some(msg.routing.trace_id.clone());
    state.last_thread_id = Some(thread_id);
    state.last_src_ilk = src_ilk;
    state.last_ich = ich;
    state.last_tags = tagger.tags;
    state.last_reason_signals_canonical = tagger.reason_signals_canonical;
    state.last_reason_signals_extra = tagger.reason_signals_extra;
    if update_result.narrative.called {
        state.narrative_summarizer_calls_total =
            state.narrative_summarizer_calls_total.saturating_add(1);
        state.last_narrative_model = Some(semantic_tagger_config.model.clone());
    }
    if update_result.narrative.failed {
        state.narrative_summarizer_failures_total =
            state.narrative_summarizer_failures_total.saturating_add(1);
    }
    if update_result.narrative.invalid_output {
        state.narrative_summarizer_invalid_outputs_total = state
            .narrative_summarizer_invalid_outputs_total
            .saturating_add(1);
    }
    state.active_threads_total = active_threads_total;
    state.open_contexts_total = open_contexts_total;
    state.open_reasons_total = open_reasons_total;
    state.open_cooccurrences_total = open_cooccurrences_total;
    state.active_scopes_total = active_scopes_total;
    state.active_memories_total = active_memories_total;
    state.active_episodes_total = active_episodes_total;
}

/// The envelopes of a turn, packed into `storage.cognition.batch` appends of at most
/// `COGNITION_APPEND_MAX_BYTES` each, on this node's router's stream for SY.storage (§6.2).
/// Returns how many envelopes went out and how many could not.
async fn append_envelopes(
    app_state: &CognitionAppState,
    envelopes: Vec<(&'static str, Vec<u8>)>,
    trace_id: &str,
) -> (u64, u64) {
    let values: Vec<Value> = envelopes
        .into_iter()
        .filter_map(|(_, body)| serde_json::from_slice(&body).ok())
        .collect();
    let sender = app_state.dispatcher.sender_snapshot();
    let (mut appended, mut failed) = (0u64, 0u64);
    for pack in pack_envelopes(values, COGNITION_APPEND_MAX_BYTES) {
        let count = pack.envelopes.len() as u64;
        let payload = serde_json::to_value(&pack).unwrap_or(Value::Null);
        match fluxbee_sdk::stream::append(&sender, SUBJECT_STORAGE_COGNITION_BATCH, payload).await {
            Ok(()) => appended += count,
            Err(err) => {
                failed += count;
                tracing::warn!(
                    error = %err,
                    trace_id,
                    "sy.cognition could not append its envelopes to the stream"
                );
            }
        }
    }
    (appended, failed)
}

/// Envelopes in packs of at most `max_bytes` serialized (one alone may pass it: the stream counts
/// it as oversize rather than this dropping it).
fn pack_envelopes(envelopes: Vec<Value>, max_bytes: usize) -> Vec<CognitionEnvelopeBatch> {
    let mut packs = Vec::new();
    let mut current = CognitionEnvelopeBatch::default();
    let mut bytes = 0usize;
    for envelope in envelopes {
        let size = serde_json::to_vec(&envelope).map(|v| v.len()).unwrap_or(0) + 1;
        if !current.envelopes.is_empty() && bytes + size > max_bytes {
            packs.push(std::mem::take(&mut current));
            bytes = 0;
        }
        bytes += size;
        current.envelopes.push(envelope);
    }
    if !current.envelopes.is_empty() {
        packs.push(current);
    }
    packs
}

/// The agent a turn's communication belongs to: its ILK and the tenant that pays for the turn's
/// calls (§7.3), as the identity shared memory of this hive holds them.
#[derive(Debug, Clone)]
struct AgentIdentity {
    ilk: String,
    tenant: Option<String>,
    /// The identity shared memory's sequence when it was read.
    seq: u64,
    read_at: std::time::Instant,
}

/// The agent of a turn's communication, from the identity shared memory (`handler_node`): the node
/// is mechanism, the ILK is the interlocutor (§6.1, D14). A cached answer holds while the region's
/// sequence is unchanged, so a node name reused by another agent is never answered from the cache;
/// when the sequence cannot be read, for `COGNITION_AGENT_ILK_TTL`.
async fn agent_identity_of(
    app_state: &CognitionAppState,
    agent_node: &str,
) -> Option<AgentIdentity> {
    let current_seq = fluxbee_sdk::identity::identity_shm_seq_from_hive_id(&app_state.hive_id).ok();
    if let Some(cached) = app_state.agent_ilks.lock().await.get(agent_node) {
        let fresh = match current_seq {
            Some(seq) => seq == cached.seq,
            None => cached.read_at.elapsed() < COGNITION_AGENT_ILK_TTL,
        };
        if fresh {
            return Some(cached.clone());
        }
    }
    let (seq, ilk) = fluxbee_sdk::identity::find_ilk_by_handler_node_from_hive_id(
        &app_state.hive_id,
        agent_node,
    )
    .ok()
    .flatten()?;
    if ilk.ilk_id.trim().is_empty() {
        return None;
    }
    let found = AgentIdentity {
        ilk: ilk.ilk_id,
        tenant: Some(ilk.tenant_id).filter(|tenant| !tenant.trim().is_empty()),
        seq,
        read_at: std::time::Instant::now(),
    };
    app_state
        .agent_ilks
        .lock()
        .await
        .insert(agent_node.to_string(), found.clone());
    Some(found)
}

/// Writes the memory region: the packages of the communications this cognition holds, ranked, and
/// after them the entries the region had for communications it has not loaded (a restart before
/// their reload, or a reload that failed), so a restart never blanks what the agents read (§6.2).
/// `turn`: the communication the current turn updated, which always stays.
async fn sync_memory_shm(
    app_state: &CognitionAppState,
    turn: Option<&CommunicationKey>,
) -> Result<(), json_router::shm::ShmError> {
    let mut memory_region = app_state.memory_region.lock().await;
    let Some(region) = memory_region.as_mut() else {
        let mut state = app_state.runtime_state.lock().await;
        state.last_shm_sync_status = Some("skipped_missing_region".to_string());
        state.last_shm_sync_at = Some(chrono::Utc::now().to_rfc3339());
        return Ok(());
    };
    let carried: Vec<MemoryShmThreadEntry> = region
        .read_snapshot()
        .map(|snapshot| snapshot.threads)
        .unwrap_or_default();
    let hot_set = {
        let mut threads = app_state.thread_states.lock().await;
        retain_memory_hot_set(&mut threads, turn, carried)?
    };
    region.write_snapshot(&hot_set.snapshot)?;
    drop(memory_region);

    let sync_status = if hot_set.stats.pruned_threads_total > 0 {
        "ok_pruned"
    } else {
        "ok"
    };
    let mut state = app_state.runtime_state.lock().await;
    state.shm_hot_threads_total = hot_set.stats.selected_threads_total;
    state.shm_pruned_threads_total = hot_set.stats.pruned_threads_total;
    state.shm_payload_bytes = hot_set.stats.payload_bytes;
    state.last_shm_sync_status = Some(sync_status.to_string());
    state.last_shm_sync_at = Some(chrono::Utc::now().to_rfc3339());
    Ok(())
}

/// Ranks the communications this cognition holds and keeps, in rank order, each one whose entry
/// still fits the region; then, in the region's previous order, the carried entries of the ones
/// it does not hold. The communication the current turn updated goes first, so it is never the
/// one dropped. Then recency (`last_seen_at`), an active scope, live entities, `turn_count` and
/// the key. Ranked by live entities ahead of recency, a new thread in a full region was dropped
/// right after its own turn and started over, with a new scope written to storage, on every turn.
fn build_memory_hot_set_snapshot(
    threads: &HashMap<CommunicationKey, ThreadCognitionState>,
    turn: Option<&CommunicationKey>,
    carried: Vec<MemoryShmThreadEntry>,
) -> Result<MemoryHotSetBuild, json_router::shm::ShmError> {
    let updated_at = json_router::shm::now_epoch_ms();
    let base_snapshot = MemoryShmSnapshot {
        schema_version: 2,
        updated_at,
        threads: Vec::new(),
    };
    let base_size = serde_json::to_vec(&base_snapshot)?.len();
    let mut candidates = Vec::with_capacity(threads.len());
    for (key, thread_state) in threads {
        let entry = MemoryShmThreadEntry {
            thread_id: key.thread_id.clone(),
            agent_ilk: key.agent_ilk.clone(),
            agent_node: thread_state.agent_node.clone(),
            package: build_memory_package_for_thread(&key.thread_id, thread_state),
        };
        let serialized_bytes = serde_json::to_vec(&entry)?.len();
        candidates.push(MemoryHotThreadCandidate {
            key: key.clone(),
            entry,
            serialized_bytes,
            updated_by_this_turn: turn == Some(key),
            has_active_scope: thread_state.active_scope.is_some(),
            live_entities_total: thread_live_entities_total(thread_state),
            last_seen_epoch_ms: thread_last_seen_epoch_ms(thread_state),
            turn_count: thread_state.turn_count,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .updated_by_this_turn
            .cmp(&left.updated_by_this_turn)
            .then_with(|| right.last_seen_epoch_ms.cmp(&left.last_seen_epoch_ms))
            .then_with(|| right.has_active_scope.cmp(&left.has_active_scope))
            .then_with(|| right.live_entities_total.cmp(&left.live_entities_total))
            .then_with(|| right.turn_count.cmp(&left.turn_count))
            .then_with(|| left.key.cmp(&right.key))
    });

    let mut selected_thread_ids = HashSet::with_capacity(candidates.len());
    let mut selected_entries = Vec::with_capacity(candidates.len());
    let mut payload_bytes = base_size;
    for candidate in candidates {
        let extra_bytes = candidate.serialized_bytes + usize::from(!selected_entries.is_empty());
        if payload_bytes.saturating_add(extra_bytes) > MEMORY_MAX_DATA_SIZE {
            continue;
        }
        payload_bytes = payload_bytes.saturating_add(extra_bytes);
        selected_thread_ids.insert(candidate.key);
        selected_entries.push(candidate.entry);
    }
    let selected_threads_total = selected_entries.len() as u64;
    let pruned_threads_total = threads.len().saturating_sub(selected_entries.len()) as u64;
    for entry in carried {
        let key = CommunicationKey {
            thread_id: entry.thread_id.clone(),
            agent_ilk: entry.agent_ilk.clone(),
        };
        if threads.contains_key(&key) {
            continue;
        }
        let extra_bytes =
            serde_json::to_vec(&entry)?.len() + usize::from(!selected_entries.is_empty());
        if payload_bytes.saturating_add(extra_bytes) > MEMORY_MAX_DATA_SIZE {
            continue;
        }
        payload_bytes = payload_bytes.saturating_add(extra_bytes);
        selected_entries.push(entry);
    }

    Ok(MemoryHotSetBuild {
        snapshot: MemoryShmSnapshot {
            schema_version: 2,
            updated_at,
            threads: selected_entries,
        },
        selected_thread_ids,
        stats: MemoryHotSetStats {
            selected_threads_total,
            pruned_threads_total,
            payload_bytes: payload_bytes as u64,
        },
    })
}

/// The one retention rule for local cognition state: build the jsr-memory hot set and drop every
/// communication that did not make it. Applied after every live turn (with the turn's
/// communication, which always stays) and after a reload, so memory stays bounded by the SHM
/// capacity. A dropped communication reloads from SY.storage if it comes back.
fn retain_memory_hot_set(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    turn: Option<&CommunicationKey>,
    carried: Vec<MemoryShmThreadEntry>,
) -> Result<MemoryHotSetBuild, json_router::shm::ShmError> {
    let hot_set = build_memory_hot_set_snapshot(threads, turn, carried)?;
    threads.retain(|key, _| hot_set.selected_thread_ids.contains(key));
    Ok(hot_set)
}

fn thread_live_entities_total(thread_state: &ThreadCognitionState) -> usize {
    let open_contexts = thread_state
        .contexts
        .values()
        .filter(|context| context.status == "open")
        .count();
    let open_reasons = thread_state
        .reasons
        .values()
        .filter(|reason| reason.status == "open")
        .count();
    let open_cooccurrences = thread_state
        .cooccurrences
        .values()
        .filter(|cooccurrence| cooccurrence.status == "open")
        .count();
    usize::from(thread_state.active_scope.is_some())
        + open_contexts
        + open_reasons
        + open_cooccurrences
        + thread_state.memories.len()
        + thread_state.episodes.len()
}

fn thread_last_seen_epoch_ms(thread_state: &ThreadCognitionState) -> i64 {
    thread_state
        .last_seen_at
        .as_deref()
        .and_then(parse_rfc3339_epoch_ms)
        .or_else(|| {
            thread_state
                .first_seen_at
                .as_deref()
                .and_then(parse_rfc3339_epoch_ms)
        })
        .unwrap_or_default()
}

fn parse_rfc3339_epoch_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// At start, before the first turn: the communications the memory region holds come back from
/// SY.storage over the mesh, at most `COGNITION_RELOAD_CONCURRENCY` at once (§6.2). One that could
/// not load, or that SY.storage has no state for yet (its last envelopes may still be on their way),
/// stays in the region as it was and loads on its next turn.
async fn reload_communications_from_shared_memory(app_state: &Arc<CognitionAppState>) {
    use futures::stream::{self, StreamExt};
    let entries: Vec<MemoryShmThreadEntry> = {
        let region = app_state.memory_region.lock().await;
        region
            .as_ref()
            .and_then(|region| region.read_snapshot().ok())
            .map(|snapshot| snapshot.threads)
            .unwrap_or_default()
    };
    let results: Vec<_> = stream::iter(entries)
        .map(|entry| async move {
            let key = CommunicationKey {
                thread_id: entry.thread_id,
                agent_ilk: entry.agent_ilk,
            };
            let state = fetch_communication_state(app_state, &key).await;
            (entry.agent_node, key, state)
        })
        .buffer_unordered(COGNITION_RELOAD_CONCURRENCY)
        .collect()
        .await;
    let (mut loaded, mut without_state, mut failed) = (0u64, 0u64, 0u64);
    {
        let mut threads = app_state.thread_states.lock().await;
        for (agent_node, key, state) in results {
            match state {
                Ok(Some(mut state)) => {
                    state.agent_node = agent_node;
                    threads.insert(key, state);
                    loaded += 1;
                }
                Ok(None) => without_state += 1,
                Err(_) => failed += 1,
            }
        }
    }
    if let Err(err) = sync_memory_shm(app_state, None).await {
        tracing::warn!(error = %err, "sy.cognition could not write jsr-memory after its reload");
    }
    let status = format!("ok loaded {loaded}, without state {without_state}, failed {failed}");
    {
        let mut state = app_state.runtime_state.lock().await;
        state.rebuild_attempts_total += 1;
        if failed == 0 {
            state.rebuild_successes_total += 1;
        } else {
            state.rebuild_failures_total += 1;
        }
        state.rebuilt_threads_total = loaded;
        state.last_rebuild_status = Some(status.clone());
        state.last_rebuild_source = Some("storage_mesh".to_string());
        state.last_rebuild_at = Some(chrono::Utc::now().to_rfc3339());
    }
    tracing::info!(node_name = %app_state.node_name, %status, "sy.cognition reloaded its communications");
}

/// One communication's state from SY.storage (`COGNITION_STATE_GET`, page by page): `None` when it
/// has none (a new communication, or one emptied), an error when SY.storage could not answer.
async fn fetch_communication_state(
    app_state: &CognitionAppState,
    key: &CommunicationKey,
) -> Result<Option<ThreadCognitionState>, CognitionError> {
    app_state.runtime_state.lock().await.state_fetches_total += 1;
    let envelopes = match fetch_communication_envelopes(app_state, key).await {
        Ok(envelopes) => envelopes,
        Err(err) => {
            app_state
                .runtime_state
                .lock()
                .await
                .state_fetch_failures_total += 1;
            return Err(err);
        }
    };
    if envelopes.is_empty() {
        return Ok(None);
    }
    Ok(Some(build_communication_state(
        key,
        rebuild_rows_from_envelopes(envelopes),
    )))
}

async fn fetch_communication_envelopes(
    app_state: &CognitionAppState,
    key: &CommunicationKey,
) -> Result<Vec<Value>, CognitionError> {
    let target = format!("SY.storage@{}", fluxbee_sdk::protocol::PRIMARY_HIVE_ID);
    let mut envelopes = Vec::new();
    let mut after: Option<(String, String)> = None;
    loop {
        let request = CognitionStateGet {
            thread_id: key.thread_id.clone(),
            agent_ilk: key.agent_ilk.clone(),
            after: after.clone(),
        };
        let reply = app_state
            .dispatcher
            .send_system_rpc(SystemRpcRequest {
                target: &target,
                request_msg: MSG_COGNITION_STATE_GET,
                response_msg: MSG_COGNITION_STATE_RESPONSE,
                payload: serde_json::to_value(&request)?,
                timeout: COGNITION_STATE_PAGE_TIMEOUT,
            })
            .await?;
        let page: CognitionStateResponse = serde_json::from_value(reply.payload)?;
        if !page.ok {
            return Err(page
                .error
                .unwrap_or_else(|| "SY.storage did not answer the state".to_string())
                .into());
        }
        envelopes.extend(page.envelopes);
        match page.next {
            Some(next) if Some(&next) != after.as_ref() => after = Some(next),
            Some(_) => return Err("SY.storage repeated a page of the state".into()),
            None => return Ok(envelopes),
        }
    }
}

/// A communication's durable rows, by entity, from the envelopes SY.storage returned.
#[derive(Debug, Default)]
struct RebuildRows {
    thread: Option<CognitionThreadData>,
    contexts: Vec<(String, CognitionContextData)>,
    reasons: Vec<(String, CognitionReasonData)>,
    cooccurrences: Vec<(String, CognitionCooccurrenceData)>,
    scopes: Vec<(String, CognitionScopeData)>,
    scope_instances: Vec<(String, ScopeInstancePayload)>,
    memories: Vec<(String, CognitionMemoryData)>,
    episodes: Vec<(String, CognitionEpisodeData)>,
}

fn rebuild_rows_from_envelopes(envelopes: Vec<Value>) -> RebuildRows {
    let mut rows = RebuildRows::default();
    for envelope in envelopes {
        let Ok(envelope) = serde_json::from_value::<CognitionDurableEnvelope<Value>>(envelope)
        else {
            continue;
        };
        let id = envelope.entity_id;
        let data = envelope.data;
        match envelope.entity {
            CognitionDurableEntity::Thread => rows.thread = serde_json::from_value(data).ok(),
            CognitionDurableEntity::Context => rows
                .contexts
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
            CognitionDurableEntity::Reason => rows
                .reasons
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
            CognitionDurableEntity::Cooccurrence => rows
                .cooccurrences
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
            CognitionDurableEntity::Scope => rows
                .scopes
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
            CognitionDurableEntity::ScopeInstance => rows
                .scope_instances
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
            CognitionDurableEntity::Memory => rows
                .memories
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
            CognitionDurableEntity::Episode => rows
                .episodes
                .extend(serde_json::from_value(data).ok().map(|d| (id, d))),
        }
    }
    rows
}

/// Assembles one communication's state from its durable rows, keyed exactly like the live path:
/// contexts and reasons by label, co-occurrences by `"<context label>|<reason label>"`, memories by
/// scope id, episodes by `"<scope instance id>|<affect id>"`. Keyed by entity id instead, every
/// lookup of the next live turn missed and it re-created each entity, under the same id, next to
/// the rebuilt one. The active scope is the open instance with the latest start turn.
fn build_communication_state(key: &CommunicationKey, rows: RebuildRows) -> ThreadCognitionState {
    let mut state = ThreadCognitionState::default();
    if let Some(thread) = rows.thread {
        state.first_seen_at = thread.first_seen_at;
        state.last_seen_at = thread.last_seen_at;
        state.turn_count = thread.turn_count.unwrap_or(0);
    }
    for (context_id, payload) in rows.contexts {
        let context = context_state_from_payload(context_id, payload);
        state.contexts.insert(context.label.clone(), context);
    }
    for (reason_id, payload) in rows.reasons {
        let reason = reason_state_from_payload(reason_id, payload);
        state.reasons.insert(reason.label.clone(), reason);
    }
    for (cooccurrence_id, payload) in rows.cooccurrences {
        let cooccurrence = cooccurrence_state_from_payload(cooccurrence_id, payload);
        let pair_key = format!(
            "{}|{}",
            cooccurrence.context_label, cooccurrence.reason_label
        );
        state.cooccurrences.insert(pair_key, cooccurrence);
    }
    let scope_payloads: HashMap<String, CognitionScopeData> = rows.scopes.into_iter().collect();
    let mut open_instance: Option<(String, ScopeInstancePayload)> = None;
    let mut instance_ids = Vec::new();
    for (scope_instance_id, payload) in rows.scope_instances {
        instance_ids.push(scope_instance_id.clone());
        if payload.closed_at.is_some() {
            continue;
        }
        let replace = match &open_instance {
            None => true,
            Some((_, current)) => payload.start_turn >= current.start_turn,
        };
        if replace {
            open_instance = Some((scope_instance_id, payload));
        }
    }
    for (memory_id, payload) in rows.memories {
        let memory = memory_state_from_payload(memory_id, payload);
        state.memories.insert(memory.scope_id.clone(), memory);
    }
    for (episode_id, payload) in rows.episodes {
        // The live path derives an episode id from (communication, scope instance, affect), so the
        // instance an episode belongs to is the one that reproduces its id.
        let scope_instance_id = instance_ids
            .iter()
            .find(|id| {
                stable_entity_id(
                    "episode",
                    &[
                        key.thread_id.as_str(),
                        key.agent_ilk.as_str(),
                        id.as_str(),
                        payload.affect_id.as_str(),
                    ],
                ) == episode_id
            })
            .cloned();
        // Without its instance row the episode keeps its own id as key, which no live turn
        // produces: it stays visible but is never mistaken for the open instance's episode.
        let episode_key = match &scope_instance_id {
            Some(instance_id) => format!("{}|{}", instance_id, payload.affect_id),
            None => episode_id.clone(),
        };
        let episode =
            episode_state_from_payload(episode_id, scope_instance_id.unwrap_or_default(), payload);
        state.episodes.insert(episode_key, episode);
    }
    if let Some((scope_instance_id, scope_payload)) = &open_instance {
        state.active_scope = scope_binding_state_from_payload(
            scope_instance_id,
            scope_payload,
            scope_payloads.get(&scope_payload.scope_id),
            &state.contexts,
            &state.reasons,
        );
    }
    state
}

fn context_state_from_payload(context_id: String, data: CognitionContextData) -> ContextState {
    ContextState {
        context_id,
        label: data.label,
        weight: data.weight.unwrap_or_default(),
        weight_avg_cumulative: data.weight_avg_cumulative.unwrap_or_default(),
        weight_avg_ema: data.weight_avg_ema.unwrap_or_default(),
        weight_samples: data.weight_samples.unwrap_or_default(),
        tags: data.tags,
        ilk_weights: data.ilk_weights,
        ilk_profile: data.ilk_profile,
        opened_at: data.opened_at.unwrap_or_default(),
        last_seen_at: data.last_seen_at.unwrap_or_default(),
        closed_at: data.closed_at,
        status: data.status,
    }
}

fn reason_state_from_payload(reason_id: String, data: CognitionReasonData) -> ReasonState {
    ReasonState {
        reason_id,
        label: data.label,
        weight: data.weight.unwrap_or_default(),
        weight_avg_cumulative: data.weight_avg_cumulative.unwrap_or_default(),
        weight_avg_ema: data.weight_avg_ema.unwrap_or_default(),
        weight_samples: data.weight_samples.unwrap_or_default(),
        signals_canonical: data.signals_canonical,
        signals_extra: data.signals_extra,
        ilk_weights: data.ilk_weights,
        ilk_profile: data.ilk_profile,
        opened_at: data.opened_at.unwrap_or_default(),
        last_seen_at: data.last_seen_at.unwrap_or_default(),
        closed_at: data.closed_at,
        status: data.status,
    }
}

fn cooccurrence_state_from_payload(
    cooccurrence_id: String,
    data: CognitionCooccurrenceData,
) -> CooccurrenceState {
    CooccurrenceState {
        cooccurrence_id,
        context_id: data.context_id,
        context_label: data.context_label,
        reason_id: data.reason_id,
        reason_label: data.reason_label,
        weight: data.weight.unwrap_or_default(),
        weight_avg_cumulative: data.weight_avg_cumulative.unwrap_or_default(),
        weight_avg_ema: data.weight_avg_ema.unwrap_or_default(),
        weight_samples: data.weight_samples.unwrap_or_default(),
        occurrences: data.occurrences.unwrap_or_default(),
        opened_at: data.opened_at.unwrap_or_default(),
        last_seen_at: data.last_seen_at.unwrap_or_default(),
        closed_at: data.closed_at,
        status: data.status,
    }
}

fn memory_state_from_payload(memory_id: String, data: CognitionMemoryData) -> MemoryState {
    MemoryState {
        memory_id,
        scope_id: data.scope_id.unwrap_or_default(),
        summary: data.summary,
        weight: data.weight.unwrap_or_default(),
        occurrences: data.occurrences.unwrap_or_default(),
        dominant_context_id: data.dominant_context_id.unwrap_or_default(),
        dominant_reason_id: data.dominant_reason_id.unwrap_or_default(),
        ilk_weights: data.ilk_weights,
        created_at: data.created_at.unwrap_or_default(),
        last_seen_at: data.last_seen_at.unwrap_or_default(),
    }
}

fn episode_state_from_payload(
    episode_id: String,
    scope_instance_id: String,
    data: CognitionEpisodeData,
) -> EpisodeState {
    EpisodeState {
        episode_id,
        scope_id: data.scope_id.unwrap_or_default(),
        scope_instance_id,
        affect_id: data.affect_id,
        title: data.title,
        summary: data.summary,
        base_intensity: data.base_intensity.unwrap_or_default(),
        evidence_strength: data.evidence_strength.unwrap_or_default(),
        evidence_context_ids: data.evidence_context_ids,
        evidence_reason_ids: data.evidence_reason_ids,
        evidence_signals: data.evidence_signals,
        intensity: data.intensity.unwrap_or_default(),
        reason: data.reason.unwrap_or_default(),
        created_at: data.created_at.unwrap_or_default(),
    }
}

fn scope_binding_state_from_payload(
    scope_instance_id: &str,
    instance: &ScopeInstancePayload,
    scope: Option<&CognitionScopeData>,
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
) -> Option<ScopeBindingState> {
    let dominant_context_id = instance
        .dominant_context_id
        .clone()
        .or_else(|| scope.and_then(|value| value.dominant_context_id.clone()))
        .unwrap_or_default();
    let dominant_reason_id = instance
        .dominant_reason_id
        .clone()
        .or_else(|| scope.and_then(|value| value.dominant_reason_id.clone()))
        .unwrap_or_default();
    // The maps are keyed by label (as live); the scope names its dominants by id.
    let dominant_context = contexts
        .values()
        .find(|context| context.context_id == dominant_context_id)?;
    let dominant_reason = reasons
        .values()
        .find(|reason| reason.reason_id == dominant_reason_id)?;
    Some(ScopeBindingState {
        scope_id: instance.scope_id.clone(),
        scope_instance_id: scope_instance_id.to_string(),
        label: scope
            .and_then(|value| value.label.clone())
            .unwrap_or_else(|| format!("scope:{}", instance.scope_id)),
        dominant_context_id: dominant_context.context_id.clone(),
        dominant_context_label: dominant_context.label.clone(),
        dominant_context_tags: dominant_context.tags.clone(),
        dominant_reason_id: dominant_reason.reason_id.clone(),
        dominant_reason_label: dominant_reason.label.clone(),
        dominant_reason_signals: dominant_reason.signals_canonical.clone(),
        ilk_weights: merged_scope_ilk_weights(dominant_context, dominant_reason),
        binding_energy_ema: scope
            .and_then(|value| value.binding_energy_ema)
            .unwrap_or_default(),
        opened_at: instance
            .opened_at
            .clone()
            .or_else(|| scope.and_then(|value| value.opened_at.clone()))
            .unwrap_or_default(),
        last_seen_at: scope
            .and_then(|value| value.last_seen_at.clone())
            .or_else(|| instance.opened_at.clone())
            .unwrap_or_default(),
        start_turn: instance.start_turn,
        unbind_streak: 0,
    })
}

fn merged_scope_ilk_weights(context: &ContextState, reason: &ReasonState) -> BTreeMap<String, f64> {
    let mut out = context.ilk_weights.clone();
    for (ilk, weight) in &reason.ilk_weights {
        *out.entry(ilk.clone()).or_insert(0.0) += *weight;
    }
    out
}

async fn resolve_cognition_ai_api_key(
    app_state: &CognitionAppState,
    wait: VaultWait,
) -> Option<String> {
    let provider = app_state
        .control_state
        .lock()
        .await
        .semantic_tagger
        .provider;
    resolve_cognition_resource(app_state, provider.resource_type(), "api_key", wait).await
}

/// Whether a vault lookup waits out a vault that is not reachable yet (as SY.architect,
/// SY.storage and SY.identity do; FINDINGS A-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VaultWait {
    /// At boot: retry transport failures for up to `VAULT_BOOT_WAIT`.
    UntilReachable,
    /// After boot (a turn, a VAULT_SECRET_CHANGED): the vault is up, ask once.
    Once,
}

/// Discover the named resource via the shared `VaultClient` (Model D' pool
/// match) and extract a plaintext string from the response (accepts a bare
/// string or `{"<field>": "..."}`).
async fn resolve_cognition_resource(
    app_state: &CognitionAppState,
    resource: fluxbee_sdk::ResourceType,
    nested_field: &str,
    wait: VaultWait,
) -> Option<String> {
    let resource_label = resource.as_str().to_string();
    let tenant = fluxbee_sdk::DEFAULT_ROOT_TENANT_ID;
    let timeout = Duration::from_secs(5);
    let result = match wait {
        VaultWait::UntilReachable => {
            app_state
                .vault
                .resolve_resource_awaiting_vault(
                    resource,
                    tenant,
                    timeout,
                    fluxbee_sdk::VAULT_BOOT_WAIT,
                    &app_state.node_name,
                )
                .await
        }
        VaultWait::Once => {
            app_state
                .vault
                .resolve_resource(resource, tenant, timeout)
                .await
        }
    };
    match result {
        Ok(Some(value)) => {
            if let Some(s) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                return Some(s.to_string());
            }
            value
                .get(nested_field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string)
        }
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                error = %err,
                resource = %resource_label,
                "sy.cognition vault resource lookup failed"
            );
            None
        }
    }
}

fn build_memory_package_for_thread(
    thread_id: &str,
    thread_state: &ThreadCognitionState,
) -> MemoryPackage {
    const MAX_CONTEXTS: usize = 6;
    const MAX_REASONS: usize = 6;
    const MAX_MEMORIES: usize = 6;
    const MAX_EPISODES: usize = 4;

    let mut contexts: Vec<&ContextState> = thread_state
        .contexts
        .values()
        .filter(|context| context.status == "open")
        .collect();
    contexts.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    let mut reasons: Vec<&ReasonState> = thread_state
        .reasons
        .values()
        .filter(|reason| reason.status == "open")
        .collect();
    reasons.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    let mut memories: Vec<&MemoryState> = thread_state.memories.values().collect();
    memories.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    let mut episodes: Vec<&EpisodeState> = thread_state.episodes.values().collect();
    episodes.sort_by(|left, right| right.intensity.total_cmp(&left.intensity));

    let dominant_context = contexts.first().map(|context| MemoryContextSummary {
        context_id: context.context_id.clone(),
        label: context.label.clone(),
        weight: context.weight,
    });
    let dominant_reason = reasons.first().map(|reason| MemoryReasonSummary {
        reason_id: reason.reason_id.clone(),
        label: reason.label.clone(),
        weight: reason.weight,
    });

    let dropped_contexts = contexts.len().saturating_sub(MAX_CONTEXTS) as u32;
    let dropped_reasons = reasons.len().saturating_sub(MAX_REASONS) as u32;
    let dropped_memories = memories.len().saturating_sub(MAX_MEMORIES) as u32;
    let dropped_episodes = episodes.len().saturating_sub(MAX_EPISODES) as u32;

    MemoryPackage {
        package_version: 2,
        thread_id: thread_id.to_string(),
        dominant_context,
        dominant_reason,
        contexts: contexts
            .into_iter()
            .take(MAX_CONTEXTS)
            .map(|context| MemoryContextSummary {
                context_id: context.context_id.clone(),
                label: context.label.clone(),
                weight: context.weight,
            })
            .collect(),
        reasons: reasons
            .into_iter()
            .take(MAX_REASONS)
            .map(|reason| MemoryReasonSummary {
                reason_id: reason.reason_id.clone(),
                label: reason.label.clone(),
                weight: reason.weight,
            })
            .collect(),
        memories: memories
            .into_iter()
            .take(MAX_MEMORIES)
            .map(|memory| MemorySummary {
                memory_id: memory.memory_id.clone(),
                summary: memory.summary.clone(),
                weight: memory.weight,
                dominant_context_id: Some(memory.dominant_context_id.clone()),
                dominant_reason_id: Some(memory.dominant_reason_id.clone()),
            })
            .collect(),
        episodes: episodes
            .into_iter()
            .take(MAX_EPISODES)
            .map(|episode| EpisodeSummary {
                episode_id: episode.episode_id.clone(),
                title: episode.title.clone(),
                intensity: episode.intensity.round().clamp(0.0, u8::MAX as f64) as u8,
            })
            .collect(),
        truncated: if dropped_contexts == 0
            && dropped_reasons == 0
            && dropped_memories == 0
            && dropped_episodes == 0
        {
            None
        } else {
            Some(MemoryPackageTruncated {
                applied: true,
                dropped_contexts,
                dropped_reasons,
                dropped_memories,
                dropped_episodes,
            })
        },
    }
}

/// Handle `VAULT_SECRET_CHANGED` for cognition. Its AI key is resolved lazily per call, so the
/// reaction is simply to probe the new value and update `ai_secret_source`.
async fn handle_vault_secret_changed_cognition(msg: &Message, app_state: &CognitionAppState) {
    tracing::info!(
        node_name = %app_state.node_name,
        trace_id = %msg.routing.trace_id,
        my_ilk = %app_state.self_ilk_id,
        "sy.cognition handle_vault_secret_changed_cognition: entered"
    );
    let payload: VaultSecretChangedPayload = match serde_json::from_value(msg.payload.clone()) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(error = %err, "ignoring malformed VAULT_SECRET_CHANGED payload");
            return;
        }
    };
    let ai_resource_type = app_state
        .control_state
        .lock()
        .await
        .semantic_tagger
        .provider
        .to_string();
    let interest_ai = VaultSecretInterest {
        resource_type: &ai_resource_type,
        my_tenant: fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
        my_ilk: Some(app_state.self_ilk_id.as_str()),
        system_caller: true,
    };
    let matched_ai = payload.matches_interest(&interest_ai);
    if !matched_ai {
        tracing::info!(
            node_name = %app_state.node_name,
            resource_type = %payload.resource_type,
            payload_tenant = %payload.tenant_id,
            payload_ilk = %payload.ilk.as_deref().unwrap_or(""),
            my_tenant = %fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
            my_ilk = %app_state.self_ilk_id,
            "sy.cognition VAULT_SECRET_CHANGED does not match its AI interest; ignoring"
        );
    }
    if matched_ai {
        tracing::info!(
            node_name = %app_state.node_name,
            resource_type = %ai_resource_type,
            op = %payload.op.as_str(),
            version = payload.version,
            "VAULT_SECRET_CHANGED for the AI provider matches; probing fresh value"
        );
        let resolved = resolve_cognition_ai_api_key(app_state, VaultWait::Once)
            .await
            .is_some();
        let next_source = if resolved {
            CognitionAiSecretSource::LocalFile
        } else {
            CognitionAiSecretSource::Missing
        };
        let mut control = app_state.control_state.lock().await;
        if control.ai_secret_source != next_source {
            tracing::info!(
                previous = %control.ai_secret_source.as_str(),
                current = %next_source.as_str(),
                "sy.cognition ai_secret_source flipped after VAULT_SECRET_CHANGED"
            );
            control.ai_secret_source = next_source;
        }
    }
}

async fn process_router_message(
    sender: &NodeSender,
    msg: &Message,
    app_state: Arc<CognitionAppState>,
) -> Result<(), CognitionError> {
    tracing::info!(
        node_name = %app_state.node_name,
        trace_id = %msg.routing.trace_id,
        src = %msg.routing.src,
        dst = ?msg.routing.dst,
        msg_type = %msg.meta.msg_type,
        msg = msg.meta.msg.as_deref().unwrap_or(""),
        action = msg.meta.action.as_deref().unwrap_or(""),
        target = msg.meta.target.as_deref().unwrap_or(""),
        "sy.cognition received router message"
    );
    if try_handle_default_node_status(sender, msg).await? {
        return Ok(());
    }
    if !is_system_kind(&msg.meta.msg_type) {
        return Ok(());
    }
    let Some(command) = msg.meta.msg.as_deref() else {
        return Ok(());
    };
    match command {
        MSG_VAULT_SECRET_CHANGED => {
            handle_vault_secret_changed_cognition(msg, &app_state).await;
        }
        "CONFIG_GET" => {
            let snapshot = app_state.runtime_state.lock().await.clone();
            let control_state = app_state.control_state.lock().await.clone();
            let payload = build_cognition_config_get_payload(
                &app_state.node_name,
                &control_state,
                &snapshot,
                &app_state.runtime_paths,
                None,
            );
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
        }
        "CONFIG_SET" => {
            let mut control_state = app_state.control_state.lock().await;
            let payload = apply_cognition_config_set(
                msg,
                &app_state.node_name,
                &mut control_state,
                &app_state.runtime_paths,
            )?;
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
        }
        "PING" => {
            let snapshot = app_state.runtime_state.lock().await.clone();
            let control_state = app_state.control_state.lock().await.clone();
            sender
                .send(build_system_reply(
                    sender,
                    msg,
                    "PONG",
                    build_status_payload(
                        &app_state.node_name,
                        &control_state,
                        &snapshot,
                        &app_state.runtime_paths,
                    ),
                ))
                .await?;
        }
        "STATUS" => {
            let snapshot = app_state.runtime_state.lock().await.clone();
            let control_state = app_state.control_state.lock().await.clone();
            sender
                .send(build_system_reply(
                    sender,
                    msg,
                    "STATUS_RESPONSE",
                    build_status_payload(
                        &app_state.node_name,
                        &control_state,
                        &snapshot,
                        &app_state.runtime_paths,
                    ),
                ))
                .await?;
        }
        _ => {}
    }
    Ok(())
}

fn build_system_reply(
    sender: &NodeSender,
    incoming: &Message,
    response_msg: &str,
    payload: Value,
) -> Message {
    Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(incoming.routing.src.clone()),
            ttl: incoming.routing.ttl.max(1),
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(response_msg.to_string()),
            src_ilk: incoming.meta.src_ilk.clone(),
            scope: incoming.meta.scope.clone(),
            target: incoming.meta.target.clone(),
            action: Some(response_msg.to_string()),
            priority: incoming.meta.priority.clone(),
            context: incoming.meta.context.clone(),
            ..Meta::default()
        },
        payload,
    }
}

fn build_status_payload(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_state: &CognitionRuntimeState,
    runtime_paths: &RuntimePaths,
) -> Value {
    let degraded_reasons = cognition_degraded_reasons(control_state);
    let ai_provider = json!({
        "provider": control_state.semantic_tagger.provider,
        "configured": control_state.ai_secret_source != CognitionAiSecretSource::Missing,
        "source": control_state.ai_secret_source.as_str()
    });
    let semantic_tagger = json!({
        "provider": control_state.semantic_tagger.provider,
        "model": control_state.semantic_tagger.model,
        "timeout_ms": control_state.semantic_tagger.timeout_ms,
        "max_tags": control_state.semantic_tagger.max_tags,
        "max_reason_signals": control_state.semantic_tagger.max_reason_signals,
        "ai_required": true,
        "implementation_status": "provider_neutral_sdk"
    });
    let degraded_semantics_policy = json!({
        "ai_required": true,
        "silent_fallback": false,
        "carrier_changes_when_degraded": false,
        "durable_schema_changes_when_degraded": false,
        "turn_behavior_without_ai": "skip_semantic_derivation_fail_open",
        "turn_behavior_on_semantic_tagger_failure": "skip_semantic_derivation_fail_open",
        "turn_behavior_on_narrative_summarizer_failure": "skip_narrative_update_fail_open",
        "notes": [
            "Without AI, SY.cognition stays live but does not produce new semantic derivations for the affected turn.",
            "The router carrier and durable entity schemas remain unchanged while semantic capability is degraded.",
            "There is no silent fallback path as product behavior."
        ]
    });
    let turns_consumer = json!({
        "subject": SUBJECT_COGNITION_TURNS,
        "source": "its hive's router streams (RT.*.stream@<hive>): both directions of its agents' communications",
        "processed_turns_total": runtime_state.processed_turns_total,
        "invalid_turns_total": runtime_state.invalid_turns_total,
        "dropped_turns_queue_full_total": runtime_state.dropped_turns_queue_full_total,
        "stream_repeats_skipped_total": runtime_state.stream_repeats_skipped_total,
        "no_agent_ilk_total": runtime_state.no_agent_ilk_total,
        "state_fetches_total": runtime_state.state_fetches_total,
        "state_fetch_failures_total": runtime_state.state_fetch_failures_total,
        "published_entities_total": runtime_state.published_entities_total,
        "publish_errors_total": runtime_state.publish_errors_total,
        "last_trace_id": runtime_state.last_trace_id,
        "last_thread_id": runtime_state.last_thread_id,
        "last_src_ilk": runtime_state.last_src_ilk,
        "last_ich": runtime_state.last_ich,
        "last_tags": runtime_state.last_tags,
        "last_reason_signals_canonical": runtime_state.last_reason_signals_canonical,
        "last_reason_signals_extra": runtime_state.last_reason_signals_extra,
        "active_threads_total": runtime_state.active_threads_total,
        "open_contexts_total": runtime_state.open_contexts_total,
        "open_reasons_total": runtime_state.open_reasons_total,
        "open_cooccurrences_total": runtime_state.open_cooccurrences_total,
        "active_scopes_total": runtime_state.active_scopes_total,
        "active_memories_total": runtime_state.active_memories_total,
        "active_episodes_total": runtime_state.active_episodes_total,
        "rebuild_attempts_total": runtime_state.rebuild_attempts_total,
        "rebuild_successes_total": runtime_state.rebuild_successes_total,
        "rebuild_failures_total": runtime_state.rebuild_failures_total,
        "rebuilt_threads_total": runtime_state.rebuilt_threads_total,
        "last_rebuild_status": runtime_state.last_rebuild_status,
        "last_rebuild_source": runtime_state.last_rebuild_source,
        "last_rebuild_at": runtime_state.last_rebuild_at,
        "semantic_tagger_calls_total": runtime_state.semantic_tagger_calls_total,
        "semantic_tagger_failures_total": runtime_state.semantic_tagger_failures_total,
        "semantic_tagger_invalid_outputs_total": runtime_state.semantic_tagger_invalid_outputs_total,
        "last_semantic_model": runtime_state.last_semantic_model,
        "last_semantic_impl": runtime_state.last_semantic_impl,
        "narrative_summarizer_calls_total": runtime_state.narrative_summarizer_calls_total,
        "narrative_summarizer_failures_total": runtime_state.narrative_summarizer_failures_total,
        "narrative_summarizer_invalid_outputs_total": runtime_state.narrative_summarizer_invalid_outputs_total,
        "last_narrative_model": runtime_state.last_narrative_model
    });
    let paths = json!({
        "state_dir": runtime_paths.state_dir,
        "cache_dir": runtime_paths.cache_dir,
        "shm_dir": runtime_paths.shm_dir,
        "memory_lance": runtime_paths.memory_lance_path
    });
    let shm = json!({
        "capacity_bytes": MEMORY_MAX_DATA_SIZE,
        "hot_threads_total": runtime_state.shm_hot_threads_total,
        "pruned_threads_total": runtime_state.shm_pruned_threads_total,
        "payload_bytes": runtime_state.shm_payload_bytes,
        "last_sync_status": runtime_state.last_shm_sync_status,
        "last_sync_at": runtime_state.last_shm_sync_at
    });
    json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "degraded": {
            "active": !degraded_reasons.is_empty(),
            "reasons": degraded_reasons
        },
        "ai_provider": ai_provider,
        "semantic_tagger": semantic_tagger,
        "degraded_semantics_policy": degraded_semantics_policy,
        "turns_consumer": turns_consumer,
        "paths": paths,
        "shm": shm
    })
}

fn build_cognition_config_get_payload(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_state: &CognitionRuntimeState,
    runtime_paths: &RuntimePaths,
    note: Option<&str>,
) -> Value {
    let configured = control_state.ai_secret_source != CognitionAiSecretSource::Missing;
    let resources = json!([
        {
            "resource_type": control_state.semantic_tagger.provider.to_string(),
            "required": true,
            "configured": configured,
            "scope": "pool (tenant or root)",
            "purpose": "semantic tagger + narrative summarizer"
        }
    ]);

    let mut notes = vec![
        Value::String(
            "SY.cognition keeps one memory per communication (the thread and the agent that serves it). Its router hands it both directions of its hive's agents' communications on the cognition.turns subject of its stream."
                .to_string(),
        ),
        Value::String(
            "AI credentials live in SY.vault. The hive-wide AI provider selects resource_type=openai|anthropic."
                .to_string(),
        ),
        Value::String(
            "The semantic tagger is an internal AI-backed component of SY.cognition using the configured provider/model."
                .to_string(),
        ),
        Value::String(
            "When AI is unavailable or a semantic call fails, carrier and durable schemas do not change; only new semantic enrichment is skipped for that turn/window."
                .to_string(),
        ),
        Value::String(
            "A communication's state comes back from SY.storage over the mesh (COGNITION_STATE_GET): at start for the communications in jsr-memory, and on the first turn of one it does not hold. A turn whose communication cannot load is skipped and counted."
                .to_string(),
        ),
        Value::String(
            "config.thresholds.context_close / reason_close (0 to 1, default 0.25) are the weights under which an open context / reason closes once turns stop reinforcing it; a co-occurrence closes under their average. Nothing gates opening: every tag the tagger returns opens a context, every canonical signal a reason. They replace context_open / reason_open, which only ever set this, at half their value."
                .to_string(),
        ),
    ];
    if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
        notes.push(Value::String(note.to_string()));
    }
    let config = json!({
        "turns": {
            "subject": SUBJECT_COGNITION_TURNS,
            "source": "router stream"
        },
        "storage": {
            "write_subject": SUBJECT_STORAGE_COGNITION_BATCH,
            "state_from": format!("{MSG_COGNITION_STATE_GET} to SY.storage@{}", fluxbee_sdk::protocol::PRIMARY_HIVE_ID)
        },
        "ai": {
            "default_provider": control_state.semantic_tagger.provider,
            "model": control_state.semantic_tagger.model,
            "resolved_from": format!("vault://resource_type={}", control_state.semantic_tagger.provider)
        },
        "semantic_tagger": {
            "provider": control_state.semantic_tagger.provider,
            "model": control_state.semantic_tagger.model,
            "timeout_ms": control_state.semantic_tagger.timeout_ms,
            "max_tags": control_state.semantic_tagger.max_tags,
            "max_reason_signals": control_state.semantic_tagger.max_reason_signals,
            "ai_required": true,
            "implementation_status": "provider_neutral_sdk"
        },
        "degraded_semantics_policy": {
            "ai_required": true,
            "silent_fallback": false,
            "carrier_changes_when_degraded": false,
            "durable_schema_changes_when_degraded": false,
            "turn_behavior_without_ai": "skip_semantic_derivation_fail_open",
            "turn_behavior_on_semantic_tagger_failure": "skip_semantic_derivation_fail_open",
            "turn_behavior_on_narrative_summarizer_failure": "skip_narrative_update_fail_open"
        },
        "thresholds": {
            "context_close": control_state.thresholds.context_close,
            "reason_close": control_state.thresholds.reason_close
        },
        "paths": {
            "state_dir": runtime_paths.state_dir,
            "cache_dir": runtime_paths.cache_dir,
            "shm_dir": runtime_paths.shm_dir,
            "memory_lance": runtime_paths.memory_lance_path
        }
    });
    let runtime = json!({
        "processed_turns_total": runtime_state.processed_turns_total,
        "invalid_turns_total": runtime_state.invalid_turns_total,
        "dropped_turns_queue_full_total": runtime_state.dropped_turns_queue_full_total,
        "published_entities_total": runtime_state.published_entities_total,
        "publish_errors_total": runtime_state.publish_errors_total,
        "no_agent_ilk_total": runtime_state.no_agent_ilk_total,
        "state_fetch_failures_total": runtime_state.state_fetch_failures_total,
        "last_thread_id": runtime_state.last_thread_id,
        "last_tags": runtime_state.last_tags,
        "last_reason_signals_canonical": runtime_state.last_reason_signals_canonical,
        "last_reason_signals_extra": runtime_state.last_reason_signals_extra,
        "active_threads_total": runtime_state.active_threads_total,
        "open_contexts_total": runtime_state.open_contexts_total,
        "open_reasons_total": runtime_state.open_reasons_total,
        "open_cooccurrences_total": runtime_state.open_cooccurrences_total,
        "active_scopes_total": runtime_state.active_scopes_total,
        "active_memories_total": runtime_state.active_memories_total,
        "active_episodes_total": runtime_state.active_episodes_total,
        "rebuild_attempts_total": runtime_state.rebuild_attempts_total,
        "rebuild_successes_total": runtime_state.rebuild_successes_total,
        "rebuild_failures_total": runtime_state.rebuild_failures_total,
        "rebuilt_threads_total": runtime_state.rebuilt_threads_total,
        "last_rebuild_status": runtime_state.last_rebuild_status,
        "last_rebuild_source": runtime_state.last_rebuild_source,
        "last_rebuild_at": runtime_state.last_rebuild_at,
        "shm_hot_threads_total": runtime_state.shm_hot_threads_total,
        "shm_pruned_threads_total": runtime_state.shm_pruned_threads_total,
        "shm_payload_bytes": runtime_state.shm_payload_bytes,
        "last_shm_sync_status": runtime_state.last_shm_sync_status,
        "last_shm_sync_at": runtime_state.last_shm_sync_at,
        "semantic_tagger_calls_total": runtime_state.semantic_tagger_calls_total,
        "semantic_tagger_failures_total": runtime_state.semantic_tagger_failures_total,
        "semantic_tagger_invalid_outputs_total": runtime_state.semantic_tagger_invalid_outputs_total,
        "last_semantic_model": runtime_state.last_semantic_model,
        "last_semantic_impl": runtime_state.last_semantic_impl,
        "narrative_summarizer_calls_total": runtime_state.narrative_summarizer_calls_total,
        "narrative_summarizer_failures_total": runtime_state.narrative_summarizer_failures_total,
        "narrative_summarizer_invalid_outputs_total": runtime_state.narrative_summarizer_invalid_outputs_total,
        "last_narrative_model": runtime_state.last_narrative_model
    });
    let contract = json!({
        "node_family": "SY",
        "node_kind": "SY.cognition",
        "supports": ["CONFIG_GET", "CONFIG_SET"],
        "required_fields": [],
        "optional_fields": [
            "config.semantic_tagger.timeout_ms",
            "config.semantic_tagger.max_tags",
            "config.semantic_tagger.max_reason_signals",
            "config.thresholds.context_close",
            "config.thresholds.reason_close"
        ],
        "resources": resources,
        "notes": notes
    });

    json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": config,
        "runtime": runtime,
        "contract": contract
    })
}

fn apply_cognition_config_set(
    msg: &Message,
    node_name: &str,
    control_state: &mut CognitionControlState,
    runtime_paths: &RuntimePaths,
) -> Result<Value, CognitionError> {
    let Some(requested_node_name) = msg.payload.get("node_name").and_then(Value::as_str) else {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            "invalid_config",
            "config-set requires node_name",
        ));
    };
    if requested_node_name != node_name {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            "invalid_config",
            "config-set node_name does not match this node",
        ));
    }
    if let Some(apply_mode) = msg.payload.get("apply_mode").and_then(Value::as_str) {
        if !apply_mode
            .trim()
            .eq_ignore_ascii_case(NODE_CONFIG_APPLY_MODE_REPLACE)
        {
            return Ok(config_error_response(
                node_name,
                control_state,
                runtime_paths,
                "unsupported_apply_mode",
                "SY.cognition currently supports only apply_mode=replace",
            ));
        }
    }

    // Model D' — cognition has no secret-bearing CONFIG_SET fields. Reject
    // any attempt to pass openai or storage credentials (plaintext or ref).
    if let Err(err) = reject_cognition_secret_fields(&msg.payload) {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            "invalid_config",
            &err.to_string(),
        ));
    }
    let thresholds = match extract_cognition_thresholds(&msg.payload) {
        Ok(value) => value,
        Err(err) => {
            return Ok(config_error_response(
                node_name,
                control_state,
                runtime_paths,
                "invalid_config",
                &err.to_string(),
            ));
        }
    };
    let semantic_tagger = match extract_cognition_semantic_tagger_config(
        &msg.payload,
        &control_state.semantic_tagger,
    ) {
        Ok(value) => value,
        Err(err) => {
            return Ok(config_error_response(
                node_name,
                control_state,
                runtime_paths,
                "invalid_config",
                &err.to_string(),
            ));
        }
    };

    if let Some(thresholds) = thresholds {
        control_state.thresholds = thresholds;
    }
    if let Some(semantic_tagger) = semantic_tagger {
        control_state.semantic_tagger = semantic_tagger;
    }
    control_state.config_version = control_state.config_version.saturating_add(1);
    persist_cognition_config_state(node_name, control_state)?;

    Ok(json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "apply_mode": NODE_CONFIG_APPLY_MODE_REPLACE,
        "config": {
            "semantic_tagger": {
                "provider": control_state.semantic_tagger.provider,
                "model": control_state.semantic_tagger.model,
                "timeout_ms": control_state.semantic_tagger.timeout_ms,
                "max_tags": control_state.semantic_tagger.max_tags,
                "max_reason_signals": control_state.semantic_tagger.max_reason_signals,
                "ai_required": true,
                "implementation_status": "provider_neutral_sdk"
            },
            "thresholds": {
                "context_close": control_state.thresholds.context_close,
                "reason_close": control_state.thresholds.reason_close
            }
        },
        "message": "SY.cognition non-secret config persisted (Model D': AI credentials live in vault under resource_type=<openai|anthropic>)."
    }))
}

fn config_error_response(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_paths: &RuntimePaths,
    code: &str,
    message: &str,
) -> Value {
    build_cognition_config_get_payload(
        node_name,
        control_state,
        &CognitionRuntimeState::default(),
        runtime_paths,
        Some(message),
    )
    .as_object()
    .cloned()
    .map(|mut value| {
        value.insert("ok".to_string(), Value::Bool(false));
        value.insert(
            "error".to_string(),
            json!({
                "code": code,
                "message": message
            }),
        );
        Value::Object(value)
    })
    .unwrap_or_else(|| {
        json!({
            "ok": false,
            "node_name": node_name,
            "state": cognition_state_label(control_state),
            "error": {
                "code": code,
                "message": message
            }
        })
    })
}

fn cognition_state_label(control_state: &CognitionControlState) -> &'static str {
    match control_state.ai_secret_source {
        CognitionAiSecretSource::Missing => "degraded_no_ai_provider",
        CognitionAiSecretSource::LocalFile => "ready_semantic_ai",
    }
}

fn cognition_degraded_reasons(control_state: &CognitionControlState) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if control_state.ai_secret_source == CognitionAiSecretSource::Missing {
        reasons.push("ai_provider_missing");
    }
    reasons
}

fn bootstrap_cognition_control_state(
    node_name: &str,
    ai_secret_source: CognitionAiSecretSource,
) -> Result<CognitionControlState, CognitionError> {
    let persisted = load_cognition_config_state(node_name);
    let schema_version = persisted
        .as_ref()
        .map(|value| value.schema_version)
        .unwrap_or(COGNITION_CONFIG_SCHEMA_VERSION);
    let config_version = persisted
        .as_ref()
        .map(|value| value.config_version)
        .unwrap_or(0);
    // Read like a CONFIG_SET, so persisted thresholds the parser refuses (keys from before
    // a rename) are reported instead of silently replaced by the defaults.
    let thresholds = match persisted
        .as_ref()
        .and_then(|value| value.config.get("thresholds"))
    {
        Some(value) => parse_cognition_thresholds(value).unwrap_or_else(|err| {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "sy.cognition persisted thresholds refused; the defaults apply"
            );
            CognitionThresholds::default()
        }),
        None => CognitionThresholds::default(),
    };
    let semantic_tagger = persisted
        .as_ref()
        .and_then(|value| {
            value
                .config
                .get("semantic_tagger")
                .cloned()
                .and_then(|value| {
                    serde_json::from_value::<CognitionSemanticTaggerConfig>(value).ok()
                })
        })
        .unwrap_or_default();
    let state = CognitionControlState {
        schema_version,
        config_version,
        ai_secret_source,
        thresholds,
        semantic_tagger,
    };
    persist_cognition_config_state(node_name, &state)?;
    Ok(state)
}

fn load_cognition_config_state(node_name: &str) -> Option<CognitionConfigStateFile> {
    let path = managed_node_config_path(node_name).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<CognitionConfigStateFile>(&raw).ok()
}

fn persist_cognition_config_state(
    node_name: &str,
    state: &CognitionControlState,
) -> Result<(), CognitionError> {
    let path = managed_node_config_path(node_name)?;
    let payload = CognitionConfigStateFile {
        schema_version: state.schema_version,
        config_version: state.config_version,
        node_name: node_name.to_string(),
        config: json!({
            "semantic_tagger": {
                "provider": state.semantic_tagger.provider,
                "model": state.semantic_tagger.model,
                "timeout_ms": state.semantic_tagger.timeout_ms,
                "max_tags": state.semantic_tagger.max_tags,
                "max_reason_signals": state.semantic_tagger.max_reason_signals
            },
            "thresholds": {
                "context_close": state.thresholds.context_close,
                "reason_close": state.thresholds.reason_close
            }
        }),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&path, &serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn ensure_runtime_paths(node_name: &str) -> Result<RuntimePaths, CognitionError> {
    let state_dir = managed_node_instance_dir(node_name)?;
    let cache_dir = state_dir.join("cache");
    let shm_dir = state_dir.join("shm");
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(&shm_dir)?;
    Ok(RuntimePaths {
        memory_lance_path: state_dir.join("memory.lance"),
        state_dir,
        shm_dir,
        cache_dir,
    })
}

/// Model D' — reject any secret-bearing field on the cognition CONFIG_SET
/// surface. OpenAI + storage postgres credentials live entirely in
/// SY.vault under resource_type=openai/postgres.
fn reject_cognition_secret_fields(body: &Value) -> Result<(), CognitionError> {
    let config_root = body.get("config").unwrap_or(body);
    if let Some(openai) = config_root
        .get("secrets")
        .and_then(|value| value.get("openai"))
    {
        for forbidden in ["api_key", "api_key_ref"] {
            if openai.get(forbidden).is_some() {
                return Err(format!(
                    "config.secrets.openai.{forbidden} is no longer accepted; load the openai secret via vault_put (resource_type=openai) and cognition will discover it from the pool"
                ).into());
            }
        }
    }
    if config_root.get("ai_providers").is_some() || config_root.get("ai").is_some() {
        return Err(
            "AI provider/model are hive-wide; configure the ai section in hive.yaml".into(),
        );
    }
    if let Some(storage) = config_root.get("storage") {
        for forbidden in ["postgres_url", "postgres_url_ref"] {
            if storage.get(forbidden).is_some() {
                return Err(format!(
                    "config.storage.{forbidden} is no longer accepted; load the postgres secret via vault_put (resource_type=postgres) and cognition will discover it from the pool"
                ).into());
            }
        }
    }
    Ok(())
}

fn extract_cognition_thresholds(
    body: &Value,
) -> Result<Option<CognitionThresholds>, CognitionError> {
    body.get("config")
        .and_then(|value| value.get("thresholds"))
        .map(parse_cognition_thresholds)
        .transpose()
}

/// `config.thresholds`, from a CONFIG_SET or the persisted config: `context_close` and
/// `reason_close`, a key left out takes its default. Any other key is refused, the retired
/// `context_open`/`reason_open` by name: they never gated opening, they only set the close
/// threshold, at half their value.
fn parse_cognition_thresholds(thresholds: &Value) -> Result<CognitionThresholds, CognitionError> {
    let Some(fields) = thresholds.as_object() else {
        return Err("config.thresholds must be an object (context_close, reason_close)".into());
    };
    if let Some(key) = fields
        .keys()
        .find(|key| !matches!(key.as_str(), "context_close" | "reason_close"))
    {
        let message = match key.as_str() {
            "context_open" | "reason_open" => {
                let entity = key.trim_end_matches("_open");
                format!(
                    "config.thresholds.{key} is no longer accepted: it never gated opening, and half its value was the weight under which an open {entity} closes. Set that weight as config.thresholds.{entity}_close"
                )
            }
            _ => format!(
                "config.thresholds.{key} is not a cognition threshold; the keys are context_close and reason_close"
            ),
        };
        return Err(message.into());
    }
    Ok(CognitionThresholds {
        context_close: close_threshold(
            fields,
            "context_close",
            COGNITION_DEFAULT_CONTEXT_CLOSE_THRESHOLD,
        )?,
        reason_close: close_threshold(
            fields,
            "reason_close",
            COGNITION_DEFAULT_REASON_CLOSE_THRESHOLD,
        )?,
    })
}

fn close_threshold(
    fields: &serde_json::Map<String, Value>,
    key: &str,
    default: f64,
) -> Result<f64, CognitionError> {
    let Some(value) = fields.get(key) else {
        return Ok(default);
    };
    value
        .as_f64()
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .ok_or_else(|| {
            format!("config.thresholds.{key} must be a finite number between 0 and 1").into()
        })
}

fn extract_cognition_semantic_tagger_config(
    body: &Value,
    current: &CognitionSemanticTaggerConfig,
) -> Result<Option<CognitionSemanticTaggerConfig>, CognitionError> {
    let Some(config) = body
        .get("config")
        .and_then(|value| value.get("semantic_tagger"))
    else {
        return Ok(None);
    };
    if config.get("provider").is_some() || config.get("model").is_some() {
        return Err(
            "config.semantic_tagger.provider/model are hive-wide; configure ai in hive.yaml".into(),
        );
    }
    let mut merged = current.clone();
    if let Some(timeout_ms) = config.get("timeout_ms").and_then(Value::as_u64) {
        if timeout_ms == 0 {
            return Err("config.semantic_tagger.timeout_ms must be > 0".into());
        }
        merged.timeout_ms = timeout_ms;
    }
    if let Some(max_tags) = config.get("max_tags").and_then(Value::as_u64) {
        if max_tags == 0 {
            return Err("config.semantic_tagger.max_tags must be > 0".into());
        }
        merged.max_tags = max_tags as usize;
    }
    if let Some(max_reason_signals) = config.get("max_reason_signals").and_then(Value::as_u64) {
        if max_reason_signals == 0 {
            return Err("config.semantic_tagger.max_reason_signals must be > 0".into());
        }
        merged.max_reason_signals = max_reason_signals as usize;
    }
    Ok(Some(merged))
}

fn extract_turn_text(payload: &Value) -> Option<String> {
    TextV1Payload::from_value(payload)
        .ok()
        .and_then(|value| value.content)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// One turn of a communication (`thread_id`, the agent `agent_ilk` that owns it, §6.2): its state
/// moves on, and the envelopes of what changed come back. The turn is numbered here; the number
/// orders the communication and guards its rows in SY.storage.
async fn update_thread_state_and_build_envelopes<S: NarrativeSummarizer>(
    hive_id: &str,
    writer: &str,
    thread_id: &str,
    agent_ilk: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    ich: Option<&str>,
    tagger: &SemanticTaggerOutput,
    thresholds: &CognitionThresholds,
    api_key: &str,
    account: &AiAccount,
    semantic_tagger_config: &CognitionSemanticTaggerConfig,
    ts: &str,
    thread_state: &mut ThreadCognitionState,
    narrative: &S,
) -> ThreadUpdateResult {
    if thread_state.first_seen_at.is_none() {
        thread_state.first_seen_at = Some(ts.to_string());
    }
    thread_state.last_seen_at = Some(ts.to_string());
    thread_state.turn_count = thread_state.turn_count.saturating_add(1);
    let comm = &CognitionCommunicationRef {
        thread_id: thread_id.to_string(),
        agent_ilk: agent_ilk.to_string(),
        turn: thread_state.turn_count,
    };

    let context_candidates = build_context_candidates(tagger);
    let reason_candidates = build_reason_candidates(tagger);

    let mut out = Vec::new();
    if let Some(body) = build_thread_envelope_body(
        hive_id,
        writer,
        comm,
        ts,
        thread_state,
        src_ilk,
        dst_ilk,
        ich,
    ) {
        out.push((SUBJECT_STORAGE_COGNITION_THREADS, body));
    }

    out.extend(update_contexts_for_thread(
        hive_id,
        writer,
        comm,
        ts,
        src_ilk,
        dst_ilk,
        thresholds.context_close,
        &context_candidates,
        &mut thread_state.contexts,
    ));
    out.extend(update_reasons_for_thread(
        hive_id,
        writer,
        comm,
        ts,
        src_ilk,
        dst_ilk,
        thresholds.reason_close,
        &reason_candidates,
        &mut thread_state.reasons,
    ));
    // A co-occurrence pairs a context with a reason, so it closes under the average of the two.
    out.extend(update_cooccurrences_for_thread(
        hive_id,
        writer,
        comm,
        ts,
        (thresholds.context_close + thresholds.reason_close) * 0.5,
        &context_candidates,
        &reason_candidates,
        &thread_state.contexts,
        &thread_state.reasons,
        &mut thread_state.cooccurrences,
    ));
    out.extend(update_scope_binding_for_thread(
        hive_id,
        writer,
        comm,
        ts,
        src_ilk,
        dst_ilk,
        thread_state.turn_count,
        &thread_state.contexts,
        &thread_state.reasons,
        &mut thread_state.active_scope,
    ));
    let memory_episode_result = update_memories_and_episodes_for_thread(
        hive_id,
        writer,
        comm,
        ts,
        src_ilk,
        dst_ilk,
        tagger,
        api_key,
        account,
        semantic_tagger_config,
        &thread_state.contexts,
        &thread_state.reasons,
        &thread_state.active_scope,
        &mut thread_state.memories,
        &mut thread_state.episodes,
        narrative,
    )
    .await;
    out.extend(memory_episode_result.envelopes);
    ThreadUpdateResult {
        envelopes: out,
        narrative: memory_episode_result.narrative,
    }
}

fn build_context_candidates(tagger: &SemanticTaggerOutput) -> Vec<ContextCandidate> {
    tagger
        .tags
        .iter()
        .map(|tag| ContextCandidate {
            label: tag.clone(),
            tags: vec![tag.clone()],
            score: 1.0,
        })
        .collect()
}

fn build_reason_candidates(tagger: &SemanticTaggerOutput) -> Vec<ReasonCandidate> {
    let signals: HashSet<&str> = tagger
        .reason_signals_canonical
        .iter()
        .map(String::as_str)
        .collect();
    let mut out = Vec::new();

    if signals.contains("resolve") && signals.contains("challenge") {
        out.push(ReasonCandidate {
            label: "seeking urgent resolution".to_string(),
            score: 1.0,
            signals_canonical: vec!["resolve".to_string(), "challenge".to_string()],
            signals_extra: tagger.reason_signals_extra.clone(),
        });
    }
    if signals.contains("inform") && signals.contains("confirm") {
        out.push(ReasonCandidate {
            label: "information verification".to_string(),
            score: 1.0,
            signals_canonical: vec!["inform".to_string(), "confirm".to_string()],
            signals_extra: tagger.reason_signals_extra.clone(),
        });
    }
    if signals.contains("request") && signals.contains("resolve") && !signals.contains("challenge")
    {
        out.push(ReasonCandidate {
            label: "seeking assistance".to_string(),
            score: 1.0,
            signals_canonical: vec!["request".to_string(), "resolve".to_string()],
            signals_extra: tagger.reason_signals_extra.clone(),
        });
    }

    for signal in &tagger.reason_signals_canonical {
        let label = match signal.as_str() {
            "resolve" => "seeking resolution",
            "inform" => "information exchange",
            "protect" => "risk containment",
            "connect" => "relationship maintenance",
            "challenge" => "confrontational pushback",
            "confirm" => "verification seeking",
            "request" => "seeking assistance",
            "abandon" => "withdrawal intent",
            _ => continue,
        };
        if out.iter().any(|candidate| candidate.label == label) {
            continue;
        }
        out.push(ReasonCandidate {
            label: label.to_string(),
            score: 1.0,
            signals_canonical: vec![signal.clone()],
            signals_extra: tagger.reason_signals_extra.clone(),
        });
    }

    out
}

fn build_thread_envelope_body(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    thread_state: &ThreadCognitionState,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    ich: Option<&str>,
) -> Option<Vec<u8>> {
    let data = CognitionThreadData {
        src_ilk: src_ilk.map(ToString::to_string),
        dst_ilk: dst_ilk.map(ToString::to_string),
        ich: ich.map(ToString::to_string),
        status: Some("open".to_string()),
        first_seen_at: thread_state.first_seen_at.clone(),
        last_seen_at: thread_state.last_seen_at.clone(),
        turn_count: Some(thread_state.turn_count),
    };
    let envelope = CognitionDurableEnvelope::new(
        CognitionDurableEntity::Thread,
        CognitionDurableOp::Upsert,
        communication_entity_id(comm),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        data,
    );
    serde_json::to_vec(&envelope).ok()
}

fn update_contexts_for_thread(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    close_threshold: f64,
    candidates: &[ContextCandidate],
    contexts: &mut HashMap<String, ContextState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let matched: HashSet<String> = candidates
        .iter()
        .map(|candidate| candidate.label.clone())
        .collect();

    for candidate in candidates {
        let context = contexts
            .entry(candidate.label.clone())
            .or_insert_with(|| ContextState {
                context_id: stable_entity_id(
                    "context",
                    &[&comm.thread_id, &comm.agent_ilk, &candidate.label],
                ),
                label: candidate.label.clone(),
                weight: 0.0,
                weight_avg_cumulative: 0.0,
                weight_avg_ema: 0.0,
                weight_samples: 0,
                tags: candidate.tags.clone(),
                ilk_weights: BTreeMap::new(),
                ilk_profile: BTreeMap::new(),
                opened_at: ts.to_string(),
                last_seen_at: ts.to_string(),
                closed_at: None,
                status: "open".to_string(),
            });
        context.status = "open".to_string();
        context.closed_at = None;
        context.last_seen_at = ts.to_string();
        context.tags = candidate.tags.clone();
        context.weight = context.weight * COGNITION_CONTEXT_DECAY_FACTOR + candidate.score;
        context.weight_samples = context.weight_samples.saturating_add(1);
        context.weight_avg_cumulative = update_cumulative_average(
            context.weight_avg_cumulative,
            candidate.score,
            context.weight_samples,
        );
        context.weight_avg_ema = update_ema(
            context.weight_avg_ema,
            candidate.score,
            COGNITION_CONTEXT_EMA_ALPHA,
        );
        apply_ilk_participation(
            &mut context.ilk_weights,
            &mut context.ilk_profile,
            src_ilk,
            dst_ilk,
        );

        let data = CognitionContextData {
            label: context.label.clone(),
            status: context.status.clone(),
            score: Some(candidate.score),
            weight: Some(context.weight),
            weight_avg_cumulative: Some(context.weight_avg_cumulative),
            weight_avg_ema: Some(context.weight_avg_ema),
            weight_samples: Some(context.weight_samples),
            tags: context.tags.clone(),
            ilk_weights: context.ilk_weights.clone(),
            ilk_profile: context.ilk_profile.clone(),
            opened_at: Some(context.opened_at.clone()),
            last_seen_at: Some(context.last_seen_at.clone()),
            closed_at: context.closed_at.clone(),
            ..CognitionContextData::default()
        };
        if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
            CognitionDurableEntity::Context,
            CognitionDurableOp::Upsert,
            context.context_id.clone(),
            Some(comm),
            hive_id.to_string(),
            writer.to_string(),
            ts.to_string(),
            data,
        )) {
            out.push((SUBJECT_STORAGE_COGNITION_CONTEXTS, body));
        }
    }

    for context in contexts.values_mut() {
        if matched.contains(&context.label) || context.status != "open" {
            continue;
        }
        context.weight *= COGNITION_CONTEXT_DECAY_FACTOR;
        context.last_seen_at = ts.to_string();
        if context.weight < close_threshold {
            context.status = "closed".to_string();
            context.closed_at = Some(ts.to_string());
            let data = CognitionContextData {
                label: context.label.clone(),
                status: context.status.clone(),
                weight: Some(context.weight),
                weight_avg_cumulative: Some(context.weight_avg_cumulative),
                weight_avg_ema: Some(context.weight_avg_ema),
                weight_samples: Some(context.weight_samples),
                tags: context.tags.clone(),
                ilk_weights: context.ilk_weights.clone(),
                ilk_profile: context.ilk_profile.clone(),
                opened_at: Some(context.opened_at.clone()),
                last_seen_at: Some(context.last_seen_at.clone()),
                closed_at: context.closed_at.clone(),
                ..CognitionContextData::default()
            };
            if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
                CognitionDurableEntity::Context,
                CognitionDurableOp::Close,
                context.context_id.clone(),
                Some(comm),
                hive_id.to_string(),
                writer.to_string(),
                ts.to_string(),
                data,
            )) {
                out.push((SUBJECT_STORAGE_COGNITION_CONTEXTS, body));
            }
        }
    }

    out
}

fn update_reasons_for_thread(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    close_threshold: f64,
    candidates: &[ReasonCandidate],
    reasons: &mut HashMap<String, ReasonState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let matched: HashSet<String> = candidates
        .iter()
        .map(|candidate| candidate.label.clone())
        .collect();

    for candidate in candidates {
        let reason = reasons
            .entry(candidate.label.clone())
            .or_insert_with(|| ReasonState {
                reason_id: stable_entity_id(
                    "reason",
                    &[&comm.thread_id, &comm.agent_ilk, &candidate.label],
                ),
                label: candidate.label.clone(),
                weight: 0.0,
                weight_avg_cumulative: 0.0,
                weight_avg_ema: 0.0,
                weight_samples: 0,
                signals_canonical: candidate.signals_canonical.clone(),
                signals_extra: candidate.signals_extra.clone(),
                ilk_weights: BTreeMap::new(),
                ilk_profile: BTreeMap::new(),
                opened_at: ts.to_string(),
                last_seen_at: ts.to_string(),
                closed_at: None,
                status: "open".to_string(),
            });
        reason.status = "open".to_string();
        reason.closed_at = None;
        reason.last_seen_at = ts.to_string();
        reason.signals_canonical = candidate.signals_canonical.clone();
        reason.signals_extra = candidate.signals_extra.clone();
        reason.weight = reason.weight * COGNITION_REASON_DECAY_FACTOR + candidate.score;
        reason.weight_samples = reason.weight_samples.saturating_add(1);
        reason.weight_avg_cumulative = update_cumulative_average(
            reason.weight_avg_cumulative,
            candidate.score,
            reason.weight_samples,
        );
        reason.weight_avg_ema = update_ema(
            reason.weight_avg_ema,
            candidate.score,
            COGNITION_REASON_EMA_ALPHA,
        );
        apply_ilk_participation(
            &mut reason.ilk_weights,
            &mut reason.ilk_profile,
            src_ilk,
            dst_ilk,
        );

        let data = CognitionReasonData {
            label: reason.label.clone(),
            status: reason.status.clone(),
            score: Some(candidate.score),
            weight: Some(reason.weight),
            weight_avg_cumulative: Some(reason.weight_avg_cumulative),
            weight_avg_ema: Some(reason.weight_avg_ema),
            weight_samples: Some(reason.weight_samples),
            signals_canonical: reason.signals_canonical.clone(),
            signals_extra: reason.signals_extra.clone(),
            ilk_weights: reason.ilk_weights.clone(),
            ilk_profile: reason.ilk_profile.clone(),
            opened_at: Some(reason.opened_at.clone()),
            last_seen_at: Some(reason.last_seen_at.clone()),
            closed_at: reason.closed_at.clone(),
            ..CognitionReasonData::default()
        };
        if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
            CognitionDurableEntity::Reason,
            CognitionDurableOp::Upsert,
            reason.reason_id.clone(),
            Some(comm),
            hive_id.to_string(),
            writer.to_string(),
            ts.to_string(),
            data,
        )) {
            out.push((SUBJECT_STORAGE_COGNITION_REASONS, body));
        }
    }

    for reason in reasons.values_mut() {
        if matched.contains(&reason.label) || reason.status != "open" {
            continue;
        }
        reason.weight *= COGNITION_REASON_DECAY_FACTOR;
        reason.last_seen_at = ts.to_string();
        if reason.weight < close_threshold {
            reason.status = "closed".to_string();
            reason.closed_at = Some(ts.to_string());
            let data = CognitionReasonData {
                label: reason.label.clone(),
                status: reason.status.clone(),
                weight: Some(reason.weight),
                weight_avg_cumulative: Some(reason.weight_avg_cumulative),
                weight_avg_ema: Some(reason.weight_avg_ema),
                weight_samples: Some(reason.weight_samples),
                signals_canonical: reason.signals_canonical.clone(),
                signals_extra: reason.signals_extra.clone(),
                ilk_weights: reason.ilk_weights.clone(),
                ilk_profile: reason.ilk_profile.clone(),
                opened_at: Some(reason.opened_at.clone()),
                last_seen_at: Some(reason.last_seen_at.clone()),
                closed_at: reason.closed_at.clone(),
                ..CognitionReasonData::default()
            };
            if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
                CognitionDurableEntity::Reason,
                CognitionDurableOp::Close,
                reason.reason_id.clone(),
                Some(comm),
                hive_id.to_string(),
                writer.to_string(),
                ts.to_string(),
                data,
            )) {
                out.push((SUBJECT_STORAGE_COGNITION_REASONS, body));
            }
        }
    }

    out
}

fn update_cooccurrences_for_thread(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    close_threshold: f64,
    context_candidates: &[ContextCandidate],
    reason_candidates: &[ReasonCandidate],
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    cooccurrences: &mut HashMap<String, CooccurrenceState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let mut matched = HashSet::new();

    for context_candidate in context_candidates {
        let Some(context) = contexts.get(&context_candidate.label) else {
            continue;
        };
        if context.status != "open" {
            continue;
        }
        for reason_candidate in reason_candidates {
            let Some(reason) = reasons.get(&reason_candidate.label) else {
                continue;
            };
            if reason.status != "open" {
                continue;
            }

            let pair_key = format!("{}|{}", context.label, reason.label);
            matched.insert(pair_key.clone());
            let score = (context_candidate.score + reason_candidate.score) * 0.5;
            let cooccurrence = cooccurrences
                .entry(pair_key)
                .or_insert_with(|| CooccurrenceState {
                    cooccurrence_id: stable_entity_id(
                        "cooccurrence",
                        &[
                            &comm.thread_id,
                            &comm.agent_ilk,
                            &context.context_id,
                            &reason.reason_id,
                        ],
                    ),
                    context_id: context.context_id.clone(),
                    context_label: context.label.clone(),
                    reason_id: reason.reason_id.clone(),
                    reason_label: reason.label.clone(),
                    weight: 0.0,
                    weight_avg_cumulative: 0.0,
                    weight_avg_ema: 0.0,
                    weight_samples: 0,
                    occurrences: 0,
                    opened_at: ts.to_string(),
                    last_seen_at: ts.to_string(),
                    closed_at: None,
                    status: "open".to_string(),
                });
            cooccurrence.context_id = context.context_id.clone();
            cooccurrence.context_label = context.label.clone();
            cooccurrence.reason_id = reason.reason_id.clone();
            cooccurrence.reason_label = reason.label.clone();
            cooccurrence.status = "open".to_string();
            cooccurrence.closed_at = None;
            cooccurrence.last_seen_at = ts.to_string();
            cooccurrence.weight = cooccurrence.weight * COGNITION_COOCCURRENCE_DECAY_FACTOR + score;
            cooccurrence.weight_samples = cooccurrence.weight_samples.saturating_add(1);
            cooccurrence.occurrences = cooccurrence.occurrences.saturating_add(1);
            cooccurrence.weight_avg_cumulative = update_cumulative_average(
                cooccurrence.weight_avg_cumulative,
                score,
                cooccurrence.weight_samples,
            );
            cooccurrence.weight_avg_ema = update_ema(
                cooccurrence.weight_avg_ema,
                score,
                COGNITION_COOCCURRENCE_EMA_ALPHA,
            );

            let data = CognitionCooccurrenceData {
                context_id: cooccurrence.context_id.clone(),
                context_label: cooccurrence.context_label.clone(),
                reason_id: cooccurrence.reason_id.clone(),
                reason_label: cooccurrence.reason_label.clone(),
                status: cooccurrence.status.clone(),
                score: Some(score),
                weight: Some(cooccurrence.weight),
                weight_avg_cumulative: Some(cooccurrence.weight_avg_cumulative),
                weight_avg_ema: Some(cooccurrence.weight_avg_ema),
                weight_samples: Some(cooccurrence.weight_samples),
                occurrences: Some(cooccurrence.occurrences),
                opened_at: Some(cooccurrence.opened_at.clone()),
                last_seen_at: Some(cooccurrence.last_seen_at.clone()),
                closed_at: cooccurrence.closed_at.clone(),
            };
            if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
                CognitionDurableEntity::Cooccurrence,
                CognitionDurableOp::Upsert,
                cooccurrence.cooccurrence_id.clone(),
                Some(comm),
                hive_id.to_string(),
                writer.to_string(),
                ts.to_string(),
                data,
            )) {
                out.push((SUBJECT_STORAGE_COGNITION_COOCCURRENCES, body));
            }
        }
    }

    for (pair_key, cooccurrence) in cooccurrences.iter_mut() {
        if matched.contains(pair_key) || cooccurrence.status != "open" {
            continue;
        }
        cooccurrence.weight *= COGNITION_COOCCURRENCE_DECAY_FACTOR;
        cooccurrence.last_seen_at = ts.to_string();
        if cooccurrence.weight < close_threshold {
            cooccurrence.status = "closed".to_string();
            cooccurrence.closed_at = Some(ts.to_string());
            let data = CognitionCooccurrenceData {
                context_id: cooccurrence.context_id.clone(),
                context_label: cooccurrence.context_label.clone(),
                reason_id: cooccurrence.reason_id.clone(),
                reason_label: cooccurrence.reason_label.clone(),
                status: cooccurrence.status.clone(),
                score: None,
                weight: Some(cooccurrence.weight),
                weight_avg_cumulative: Some(cooccurrence.weight_avg_cumulative),
                weight_avg_ema: Some(cooccurrence.weight_avg_ema),
                weight_samples: Some(cooccurrence.weight_samples),
                occurrences: Some(cooccurrence.occurrences),
                opened_at: Some(cooccurrence.opened_at.clone()),
                last_seen_at: Some(cooccurrence.last_seen_at.clone()),
                closed_at: cooccurrence.closed_at.clone(),
            };
            if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
                CognitionDurableEntity::Cooccurrence,
                CognitionDurableOp::Close,
                cooccurrence.cooccurrence_id.clone(),
                Some(comm),
                hive_id.to_string(),
                writer.to_string(),
                ts.to_string(),
                data,
            )) {
                out.push((SUBJECT_STORAGE_COGNITION_COOCCURRENCES, body));
            }
        }
    }

    out
}

fn update_scope_binding_for_thread(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    turn_count: u64,
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    active_scope: &mut Option<ScopeBindingState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    // On a weight tie the scope's own context/reason stays dominant. HashMap order used to
    // pick one, so a tag reinforced together with the incumbent could pose as a topic change.
    let incumbent = active_scope.as_ref();
    let Some(context) = select_dominant_context(
        contexts,
        incumbent.map(|scope| scope.dominant_context_id.as_str()),
    ) else {
        return out;
    };
    let Some(reason) = select_dominant_reason(
        reasons,
        incumbent.map(|scope| scope.dominant_reason_id.as_str()),
    ) else {
        return out;
    };

    let current_label = format!("{} :: {}", context.label, reason.label);
    let current_ilk_weights = current_turn_ilk_weights(src_ilk, dst_ilk);

    match active_scope {
        None => {
            let opened = ScopeBindingState {
                scope_id: format!("scope:{}", Uuid::new_v4()),
                scope_instance_id: format!("scope_instance:{}", Uuid::new_v4()),
                label: current_label,
                dominant_context_id: context.context_id.clone(),
                dominant_context_label: context.label.clone(),
                dominant_context_tags: context.tags.clone(),
                dominant_reason_id: reason.reason_id.clone(),
                dominant_reason_label: reason.label.clone(),
                dominant_reason_signals: reason.signals_canonical.clone(),
                ilk_weights: current_ilk_weights,
                binding_energy_ema: 1.0,
                opened_at: ts.to_string(),
                last_seen_at: ts.to_string(),
                start_turn: Some(comm.turn),
                unbind_streak: 0,
            };
            out.extend(build_scope_upsert_events(
                hive_id, writer, comm, ts, &opened,
            ));
            *active_scope = Some(opened);
        }
        Some(scope) => {
            let ctx_similarity = jaccard_similarity(&scope.dominant_context_tags, &context.tags);
            let reason_similarity =
                jaccard_similarity(&scope.dominant_reason_signals, &reason.signals_canonical);
            let ilk_similarity = cosine_similarity(&scope.ilk_weights, &current_ilk_weights);
            let binding = ctx_similarity * reason_similarity * ilk_similarity;
            scope.binding_energy_ema = update_ema(
                scope.binding_energy_ema,
                binding,
                COGNITION_SCOPE_ENERGY_ALPHA,
            );

            let candidate_shift = scope.dominant_context_id != context.context_id
                || scope.dominant_reason_id != reason.reason_id;
            if candidate_shift
                && turn_count >= 2
                && scope.binding_energy_ema < COGNITION_SCOPE_UNBIND_THRESHOLD
            {
                scope.unbind_streak = scope.unbind_streak.saturating_add(1);
            } else {
                scope.unbind_streak = 0;
            }

            if candidate_shift && scope.unbind_streak >= COGNITION_SCOPE_SUSTAIN_COUNT {
                out.extend(build_scope_close_events(hive_id, writer, comm, ts, scope));
                let opened = ScopeBindingState {
                    scope_id: format!("scope:{}", Uuid::new_v4()),
                    scope_instance_id: format!("scope_instance:{}", Uuid::new_v4()),
                    label: current_label,
                    dominant_context_id: context.context_id.clone(),
                    dominant_context_label: context.label.clone(),
                    dominant_context_tags: context.tags.clone(),
                    dominant_reason_id: reason.reason_id.clone(),
                    dominant_reason_label: reason.label.clone(),
                    dominant_reason_signals: reason.signals_canonical.clone(),
                    ilk_weights: current_ilk_weights,
                    binding_energy_ema: binding,
                    opened_at: ts.to_string(),
                    last_seen_at: ts.to_string(),
                    start_turn: Some(comm.turn),
                    unbind_streak: 0,
                };
                out.extend(build_scope_upsert_events(
                    hive_id, writer, comm, ts, &opened,
                ));
                *scope = opened;
            } else if candidate_shift && binding < COGNITION_SCOPE_UNBIND_THRESHOLD {
                // The candidate does not bind to this scope: the shift is pending. The scope
                // keeps its own context/reason so the next turns keep measuring the divergence
                // until the sustain count cuts it (§3.2, §7.2). Relabeling here reset the
                // streak, so a lasting topic change renamed the scope instead of cutting it.
                scope.last_seen_at = ts.to_string();
                out.extend(build_scope_upsert_events(hive_id, writer, comm, ts, scope));
            } else {
                scope.label = current_label;
                scope.dominant_context_id = context.context_id.clone();
                scope.dominant_context_label = context.label.clone();
                scope.dominant_context_tags = context.tags.clone();
                scope.dominant_reason_id = reason.reason_id.clone();
                scope.dominant_reason_label = reason.label.clone();
                scope.dominant_reason_signals = reason.signals_canonical.clone();
                scope.ilk_weights = current_ilk_weights;
                scope.last_seen_at = ts.to_string();
                out.extend(build_scope_upsert_events(hive_id, writer, comm, ts, scope));
            }
        }
    }

    out
}

async fn update_memories_and_episodes_for_thread<S: NarrativeSummarizer>(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    tagger: &SemanticTaggerOutput,
    api_key: &str,
    account: &AiAccount,
    semantic_tagger_config: &CognitionSemanticTaggerConfig,
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    active_scope: &Option<ScopeBindingState>,
    memories: &mut HashMap<String, MemoryState>,
    episodes: &mut HashMap<String, EpisodeState>,
    narrative: &S,
) -> ThreadUpdateResult {
    let mut out = Vec::new();
    let Some(scope) = active_scope.as_ref() else {
        return ThreadUpdateResult::default();
    };
    let Some(context) = contexts.get(&scope.dominant_context_label) else {
        return ThreadUpdateResult::default();
    };
    let Some(reason) = reasons.get(&scope.dominant_reason_label) else {
        return ThreadUpdateResult::default();
    };

    let episode_candidate = build_episode_candidate(tagger, context);
    let episode_existing = episode_candidate.as_ref().and_then(|candidate| {
        let episode_key = format!("{}|{}", scope.scope_instance_id, candidate.affect_id);
        episodes.get(&episode_key)
    });

    let narrative_result = match narrative
        .summarize(NarrativeSummarizerAiInput {
            api_key,
            account,
            config: semantic_tagger_config,
            thread_id: &comm.thread_id,
            context_label: &context.label,
            context_tags: &context.tags,
            reason_label: &reason.label,
            canonical_signals: &reason.signals_canonical,
            extra_signals: &tagger.reason_signals_extra,
            previous_memory_summary: memories.get(&scope.scope_id).map(|m| m.summary.as_str()),
            episode_affect_id: episode_candidate.as_ref().map(|c| c.affect_id.as_str()),
            episode_title: episode_candidate.as_ref().map(|c| c.title.as_str()),
            previous_episode_summary: episode_existing.map(|episode| episode.summary.as_str()),
            previous_episode_reason: episode_existing.map(|episode| episode.reason.as_str()),
        })
        .await
    {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                thread_id = %comm.thread_id,
                context = %context.label,
                reason = %reason.label,
                error = %err,
                "sy.cognition narrative summarizer failed; skipping memory/episode synthesis for turn"
            );
            return ThreadUpdateResult {
                envelopes: out,
                narrative: NarrativeOutcome {
                    called: true,
                    failed: !matches!(
                        err,
                        fluxbee_ai_sdk::AiSdkError::Json(_)
                            | fluxbee_ai_sdk::AiSdkError::Protocol(_)
                    ),
                    invalid_output: matches!(
                        err,
                        fluxbee_ai_sdk::AiSdkError::Json(_)
                            | fluxbee_ai_sdk::AiSdkError::Protocol(_)
                    ),
                },
            };
        }
    };

    let current_ilk_weights = current_turn_ilk_weights(src_ilk, dst_ilk);
    out.extend(update_memories_for_thread(
        hive_id,
        writer,
        comm,
        ts,
        scope,
        context,
        reason,
        &narrative_result.memory_summary,
        &current_ilk_weights,
        memories,
    ));
    // The summarizer must return the episode text whenever a candidate was sent (else the
    // call fails above), so an episode is the candidate's gate fields plus the AI's text.
    if let Some((candidate, episode_narrative)) =
        episode_candidate.zip(narrative_result.episode.as_ref())
    {
        out.extend(update_episodes_for_thread(
            hive_id,
            writer,
            comm,
            ts,
            candidate,
            episode_narrative,
            scope,
            context,
            reason,
            episodes,
        ));
    }
    ThreadUpdateResult {
        envelopes: out,
        narrative: NarrativeOutcome {
            called: true,
            failed: false,
            invalid_output: false,
        },
    }
}

fn update_memories_for_thread(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    scope: &ScopeBindingState,
    context: &ContextState,
    reason: &ReasonState,
    narrative_summary: &str,
    current_ilk_weights: &BTreeMap<String, f64>,
    memories: &mut HashMap<String, MemoryState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let memory_key = scope.scope_id.clone();
    let summary = narrative_summary.to_string();
    let memory = memories
        .entry(memory_key.clone())
        .or_insert_with(|| MemoryState {
            memory_id: stable_entity_id(
                "memory",
                &[&comm.thread_id, &comm.agent_ilk, &scope.scope_id],
            ),
            scope_id: scope.scope_id.clone(),
            summary: summary.clone(),
            weight: 0.0,
            occurrences: 0,
            dominant_context_id: context.context_id.clone(),
            dominant_reason_id: reason.reason_id.clone(),
            ilk_weights: BTreeMap::new(),
            created_at: ts.to_string(),
            last_seen_at: ts.to_string(),
        });
    memory.summary = summary;
    memory.scope_id = scope.scope_id.clone();
    memory.weight =
        memory.weight * COGNITION_MEMORY_DECAY_FACTOR + scope.binding_energy_ema.max(0.25);
    memory.occurrences = memory.occurrences.saturating_add(1);
    memory.dominant_context_id = context.context_id.clone();
    memory.dominant_reason_id = reason.reason_id.clone();
    merge_ilk_weights(&mut memory.ilk_weights, current_ilk_weights);
    memory.last_seen_at = ts.to_string();

    let data = CognitionMemoryData {
        summary: memory.summary.clone(),
        scope_id: Some(memory.scope_id.clone()),
        weight: Some(memory.weight),
        occurrences: Some(memory.occurrences),
        dominant_context_id: Some(memory.dominant_context_id.clone()),
        dominant_reason_id: Some(memory.dominant_reason_id.clone()),
        ilk_weights: memory.ilk_weights.clone(),
        created_at: Some(memory.created_at.clone()),
        last_seen_at: Some(memory.last_seen_at.clone()),
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::Memory,
        CognitionDurableOp::Upsert,
        memory.memory_id.clone(),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_MEMORIES, body));
    }

    for (key, other_memory) in memories.iter_mut() {
        if key == &memory_key {
            continue;
        }
        other_memory.weight *= COGNITION_MEMORY_DECAY_FACTOR;
        other_memory.last_seen_at = ts.to_string();
        let data = CognitionMemoryData {
            summary: other_memory.summary.clone(),
            scope_id: Some(other_memory.scope_id.clone()),
            weight: Some(other_memory.weight),
            occurrences: Some(other_memory.occurrences),
            dominant_context_id: Some(other_memory.dominant_context_id.clone()),
            dominant_reason_id: Some(other_memory.dominant_reason_id.clone()),
            ilk_weights: other_memory.ilk_weights.clone(),
            created_at: Some(other_memory.created_at.clone()),
            last_seen_at: Some(other_memory.last_seen_at.clone()),
        };
        if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
            CognitionDurableEntity::Memory,
            CognitionDurableOp::Upsert,
            other_memory.memory_id.clone(),
            Some(comm),
            hive_id.to_string(),
            writer.to_string(),
            ts.to_string(),
            data,
        )) {
            out.push((SUBJECT_STORAGE_COGNITION_MEMORIES, body));
        }
    }

    out
}

fn update_episodes_for_thread(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    candidate: EpisodeCandidate,
    episode_narrative: &EpisodeNarrative,
    scope: &ScopeBindingState,
    context: &ContextState,
    reason: &ReasonState,
    episodes: &mut HashMap<String, EpisodeState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let episode_key = format!("{}|{}", scope.scope_instance_id, candidate.affect_id);
    let episode = episodes.entry(episode_key).or_insert_with(|| EpisodeState {
        episode_id: stable_entity_id(
            "episode",
            &[
                &comm.thread_id,
                &comm.agent_ilk,
                &scope.scope_instance_id,
                &candidate.affect_id,
            ],
        ),
        scope_id: scope.scope_id.clone(),
        scope_instance_id: scope.scope_instance_id.clone(),
        affect_id: candidate.affect_id.clone(),
        title: candidate.title.clone(),
        summary: episode_narrative.summary.clone(),
        base_intensity: candidate.base_intensity,
        evidence_strength: candidate.evidence_strength,
        evidence_context_ids: vec![context.context_id.clone()],
        evidence_reason_ids: vec![reason.reason_id.clone()],
        evidence_signals: candidate.evidence_signals.clone(),
        intensity: candidate.base_intensity,
        reason: episode_narrative.reason.clone(),
        created_at: ts.to_string(),
    });

    episode.scope_id = scope.scope_id.clone();
    episode.scope_instance_id = scope.scope_instance_id.clone();
    episode.title = candidate.title.clone();
    episode.summary = episode_narrative.summary.clone();
    episode.base_intensity = episode.base_intensity.max(candidate.base_intensity);
    episode.evidence_strength = episode.evidence_strength.max(candidate.evidence_strength);
    episode.intensity = episode.intensity.max(candidate.base_intensity);
    episode.reason = episode_narrative.reason.clone();
    for context_id in [context.context_id.clone()] {
        if !episode.evidence_context_ids.contains(&context_id) {
            episode.evidence_context_ids.push(context_id);
        }
    }
    for reason_id in [reason.reason_id.clone()] {
        if !episode.evidence_reason_ids.contains(&reason_id) {
            episode.evidence_reason_ids.push(reason_id);
        }
    }
    for signal in &candidate.evidence_signals {
        if !episode.evidence_signals.contains(signal) {
            episode.evidence_signals.push(signal.clone());
        }
    }

    let data = CognitionEpisodeData {
        affect_id: episode.affect_id.clone(),
        title: episode.title.clone(),
        summary: episode.summary.clone(),
        scope_id: Some(episode.scope_id.clone()),
        base_intensity: Some(episode.base_intensity),
        evidence_strength: Some(episode.evidence_strength),
        evidence_context_ids: episode.evidence_context_ids.clone(),
        evidence_reason_ids: episode.evidence_reason_ids.clone(),
        evidence_signals: episode.evidence_signals.clone(),
        intensity: Some(episode.intensity),
        reason: Some(episode.reason.clone()),
        created_at: Some(episode.created_at.clone()),
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::Episode,
        CognitionDurableOp::Upsert,
        episode.episode_id.clone(),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_EPISODES, body));
    }

    out
}

/// The deterministic episode gate: an affect fires only on these canonical + extra signal
/// combinations. The summary and reason are the narrative summarizer's.
fn build_episode_candidate(
    tagger: &SemanticTaggerOutput,
    context: &ContextState,
) -> Option<EpisodeCandidate> {
    let extra_signals: HashSet<&str> = tagger
        .reason_signals_extra
        .iter()
        .map(String::as_str)
        .collect();
    let canonical_signals: HashSet<&str> = tagger
        .reason_signals_canonical
        .iter()
        .map(String::as_str)
        .collect();

    let mut affect_id = None;
    let mut title = String::new();
    let mut evidence_signals = Vec::new();
    let mut base_intensity = 0.0;
    let mut evidence_strength = 0.0;

    if extra_signals.contains("frustration")
        && (canonical_signals.contains("challenge") || canonical_signals.contains("resolve"))
    {
        affect_id = Some("anger".to_string());
        title = format!("Friction around {}", context.label);
        evidence_signals.extend(["frustration".to_string(), "challenge".to_string()]);
        base_intensity = 8.0;
        evidence_strength = 9.0;
    } else if extra_signals.contains("escalation")
        && (canonical_signals.contains("protect") || canonical_signals.contains("challenge"))
    {
        affect_id = Some("escalation".to_string());
        title = format!("Escalation around {}", context.label);
        evidence_signals.extend(["escalation".to_string(), "protect".to_string()]);
        base_intensity = 8.0;
        evidence_strength = 9.0;
    } else if extra_signals.contains("urgency")
        && canonical_signals.contains("resolve")
        && canonical_signals.contains("request")
    {
        affect_id = Some("urgency".to_string());
        title = format!("Urgent push on {}", context.label);
        evidence_signals.extend([
            "urgency".to_string(),
            "request".to_string(),
            "resolve".to_string(),
        ]);
        base_intensity = 7.0;
        evidence_strength = 8.0;
    }

    if base_intensity < COGNITION_EPISODE_MIN_INTENSITY
        || evidence_strength < COGNITION_EPISODE_MIN_EVIDENCE_STRENGTH
    {
        return None;
    }

    Some(EpisodeCandidate {
        affect_id: affect_id?,
        title,
        base_intensity,
        evidence_strength,
        evidence_signals,
    })
}

fn build_scope_upsert_events(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    scope: &ScopeBindingState,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let scope_data = CognitionScopeData {
        status: "open".to_string(),
        label: Some(scope.label.clone()),
        dominant_context_id: Some(scope.dominant_context_id.clone()),
        dominant_reason_id: Some(scope.dominant_reason_id.clone()),
        binding_energy_ema: Some(scope.binding_energy_ema),
        opened_at: Some(scope.opened_at.clone()),
        last_seen_at: Some(scope.last_seen_at.clone()),
        closed_at: None,
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::Scope,
        CognitionDurableOp::Upsert,
        scope.scope_id.clone(),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        scope_data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_SCOPES, body));
    }

    let scope_instance_data = CognitionScopeInstanceData {
        scope_id: scope.scope_id.clone(),
        dominant_context_id: Some(scope.dominant_context_id.clone()),
        dominant_reason_id: Some(scope.dominant_reason_id.clone()),
        start_turn: scope.start_turn,
        end_turn: Some(comm.turn),
        opened_at: Some(scope.opened_at.clone()),
        closed_at: None,
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::ScopeInstance,
        CognitionDurableOp::Upsert,
        scope.scope_instance_id.clone(),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        scope_instance_data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES, body));
    }
    out
}

fn build_scope_close_events(
    hive_id: &str,
    writer: &str,
    comm: &CognitionCommunicationRef,
    ts: &str,
    scope: &ScopeBindingState,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let scope_data = CognitionScopeData {
        status: "closed".to_string(),
        label: Some(scope.label.clone()),
        dominant_context_id: Some(scope.dominant_context_id.clone()),
        dominant_reason_id: Some(scope.dominant_reason_id.clone()),
        binding_energy_ema: Some(scope.binding_energy_ema),
        opened_at: Some(scope.opened_at.clone()),
        last_seen_at: Some(ts.to_string()),
        closed_at: Some(ts.to_string()),
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::Scope,
        CognitionDurableOp::Close,
        scope.scope_id.clone(),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        scope_data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_SCOPES, body));
    }

    let scope_instance_data = CognitionScopeInstanceData {
        scope_id: scope.scope_id.clone(),
        dominant_context_id: Some(scope.dominant_context_id.clone()),
        dominant_reason_id: Some(scope.dominant_reason_id.clone()),
        start_turn: scope.start_turn,
        end_turn: Some(comm.turn),
        opened_at: Some(scope.opened_at.clone()),
        closed_at: Some(ts.to_string()),
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::ScopeInstance,
        CognitionDurableOp::Close,
        scope.scope_instance_id.clone(),
        Some(comm),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        scope_instance_data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES, body));
    }
    out
}

fn select_dominant_context<'a>(
    contexts: &'a HashMap<String, ContextState>,
    incumbent_id: Option<&str>,
) -> Option<&'a ContextState> {
    contexts
        .values()
        .filter(|context| context.status == "open")
        .max_by(|left, right| {
            dominance_order(
                (left.weight, &left.context_id, &left.label),
                (right.weight, &right.context_id, &right.label),
                incumbent_id,
            )
        })
}

fn select_dominant_reason<'a>(
    reasons: &'a HashMap<String, ReasonState>,
    incumbent_id: Option<&str>,
) -> Option<&'a ReasonState> {
    reasons
        .values()
        .filter(|reason| reason.status == "open")
        .max_by(|left, right| {
            dominance_order(
                (left.weight, &left.reason_id, &left.label),
                (right.weight, &right.reason_id, &right.label),
                incumbent_id,
            )
        })
}

/// Higher weight dominates. On a tie the incumbent (the active scope's own entity) keeps it,
/// then the smaller label: deterministic, where HashMap order used to decide.
fn dominance_order(
    (left_weight, left_id, left_label): (f64, &str, &str),
    (right_weight, right_id, right_label): (f64, &str, &str),
    incumbent_id: Option<&str>,
) -> std::cmp::Ordering {
    left_weight
        .total_cmp(&right_weight)
        .then_with(|| (Some(left_id) == incumbent_id).cmp(&(Some(right_id) == incumbent_id)))
        .then_with(|| right_label.cmp(left_label))
}

/// The interlocutors of a turn, weighing the same whichever spoke: a reply carries the agent's ILK
/// as sender (§6.2), and weighing the sender higher would make every reply look like a topic shift
/// to the scope binding.
fn current_turn_ilk_weights(src_ilk: Option<&str>, dst_ilk: Option<&str>) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for ilk in [src_ilk, dst_ilk]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.insert(ilk.to_string(), 1.0);
    }
    out
}

/// A communication's thread row id: the thread and the agent that owns it (§6.2).
fn communication_entity_id(comm: &CognitionCommunicationRef) -> String {
    stable_entity_id("thread", &[&comm.thread_id, &comm.agent_ilk])
}

fn jaccard_similarity(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let left_set: HashSet<&str> = left.iter().map(String::as_str).collect();
    let right_set: HashSet<&str> = right.iter().map(String::as_str).collect();
    let intersection = left_set.intersection(&right_set).count() as f64;
    let union = left_set.union(&right_set).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn cosine_similarity(left: &BTreeMap<String, f64>, right: &BTreeMap<String, f64>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;

    for value in left.values() {
        left_norm += value * value;
    }
    for value in right.values() {
        right_norm += value * value;
    }
    for (key, left_value) in left {
        if let Some(right_value) = right.get(key) {
            dot += left_value * right_value;
        }
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

fn merge_ilk_weights(target: &mut BTreeMap<String, f64>, source: &BTreeMap<String, f64>) {
    for (ilk, weight) in source {
        *target.entry(ilk.clone()).or_insert(0.0) += weight;
    }
}

fn apply_ilk_participation(
    ilk_weights: &mut BTreeMap<String, f64>,
    ilk_profile: &mut BTreeMap<String, CognitionIlkProfile>,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
) {
    if let Some(src_ilk) = src_ilk.map(str::trim).filter(|value| !value.is_empty()) {
        *ilk_weights.entry(src_ilk.to_string()).or_insert(0.0) += 1.0;
        let profile = ilk_profile.entry(src_ilk.to_string()).or_default();
        profile.as_sender = profile.as_sender.saturating_add(1);
    }
    if let Some(dst_ilk) = dst_ilk.map(str::trim).filter(|value| !value.is_empty()) {
        *ilk_weights.entry(dst_ilk.to_string()).or_insert(0.0) += 0.5;
        let profile = ilk_profile.entry(dst_ilk.to_string()).or_default();
        profile.as_receiver = profile.as_receiver.saturating_add(1);
    }
}

fn stable_entity_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    for part in parts {
        hasher.update(b"|");
        hasher.update(part.as_bytes());
    }
    format!("{prefix}:{}", hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn update_cumulative_average(current: f64, new_value: f64, samples: u64) -> f64 {
    if samples <= 1 {
        new_value
    } else {
        ((current * (samples.saturating_sub(1) as f64)) + new_value) / (samples as f64)
    }
}

fn update_ema(current: f64, new_value: f64, alpha: f64) -> f64 {
    if current == 0.0 {
        new_value
    } else {
        alpha * new_value + (1.0 - alpha) * current
    }
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, CognitionError> {
    let path = config_dir.join("hive.yaml");
    let raw = fs::read_to_string(&path)?;
    Ok(serde_yaml::from_str(&raw)?)
}

const RPC_CH_SYSTEM: &str = "system";

fn build_cognition_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    if trimmed.contains('@') {
        trimmed.to_string()
    } else {
        format!("{trimmed}@{hive_id}")
    }
}

fn write_json_atomic(path: &Path, body: &str) -> Result<(), CognitionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        use std::io::Write;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir_file) = OpenOptions::new().read(true).open(parent) {
            let _ = dir_file.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_THREAD: &str = "thread:sha256:test";
    const TEST_AGENT: &str = "ilk:agent";

    fn key(thread_id: &str) -> CommunicationKey {
        CommunicationKey {
            thread_id: thread_id.to_string(),
            agent_ilk: TEST_AGENT.to_string(),
        }
    }

    /// Answers like the AI does, from the input: memory text always, episode text exactly when
    /// the turn carries an episode candidate (the contract `parse_narrative_summaries` enforces).
    struct CannedNarrative;

    impl NarrativeSummarizer for CannedNarrative {
        fn summarize<'a>(
            &'a self,
            input: NarrativeSummarizerAiInput<'a>,
        ) -> impl Future<Output = Result<NarrativeSummaries, fluxbee_ai_sdk::AiSdkError>> + Send + 'a
        {
            let memory_summary = format!(
                "Memory of {} driven by {}.",
                input.context_label, input.reason_label
            );
            let episode = input.episode_affect_id.map(|affect| EpisodeNarrative {
                summary: format!("AI episode summary ({affect})."),
                reason: format!("AI episode reason ({affect})."),
            });
            async move {
                Ok(NarrativeSummaries {
                    memory_summary,
                    episode,
                })
            }
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// One turn through the live path, with a canned narrative instead of the AI.
    async fn live_turn(
        thread: &mut ThreadCognitionState,
        seq: u64,
        tags: &[&str],
        canonical: &[&str],
        extra: &[&str],
    ) -> Vec<(&'static str, Vec<u8>)> {
        live_turn_with(
            &CognitionThresholds::default(),
            thread,
            seq,
            tags,
            canonical,
            extra,
        )
        .await
    }

    async fn live_turn_with(
        thresholds: &CognitionThresholds,
        thread: &mut ThreadCognitionState,
        seq: u64,
        tags: &[&str],
        canonical: &[&str],
        extra: &[&str],
    ) -> Vec<(&'static str, Vec<u8>)> {
        let tagger = SemanticTaggerOutput {
            tags: strings(tags),
            reason_signals_canonical: strings(canonical),
            reason_signals_extra: strings(extra),
        };
        let ts = format!("2026-01-01T{:02}:{:02}:00Z", seq / 60, seq % 60);
        update_thread_state_and_build_envelopes(
            "motherbee",
            "SY.cognition@motherbee",
            TEST_THREAD,
            TEST_AGENT,
            Some("ilk:juan"),
            Some("ilk:agent"),
            Some("ich:test"),
            &tagger,
            thresholds,
            "test-key",
            &AiAccount::root(UsageRecorder::disabled(), "SY.cognition@motherbee"),
            &CognitionSemanticTaggerConfig::default(),
            &ts,
            thread,
            &CannedNarrative,
        )
        .await
        .envelopes
    }

    fn envelopes_of(
        out: &[(&'static str, Vec<u8>)],
        subject: &str,
    ) -> Vec<CognitionDurableEnvelope<Value>> {
        out.iter()
            .filter(|(published_to, _)| *published_to == subject)
            .map(|(_, body)| serde_json::from_slice(body).expect("envelope"))
            .collect()
    }

    fn closed_scope_ids(out: &[(&'static str, Vec<u8>)]) -> Vec<String> {
        envelopes_of(out, SUBJECT_STORAGE_COGNITION_SCOPES)
            .into_iter()
            .filter(|envelope| envelope.op == CognitionDurableOp::Close)
            .map(|envelope| envelope.entity_id)
            .collect()
    }

    fn sorted_keys<V>(map: &HashMap<String, V>) -> Vec<String> {
        let mut keys: Vec<String> = map.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// What SY.storage keeps of the published envelopes and returns for a communication: one row
    /// per entity id, the last write winning (`persist_cognition_*`).
    fn durable_rows(published: &[(&'static str, Vec<u8>)]) -> RebuildRows {
        let mut rows: Vec<(String, Value)> = Vec::new();
        for (_, body) in published {
            let envelope: Value = serde_json::from_slice(body).expect("envelope");
            let id = envelope["entity_id"]
                .as_str()
                .expect("entity id")
                .to_string();
            rows.retain(|(existing, _)| *existing != id);
            rows.push((id, envelope));
        }
        rebuild_rows_from_envelopes(rows.into_iter().map(|(_, envelope)| envelope).collect())
    }

    fn context_state(label: &str) -> ContextState {
        ContextState {
            context_id: format!("context:{label}"),
            label: label.to_string(),
            weight: 1.0,
            weight_avg_cumulative: 1.0,
            weight_avg_ema: 1.0,
            weight_samples: 1,
            tags: vec![label.to_string()],
            ilk_weights: BTreeMap::new(),
            ilk_profile: BTreeMap::new(),
            opened_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            status: "open".to_string(),
        }
    }

    /// A thread after one turn on `tags` at `seq`, with a memory summary of a fifth of the
    /// jsr-memory region: four such threads fill it, the JSON framing leaves no room for a fifth.
    async fn thread_of_a_fifth_of_the_region(seq: u64, tags: &[&str]) -> ThreadCognitionState {
        let mut thread = ThreadCognitionState::default();
        live_turn(&mut thread, seq, tags, &["inform"], &[]).await;
        for memory in thread.memories.values_mut() {
            memory.summary = "x".repeat(MEMORY_MAX_DATA_SIZE / 5);
        }
        thread
    }

    fn test_control_state() -> CognitionControlState {
        CognitionControlState {
            schema_version: COGNITION_CONFIG_SCHEMA_VERSION,
            config_version: 1,
            ai_secret_source: CognitionAiSecretSource::Missing,
            thresholds: CognitionThresholds::default(),
            semantic_tagger: CognitionSemanticTaggerConfig::default(),
        }
    }

    fn test_runtime_paths() -> RuntimePaths {
        RuntimePaths {
            state_dir: PathBuf::from("/var/lib/fluxbee/test"),
            shm_dir: PathBuf::from("/var/lib/fluxbee/test/shm"),
            cache_dir: PathBuf::from("/var/lib/fluxbee/test/cache"),
            memory_lance_path: PathBuf::from("/var/lib/fluxbee/test/memory.lance"),
        }
    }

    fn config_set_message(config: Value) -> Message {
        Message {
            routing: Routing {
                src: "SY.admin@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.cognition@motherbee".to_string()),
                ttl: 1,
                trace_id: "trace:test".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some("CONFIG_SET".to_string()),
                ..Meta::default()
            },
            payload: json!({ "node_name": "SY.cognition@motherbee", "config": config }),
        }
    }

    /// A restart (durable rows, then rebuild) must leave state the next live turn continues.
    /// Keyed by entity id, the rebuild made every lookup of that turn miss: it re-created
    /// each context, reason, co-occurrence, memory and episode from zero, under the same id,
    /// next to the rebuilt one.
    #[tokio::test]
    async fn rebuild_keys_state_like_the_live_path() {
        let turn = (
            &["billing"][..],
            &["resolve", "challenge"][..],
            &["frustration"][..],
        );
        let mut reference = ThreadCognitionState::default();
        let mut published = Vec::new();
        for seq in 1..=3 {
            published.extend(live_turn(&mut reference, seq, turn.0, turn.1, turn.2).await);
        }

        let mut rebuilt = build_communication_state(&key(TEST_THREAD), durable_rows(&published));
        let scope = rebuilt.active_scope.clone().expect("open scope rebuilt");
        assert_eq!(
            Some(&scope.scope_id),
            reference.active_scope.as_ref().map(|open| &open.scope_id)
        );
        assert!(rebuilt.contexts.contains_key(&scope.dominant_context_label));
        assert!(rebuilt.reasons.contains_key(&scope.dominant_reason_label));
        assert!(rebuilt.memories.contains_key(&scope.scope_id));
        assert!(!rebuilt.episodes.is_empty());

        live_turn(&mut reference, 4, turn.0, turn.1, turn.2).await;
        live_turn(&mut rebuilt, 4, turn.0, turn.1, turn.2).await;

        assert_eq!(
            sorted_keys(&rebuilt.contexts),
            sorted_keys(&reference.contexts)
        );
        assert_eq!(
            sorted_keys(&rebuilt.reasons),
            sorted_keys(&reference.reasons)
        );
        assert_eq!(
            sorted_keys(&rebuilt.cooccurrences),
            sorted_keys(&reference.cooccurrences)
        );
        assert_eq!(
            sorted_keys(&rebuilt.memories),
            sorted_keys(&reference.memories)
        );
        assert_eq!(
            sorted_keys(&rebuilt.episodes),
            sorted_keys(&reference.episodes)
        );
        // Continued, not re-created: four samples and the same accumulated weight.
        let billing = &rebuilt.contexts["billing"];
        assert_eq!(billing.weight_samples, 4);
        assert!((billing.weight - reference.contexts["billing"].weight).abs() < 1e-9);
        assert_eq!(rebuilt.memories[&scope.scope_id].occurrences, 4);
        assert_eq!(
            rebuilt.active_scope.as_ref().map(|open| &open.scope_id),
            Some(&scope.scope_id)
        );
        // No entity lives twice in a map.
        let unique = |ids: Vec<&String>| ids.iter().collect::<HashSet<_>>().len() == ids.len();
        assert!(unique(
            rebuilt.contexts.values().map(|c| &c.context_id).collect()
        ));
        assert!(unique(
            rebuilt.reasons.values().map(|r| &r.reason_id).collect()
        ));
        assert!(unique(
            rebuilt
                .cooccurrences
                .values()
                .map(|c| &c.cooccurrence_id)
                .collect()
        ));
        assert!(unique(
            rebuilt.memories.values().map(|m| &m.memory_id).collect()
        ));
        assert!(unique(
            rebuilt.episodes.values().map(|e| &e.episode_id).collect()
        ));
    }

    /// After a restart the region keeps the entries of communications not loaded yet, after the
    /// ones in memory; an entry of a communication in memory is replaced, never doubled.
    #[tokio::test]
    async fn the_region_keeps_what_it_has_not_loaded() {
        let mut threads = HashMap::new();
        let mut loaded = ThreadCognitionState::default();
        live_turn(&mut loaded, 1, &["billing"], &["inform"], &[]).await;
        loaded.agent_node = "AI.sales@motherbee".to_string();
        threads.insert(key("thread:loaded"), loaded.clone());
        let entry = |thread: &str, agent: &str| MemoryShmThreadEntry {
            thread_id: thread.to_string(),
            agent_ilk: agent.to_string(),
            agent_node: "AI.sales@motherbee".to_string(),
            package: build_memory_package_for_thread(thread, &loaded),
        };
        let carried = vec![
            entry("thread:loaded", TEST_AGENT),
            entry("thread:not-loaded", TEST_AGENT),
        ];
        let hot_set = build_memory_hot_set_snapshot(&threads, None, carried).expect("hot set");
        let kept: Vec<(&str, &str)> = hot_set
            .snapshot
            .threads
            .iter()
            .map(|entry| (entry.thread_id.as_str(), entry.agent_node.as_str()))
            .collect();
        assert_eq!(
            kept,
            vec![
                ("thread:loaded", "AI.sales@motherbee"),
                ("thread:not-loaded", "AI.sales@motherbee"),
            ]
        );
    }

    /// Live state follows the rebuild's retention rule: a thread outside the jsr-memory hot
    /// set leaves local state too, so memory stays bounded by the SHM capacity.
    #[test]
    fn live_state_keeps_only_the_jsr_memory_hot_set() {
        // Five threads of a fifth of the region each cannot all fit with the JSON framing.
        let mut threads = HashMap::new();
        for index in 0..5u8 {
            let scope_id = format!("scope:{index}");
            let mut thread = ThreadCognitionState {
                last_seen_at: Some(format!("2026-01-01T00:00:0{index}Z")),
                turn_count: 1,
                ..ThreadCognitionState::default()
            };
            thread.memories.insert(
                scope_id.clone(),
                MemoryState {
                    memory_id: format!("memory:{index}"),
                    scope_id,
                    summary: "x".repeat(MEMORY_MAX_DATA_SIZE / 5),
                    weight: 1.0,
                    occurrences: 1,
                    dominant_context_id: String::new(),
                    dominant_reason_id: String::new(),
                    ilk_weights: BTreeMap::new(),
                    created_at: String::new(),
                    last_seen_at: String::new(),
                },
            );
            threads.insert(key(&format!("thread:{index}")), thread);
        }

        let hot_set = retain_memory_hot_set(&mut threads, None, Vec::new()).expect("hot set");
        assert_eq!(hot_set.stats.pruned_threads_total, 1);
        assert_eq!(threads.len() as u64, hot_set.stats.selected_threads_total);
        // The oldest thread goes.
        assert!(!threads.contains_key(&key("thread:0")));
        assert!(threads
            .keys()
            .all(|thread_id| hot_set.selected_thread_ids.contains(thread_id)));
    }

    /// With the region full, recency ranks ahead of live entities: a new thread with one
    /// topic stays and the oldest goes, however much more state the older threads carry.
    /// Ranked by live entities first, a new thread was dropped right after its own turn and
    /// started over, with a new scope written to storage, on every turn.
    #[tokio::test]
    async fn a_full_region_keeps_the_newest_thread() {
        let topics = ["billing", "refund", "invoice", "charge", "card", "bank"];
        let mut threads = HashMap::new();
        for seq in 1..=4u64 {
            threads.insert(
                key(&format!("thread:old:{seq}")),
                thread_of_a_fifth_of_the_region(seq, &topics).await,
            );
        }
        let full = retain_memory_hot_set(&mut threads, None, Vec::new()).expect("hot set");
        assert_eq!(full.stats.pruned_threads_total, 0);

        threads.insert(
            key("thread:new"),
            thread_of_a_fifth_of_the_region(5, &["shipping"]).await,
        );
        assert!(
            thread_live_entities_total(&threads[&key("thread:new")])
                < thread_live_entities_total(&threads[&key("thread:old:1")])
        );
        // No turn thread: the ranking alone keeps it, as after a reload.
        let hot_set = retain_memory_hot_set(&mut threads, None, Vec::new()).expect("hot set");
        assert_eq!(hot_set.stats.pruned_threads_total, 1);
        assert!(threads.contains_key(&key("thread:new")));
        assert!(!threads.contains_key(&key("thread:old:1")));
    }

    /// The thread the current turn updated is never the one dropped, even when it does not
    /// rank as the most recent: here its turn is dated before the others (the clock stepped
    /// back).
    #[tokio::test]
    async fn the_thread_of_the_turn_is_never_dropped() {
        let mut threads = HashMap::new();
        for seq in 2..=5u64 {
            threads.insert(
                key(&format!("thread:{seq}")),
                thread_of_a_fifth_of_the_region(seq, &["billing"]).await,
            );
        }
        threads.insert(
            key("thread:turn"),
            thread_of_a_fifth_of_the_region(1, &["shipping"]).await,
        );
        let hot_set = retain_memory_hot_set(&mut threads, Some(&key("thread:turn")), Vec::new())
            .expect("hot set");
        assert_eq!(hot_set.stats.pruned_threads_total, 1);
        assert!(threads.contains_key(&key("thread:turn")));
        assert!(!threads.contains_key(&key("thread:2")));
    }

    /// §3.2/§7.2/§8.3: a lasting change of topic and drive cuts the scope once the shift is
    /// sustained: on the fifth turn after the dominant pair changes, which for a scope two to
    /// five turns old is the 6th divergent message (a scope one turn old cuts on the 5th, a
    /// long-reinforced one on the 7th). The scope used to adopt the new topic on the first
    /// divergent turn, which reset the unbind streak, so it was renamed instead of cut.
    #[tokio::test]
    async fn a_sustained_topic_change_cuts_the_scope() {
        // (turns on the first topic, the divergent message that cuts)
        for (history, cut_on) in [(1u64, 5u64), (3, 6), (12, 7)] {
            let mut thread = ThreadCognitionState::default();
            for seq in 1..=history {
                live_turn(&mut thread, seq, &["billing"], &["inform"], &[]).await;
            }
            let first = thread.active_scope.clone().expect("scope opened");
            assert_eq!(first.dominant_context_label, "billing");

            let mut cut = None;
            for divergent in 1..=20 {
                let seq = history + divergent;
                let out = live_turn(&mut thread, seq, &["shipping"], &["protect"], &[]).await;
                let scope = thread.active_scope.as_ref().expect("scope");
                if closed_scope_ids(&out).contains(&first.scope_id) {
                    assert_ne!(scope.scope_id, first.scope_id);
                    assert_eq!(scope.dominant_context_label, "shipping");
                    assert_eq!(scope.dominant_reason_label, "risk containment");
                    cut = Some(divergent);
                    break;
                }
                // Until the cut the scope keeps its identity and its anchor.
                assert_eq!(scope.scope_id, first.scope_id);
                assert_eq!(scope.dominant_context_label, "billing");
                assert_eq!(scope.dominant_reason_label, "information exchange");
            }
            assert_eq!(cut, Some(cut_on), "turns on the first topic: {history}");
        }
    }

    /// Tags told together tie on weight. The scope's own context keeps a tie, so a tag
    /// reinforced with it never poses as a topic change, however the map is laid out.
    #[tokio::test]
    async fn co_reinforced_tags_keep_the_scope() {
        let tags = [
            "billing",
            "refund",
            "invoice",
            "charge",
            "card",
            "bank",
            "statement",
            "fee",
            "receipt",
            "account",
        ];
        let mut thread = ThreadCognitionState::default();
        live_turn(&mut thread, 1, &tags[..2], &["inform"], &[]).await;
        let first = thread.active_scope.clone().expect("scope opened");
        for seq in 2..=12u64 {
            // The same two tags every turn, plus new ones that keep growing the map.
            let told = &tags[..(seq as usize).min(tags.len())];
            let out = live_turn(&mut thread, seq, told, &["inform"], &[]).await;
            assert!(closed_scope_ids(&out).is_empty());
            let scope = thread.active_scope.as_ref().expect("scope");
            assert_eq!(scope.scope_id, first.scope_id);
            assert_eq!(scope.dominant_context_label, first.dominant_context_label);
        }
    }

    /// What ships for an episode: the gate's affect, title, intensity and evidence, and the
    /// summarizer's summary and reason. The summarizer must return both whenever there is a
    /// candidate, so there is no deterministic text behind them.
    #[tokio::test]
    async fn an_episode_ships_the_gate_fields_and_the_narrative_text() {
        let mut thread = ThreadCognitionState::default();
        let out = live_turn(
            &mut thread,
            1,
            &["billing"],
            &["resolve", "challenge"],
            &["frustration"],
        )
        .await;
        let episodes = envelopes_of(&out, SUBJECT_STORAGE_COGNITION_EPISODES);
        assert_eq!(episodes.len(), 1);
        let episode: CognitionEpisodeData =
            serde_json::from_value(episodes[0].data.clone()).expect("episode data");
        assert_eq!(episode.affect_id, "anger");
        assert_eq!(episode.title, "Friction around billing");
        assert_eq!(episode.summary, "AI episode summary (anger).");
        assert_eq!(
            episode.reason.as_deref(),
            Some("AI episode reason (anger).")
        );
        assert_eq!(episode.base_intensity, Some(8.0));
        assert_eq!(episode.evidence_strength, Some(9.0));
        assert!(episode
            .evidence_signals
            .contains(&"frustration".to_string()));
    }

    #[test]
    fn the_episode_gate_needs_its_signal_combination() {
        let context = context_state("billing");
        let tagger = |canonical: &[&str], extra: &[&str]| SemanticTaggerOutput {
            tags: strings(&["billing"]),
            reason_signals_canonical: strings(canonical),
            reason_signals_extra: strings(extra),
        };
        assert!(
            build_episode_candidate(&tagger(&["inform"], &["frustration"]), &context).is_none()
        );
        assert!(
            build_episode_candidate(&tagger(&["resolve", "challenge"], &[]), &context).is_none()
        );
        let candidate =
            build_episode_candidate(&tagger(&["resolve", "request"], &["urgency"]), &context)
                .expect("urgency candidate");
        assert_eq!(candidate.affect_id, "urgency");
        assert_eq!(candidate.title, "Urgent push on billing");
        assert_eq!(candidate.base_intensity, 7.0);
        assert_eq!(candidate.evidence_strength, 8.0);
    }

    /// A close threshold is the weight under which an open entity closes, taken as set; a
    /// co-occurrence closes under the average of the two. Counted in turns that leave a
    /// one-turn topic unreinforced, from weight 1.0 decaying by 0.85 (context), 0.75 (reason)
    /// and 0.80 (co-occurrence). The defaults close where `*_open` at 0.5, halved, did.
    #[tokio::test]
    async fn a_close_threshold_applies_as_set() {
        async fn misses_until_closed(thresholds: CognitionThresholds) -> [Option<u64>; 3] {
            let mut thread = ThreadCognitionState::default();
            live_turn_with(&thresholds, &mut thread, 1, &["billing"], &["inform"], &[]).await;
            let mut closed = [None; 3];
            for miss in 1..=20u64 {
                live_turn_with(
                    &thresholds,
                    &mut thread,
                    1 + miss,
                    &["shipping"],
                    &["protect"],
                    &[],
                )
                .await;
                let statuses = [
                    &thread.contexts["billing"].status,
                    &thread.reasons["information exchange"].status,
                    &thread.cooccurrences["billing|information exchange"].status,
                ];
                for (slot, status) in closed.iter_mut().zip(statuses) {
                    if slot.is_none() && status == "closed" {
                        *slot = Some(miss);
                    }
                }
            }
            closed
        }

        // 0.85 < 0.9; 0.5625 >= 0.5 > 0.4219; 0.8 >= (0.9 + 0.5) / 2 > 0.64.
        let set = CognitionThresholds {
            context_close: 0.9,
            reason_close: 0.5,
        };
        assert_eq!(misses_until_closed(set).await, [Some(1), Some(3), Some(2)]);
        // 0.85^9, 0.75^5 and 0.8^7 are the first powers under 0.25.
        assert_eq!(
            misses_until_closed(CognitionThresholds::default()).await,
            [Some(9), Some(5), Some(7)]
        );
    }

    /// The thresholds are named for what they do. CONFIG_GET shows `context_close` and
    /// `reason_close`; CONFIG_SET refuses the retired `*_open` keys by name, pointing at the
    /// close key, and takes the close keys as set (a key left out takes its default).
    #[test]
    fn the_thresholds_are_named_for_what_they_do() {
        let payload = build_cognition_config_get_payload(
            "SY.cognition@motherbee",
            &test_control_state(),
            &CognitionRuntimeState::default(),
            &test_runtime_paths(),
            None,
        );
        assert_eq!(
            payload["config"]["thresholds"],
            json!({ "context_close": 0.25, "reason_close": 0.25 })
        );
        let optional_fields = payload["contract"]["optional_fields"].to_string();
        assert!(optional_fields.contains("config.thresholds.context_close"));
        assert!(optional_fields.contains("config.thresholds.reason_close"));
        assert!(!optional_fields.contains("_open"));

        for (retired, current) in [
            ("context_open", "context_close"),
            ("reason_open", "reason_close"),
        ] {
            let mut control = test_control_state();
            let response = apply_cognition_config_set(
                &config_set_message(json!({ "thresholds": { retired: 0.5 } })),
                "SY.cognition@motherbee",
                &mut control,
                &test_runtime_paths(),
            )
            .expect("CONFIG_SET answers");
            assert_eq!(response["ok"], json!(false));
            assert_eq!(response["error"]["code"], json!("invalid_config"));
            let message = response["error"]["message"].as_str().expect("message");
            assert!(
                message.contains(&format!("config.thresholds.{retired} ")),
                "{message}"
            );
            assert!(
                message.contains(&format!("config.thresholds.{current}")),
                "{message}"
            );
            assert_eq!(control.config_version, 1, "nothing applied");
        }

        let thresholds = extract_cognition_thresholds(&json!({
            "config": { "thresholds": { "context_close": 0.4 } }
        }))
        .expect("valid")
        .expect("present");
        assert_eq!(thresholds.context_close, 0.4);
        assert_eq!(
            thresholds.reason_close,
            COGNITION_DEFAULT_REASON_CLOSE_THRESHOLD
        );
    }

    /// CONFIG_GET names where the turns come from and where its state is read: the router stream
    /// and SY.storage over the mesh, with no postgres resource of its own (§6.2).
    #[test]
    fn config_get_names_the_stream_and_the_mesh() {
        let payload = build_cognition_config_get_payload(
            "SY.cognition@motherbee",
            &test_control_state(),
            &CognitionRuntimeState::default(),
            &test_runtime_paths(),
            None,
        );
        assert_eq!(
            payload["config"]["turns"]["subject"],
            json!("cognition.turns")
        );
        assert_eq!(
            payload["config"]["storage"]["write_subject"],
            json!("storage.cognition.batch")
        );
        let resources = payload["contract"]["resources"].to_string();
        assert!(!resources.contains("postgres"), "{resources}");
    }

    #[test]
    fn a_turns_envelopes_pack_under_the_append_limit() {
        let envelope =
            |n: usize| json!({ "entity_id": format!("context:{n}"), "pad": "x".repeat(900) });
        let packs = pack_envelopes((0..90).map(envelope).collect(), COGNITION_APPEND_MAX_BYTES);
        assert!(packs.len() >= 2);
        assert_eq!(packs.iter().map(|p| p.envelopes.len()).sum::<usize>(), 90);
        for pack in &packs {
            assert!(serde_json::to_vec(pack).unwrap().len() <= COGNITION_APPEND_MAX_BYTES + 64);
        }
        // An envelope alone past the limit still goes, alone: the stream counts it oversize.
        let big = vec![json!({ "pad": "x".repeat(COGNITION_APPEND_MAX_BYTES * 2) })];
        assert_eq!(pack_envelopes(big, COGNITION_APPEND_MAX_BYTES).len(), 1);
    }

    #[tokio::test]
    async fn a_communications_references_carry_its_agent() {
        let mut thread = ThreadCognitionState::default();
        let out = live_turn(&mut thread, 1, &["billing"], &["inform"], &[]).await;
        let contexts = envelopes_of(&out, SUBJECT_STORAGE_COGNITION_CONTEXTS);
        let context = &contexts[0];
        assert_eq!(context.agent_ilk.as_deref(), Some(TEST_AGENT));
        assert_eq!(context.turn, Some(1));
        assert_eq!(
            context.entity_id,
            stable_entity_id("context", &[TEST_THREAD, TEST_AGENT, "billing"])
        );
        let threads = envelopes_of(&out, SUBJECT_STORAGE_COGNITION_THREADS);
        assert_eq!(
            threads[0].entity_id,
            stable_entity_id("thread", &[TEST_THREAD, TEST_AGENT])
        );
        // Both interlocutors weigh the same.
        assert_eq!(
            current_turn_ilk_weights(Some("ilk:agent"), Some("ilk:juan")),
            BTreeMap::from([
                ("ilk:agent".to_string(), 1.0),
                ("ilk:juan".to_string(), 1.0)
            ])
        );
    }
}
