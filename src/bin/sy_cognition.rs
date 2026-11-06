// STATUS's `turns_consumer` is one `json!` object past the macro's default depth.
#![recursion_limit = "256"]

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
use fluxbee_sdk::message_trace;
use fluxbee_sdk::message_writer::{self, IlkFacts};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, EpisodeSummary, MemoryContextSummary, MemoryPackage,
    MemoryPackageTruncated, MemoryReasonSummary, MemorySummary, MemoryTurn, MemoryTurnChoice,
    MemoryTurnDirection, MemoryTurnParty, Message, Meta, Routing, VaultSecretChangedPayload,
    VaultSecretInterest, MEMORY_PACKAGE_VERSION, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::stream::{
    is_stream_node_name, stream_hive, StreamBatch, MSG_STREAM_ACK, MSG_STREAM_BATCH,
    SUBJECT_COGNITION_TURNS,
};
use fluxbee_sdk::{
    build_node_config_response_message, managed_node_config_path, managed_node_instance_dir,
    managed_node_name, try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, VaultCallerOwned,
    VaultClient, NODE_CONFIG_APPLY_MODE_REPLACE,
};
use json_router::shm::{
    memory_shm_name_for_hive, MemoryRegionWriter, MemoryShmSnapshot, MemoryShmThreadEntry,
    MEMORY_MAX_DATA_SIZE,
};

#[path = "sy_cognition/narrative_summarizer_ai.rs"]
mod narrative_summarizer_ai;
#[path = "sy_cognition/semantic_tagger_ai.rs"]
mod semantic_tagger_ai;
#[path = "sy_cognition/state_store.rs"]
mod state_store;

use narrative_summarizer_ai::{
    run_narrative_summarizer_ai, EpisodeNarrative, NarrativeSummaries, NarrativeSummarizerAiInput,
};
use semantic_tagger_ai::run_semantic_tagger_ai;
use state_store::CognitionStore;

type CognitionError = Box<dyn std::error::Error + Send + Sync>;

const COGNITION_NODE_BASE_NAME: &str = "SY.cognition";
const COGNITION_NODE_VERSION: &str = "2.0";
const COGNITION_CONFIG_SCHEMA_VERSION: u32 = 1;
/// How long an agent node's ILK and tenant are trusted when the identity shared memory's sequence
/// cannot be read; while it can, they are trusted until it changes.
const COGNITION_AGENT_ILK_TTL: Duration = Duration::from_secs(60);
/// How many of its last turns a communication keeps, oldest first: the 21st pushes out the oldest.
const COGNITION_RECENT_TURNS: usize = 20;
/// The most of a turn's text kept, in characters.
const COGNITION_TURN_TEXT_MAX_CHARS: usize = 2_000;
/// The most characters a turn keeps of its sender's ILK and of the node it came through: the
/// message names both, so they are bounded like its text.
const COGNITION_TURN_NAME_MAX_CHARS: usize = 256;
/// The most bytes a communication's turns take, serialized; past it the oldest go first. Twenty
/// turns of 2,000 characters of Latin text fit; text that takes more bytes a character keeps fewer.
/// It keeps a package and the message it rides on under a frame (128 KiB, of which an inline text
/// message takes at most 64 KiB).
const COGNITION_RECENT_TURNS_MAX_BYTES: usize = 48 * 1024;
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
/// Each feeling's inertia (docs/cognition-hands-v1.md §10.3): how many times a context's half-life
/// its own is. The operator set two to ten times by feeling (2026-11-05); the lab's numbers are
/// checked against its code when it is found.
const COGNITION_FEELING_INERTIA: [(&str, f64); 3] =
    [("urgency", 2.0), ("anger", 5.0), ("escalation", 10.0)];
/// A feeling closes under this weight, as a context does by default.
const COGNITION_FEELING_CLOSE_THRESHOLD: f64 = 0.25;
/// How many open feelings a package carries, the heaviest first.
const COGNITION_PACKAGE_MAX_FEELINGS: usize = 12;
/// The most characters of a feeling's text a package carries.
const COGNITION_FEELING_TEXT_MAX_CHARS: usize = 300;
const COGNITION_MAX_TAGS: usize = 12;
const COGNITION_MAX_REASON_SIGNALS: usize = 4;
/// The affects a turn can show its writer feels: the episode gate's words (`build_episode_candidate`).
/// The tagger names them in a closed set of their own, `affect_signals` (FINDINGS A-157); its free
/// extra hints never named them.
const COGNITION_AFFECT_SIGNALS: [&str; 3] = ["frustration", "escalation", "urgency"];
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
    /// The writer's own affect, from `COGNITION_AFFECT_SIGNALS` (FINDINGS A-157).
    affect_signals: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct NarrativeOutcome {
    called: bool,
    failed: bool,
    invalid_output: bool,
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
    /// Its own file (docs/cognition-hands-v1.md §11.3).
    store_path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize)]
struct CognitionRuntimeState {
    processed_turns_total: u64,
    invalid_turns_total: u64,
    /// Turns dropped because the queue between their delivery and their processing was full.
    dropped_turns_queue_full_total: u64,
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
    /// The communications loaded from the file at start (docs/cognition-hands-v1.md §11.3).
    loaded_at_start_total: u64,
    /// Turns whose agent node has no ILK in the identity shared memory: no memory for them.
    no_agent_ilk_total: u64,
    /// Turns the intake added to their communication's last turns, ahead of the worker.
    intake_turns_total: u64,
    /// Turns the worker did not analyze: no person at their other end (docs/cognition-hands-v1.md
    /// §6). They stay among their communication's last turns.
    turns_not_analyzed_total: u64,
    /// Records of a stream batch at or below the last seq taken for that stream and epoch.
    stream_repeats_skipped_total: u64,
    /// Communications loaded from the file after start, and the loads that failed: a turn whose
    /// communication could not load is skipped.
    store_loads_total: u64,
    store_load_failures_total: u64,
    /// Communications written to the file, and the writes that failed.
    store_writes_total: u64,
    store_write_failures_total: u64,
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
    /// The communications' state, this node's alone (docs/cognition-hands-v1.md §11.3).
    store: CognitionStore,
    control_state: Arc<Mutex<CognitionControlState>>,
    runtime_state: Arc<Mutex<CognitionRuntimeState>>,
    /// The communications this cognition holds in memory, each owned by one of its hive's agents
    /// (§6.1): those of the memory region, out of the file. The intake and the worker both write
    /// it, and the region is built from it: whoever writes the region takes `memory_region` first,
    /// then this (`sync_held_state`).
    thread_states: Arc<Mutex<HashMap<CommunicationKey, ThreadCognitionState>>>,
    memory_region: Arc<Mutex<Option<MemoryRegionWriter>>>,
    /// The communication of the turn the worker is on, which every region write keeps: an intake
    /// batch must not drop it while its model calls run.
    worker_turn: Mutex<Option<CommunicationKey>>,
    /// Communications a failed write left only in memory: the next write takes them again, and
    /// until then they stay held (`sync_held_state`).
    unsaved: std::sync::Mutex<HashSet<CommunicationKey>>,
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

/// A communication as this cognition holds it and writes it to its file: online state, so an item
/// that closes leaves it (docs/cognition-hands-v1.md §11.2). The intake owns its last turns and the
/// last turn's ILKs and channel; the worker owns the rest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ThreadCognitionState {
    /// The agent node its router delivers to, where the memory package is read (mechanism only).
    agent_node: String,
    first_seen_at: Option<String>,
    last_seen_at: Option<String>,
    turn_count: u64,
    /// The last turn's sender and receiver ILKs and channel.
    src_ilk: Option<String>,
    dst_ilk: Option<String>,
    ich: Option<String>,
    contexts: HashMap<String, ContextState>,
    reasons: HashMap<String, ReasonState>,
    cooccurrences: HashMap<String, CooccurrenceState>,
    active_scope: Option<ScopeBindingState>,
    memories: HashMap<String, MemoryState>,
    episodes: HashMap<String, EpisodeState>,
    /// Its last turns, oldest first (`push_recent_turn`): what its agent received and sent.
    turns: Vec<MemoryTurn>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ContextState {
    context_id: String,
    label: String,
    weight: f64,
    weight_avg_cumulative: f64,
    weight_avg_ema: f64,
    weight_samples: u64,
    tags: Vec<String>,
    ilk_weights: BTreeMap<String, f64>,
    ilk_profile: BTreeMap<String, IlkProfile>,
    opened_at: String,
    last_seen_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
    ilk_profile: BTreeMap<String, IlkProfile>,
    opened_at: String,
    last_seen_at: String,
}

/// How often an ILK sent and got the turns that named an item.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
struct IlkProfile {
    as_sender: u64,
    as_receiver: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ScopeBindingState {
    scope_id: String,
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
    unbind_streak: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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

/// A person's feeling (docs/cognition-hands-v1.md §10.3): one per owner, affect and context. Each
/// of its owner's turns that shows it makes its weight `weight × decay + step`, and each of the
/// owner's other turns `weight × decay`, as a context's, with the more inertia of its affect. It
/// closes under `COGNITION_FEELING_CLOSE_THRESHOLD` and leaves the state; one that comes back
/// starts again (§11.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct EpisodeState {
    episode_id: String,
    /// The person whose turns show it.
    owner: MemoryTurnParty,
    affect_id: String,
    context_id: String,
    context_label: String,
    /// The scope it was last felt in.
    scope_id: String,
    title: String,
    summary: String,
    reason: String,
    base_intensity: f64,
    evidence_strength: f64,
    evidence_context_ids: Vec<String>,
    evidence_reason_ids: Vec<String>,
    evidence_signals: Vec<String>,
    weight: f64,
    weight_avg_cumulative: f64,
    weight_avg_ema: f64,
    weight_samples: u64,
    occurrences: u64,
    created_at: String,
    last_felt_at: String,
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

/// What a person's turn shows (docs/cognition-hands-v1.md §10.3): the gate's affect, felt around
/// the active scope's dominant context and reason.
#[derive(Debug, Clone)]
struct FeelingCandidate {
    episode: EpisodeCandidate,
    scope_id: String,
    context_id: String,
    context_label: String,
    reason_id: String,
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
    just_updated: bool,
    has_active_scope: bool,
    live_entities_total: usize,
    last_seen_epoch_ms: i64,
    turn_count: u64,
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
    // Its own file, before anything else is written to its folder (docs/cognition-hands-v1.md
    // §11.3). Without it the node does not start: the file is where its state lives.
    let store = CognitionStore::open(&runtime_paths.state_dir)
        .map_err(|err| format!("sy.cognition could not open its file: {err}"))?;
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
        store,
        control_state: Arc::clone(&control_state),
        runtime_state: Arc::clone(&runtime_state),
        thread_states: Arc::new(Mutex::new(HashMap::new())),
        memory_region: Arc::new(Mutex::new(None)),
        worker_turn: Mutex::new(None),
        unsaved: std::sync::Mutex::new(HashSet::new()),
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
    // Before the first batch: the region gets the most recent communications from the file, so a
    // restart or a host reboot neither blanks the packages the agents read nor starts them over.
    load_most_recent(&app_state).await;

    // The worker's task first resolves the AI key, then takes turns. Meanwhile the main loop keeps
    // taking its routers' batches, their last turns go to the file and the region at once, and the
    // turns wait in the queue, inside the streams' window (§6.2).
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

/// At start, the most recent communications come back from the file, twice the region's bytes of
/// state at most, and the region is written from them: what does not fit it is let go again
/// (`retain_memory_hot_set`). Any other comes back at its next turn.
async fn load_most_recent(app_state: &CognitionAppState) {
    let loaded = match app_state.store.most_recent(2 * MEMORY_MAX_DATA_SIZE) {
        Ok(loaded) => loaded,
        Err(err) => {
            tracing::warn!(error = %err, "sy.cognition could not read its file at start");
            return;
        }
    };
    let count = loaded.len() as u64;
    if let Err(err) = sync_held_state(app_state, &[], |threads| {
        threads.extend(loaded);
        Vec::new()
    })
    .await
    {
        tracing::warn!(error = %err, "sy.cognition failed to write jsr-memory at start");
    }
    let mut state = app_state.runtime_state.lock().await;
    state.loaded_at_start_total = count;
    tracing::info!(
        loaded = count,
        hot = state.shm_hot_threads_total,
        store = %app_state.store.path().display(),
        "sy.cognition loaded its most recent communications from its file"
    );
}

/// Takes the turns its router copied for it (`cognition.turns`, §6.2), in order, one at a time.
async fn run_turn_worker(mut rx: mpsc::Receiver<CognitionTurn>, app_state: Arc<CognitionAppState>) {
    while let Some(turn) = rx.recv().await {
        handle_turn(turn, Arc::clone(&app_state)).await;
        *app_state.worker_turn.lock().await = None;
    }
}

/// A `STREAM_BATCH` from one of its hive's routers. Its new turns go first to their communications'
/// last turns, which the file and the memory region get at once (`take_recent_turns`), then each
/// to the worker's queue; the batch is acked without waiting for the worker (best effort, §2.7). A
/// record at or below the last one taken from that stream and epoch is a repeat; a full queue drops
/// a turn and counts it.
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
    let mut turns = Vec::new();
    let mut recent = Vec::new();
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
        recent.extend(intake_turn(app_state, &message, &record.producer, record.ts_ms).await);
        turns.push(CognitionTurn {
            message,
            agent_node: record.producer,
        });
    }
    // The next message its router delivers to the agent carries these turns, whatever the
    // worker's backlog.
    take_recent_turns(app_state, recent).await;
    for turn in turns {
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

/// A turn as the intake takes it (docs/reference/cognition.md §3.4): the communication it belongs
/// to, the last turn's ILKs and channel its state keeps, and the turn as the agent's package shows
/// it.
#[derive(Debug, Clone)]
struct RecentTurn {
    key: CommunicationKey,
    agent_node: String,
    src_ilk: Option<String>,
    dst_ilk: Option<String>,
    ich: Option<String>,
    turn: MemoryTurn,
}

/// What the intake keeps of a copy of a turn: `None` without a thread id or without an ILK for its
/// agent node in the identity shared memory, as the worker finds it (`agent_identity_of`, the
/// worker counts both), and without text or files.
async fn intake_turn(
    app_state: &CognitionAppState,
    message: &Message,
    agent_node: &str,
    ts_ms: u64,
) -> Option<RecentTurn> {
    message
        .meta
        .thread_id
        .as_deref()
        .filter(|thread_id| !thread_id.trim().is_empty())?;
    let agent = agent_identity_of(app_state, agent_node).await?;
    recent_turn_of(message, agent_node, &agent.ilk, ts_ms, |ilk| {
        message_writer::ilk_facts(&app_state.hive_id, ilk)
    })
}

/// A copy of a turn as its communication keeps it. The agent sent it (`out`) when its router names
/// the agent node as the sender; otherwise the agent received it (`in`), through `turn_via`. A turn
/// the agent got names its writer, and one it sent whom it answered (`turn_party`, with what `facts`
/// says of each ILK). An answer that chose from a WF's menu keeps its choice (`menu_choice`). A
/// turn with neither text, files nor a choice is left out: the package would show it empty.
fn recent_turn_of(
    message: &Message,
    agent_node: &str,
    agent_ilk: &str,
    ts_ms: u64,
    facts: impl Fn(&str) -> Option<IlkFacts>,
) -> Option<RecentTurn> {
    let thread_id = message
        .meta
        .thread_id
        .clone()
        .filter(|thread_id| !thread_id.trim().is_empty())?;
    let direction = if message.routing.src_l2_name.as_deref() == Some(agent_node) {
        MemoryTurnDirection::Out
    } else {
        MemoryTurnDirection::In
    };
    let text = extract_turn_text(&message.payload)
        .map(|text| turn_field(&text, COGNITION_TURN_TEXT_MAX_CHARS))
        .unwrap_or_default();
    let attachments = turn_attachments(&message.payload);
    let choice = match direction {
        MemoryTurnDirection::Out => menu_choice(message.meta.context.as_ref()),
        MemoryTurnDirection::In => None,
    };
    if text.is_empty() && attachments == 0 && choice.is_none() {
        return None;
    }
    let turn = MemoryTurn {
        direction,
        src_ilk: message
            .meta
            .src_ilk
            .as_deref()
            .map(str::trim)
            .filter(|ilk| !ilk.is_empty())
            .map(|ilk| turn_field(ilk, COGNITION_TURN_NAME_MAX_CHARS)),
        via: match direction {
            MemoryTurnDirection::In => turn_via(message),
            MemoryTurnDirection::Out => None,
        },
        writer: match direction {
            MemoryTurnDirection::In => turn_writer(message, &facts),
            MemoryTurnDirection::Out => None,
        },
        to: match direction {
            MemoryTurnDirection::In => None,
            MemoryTurnDirection::Out => turn_party(message.meta.dst_ilk.as_deref(), None, &facts),
        },
        text,
        attachments,
        choice,
        ts: turn_ts(ts_ms),
    };
    Some(RecentTurn {
        key: CommunicationKey {
            thread_id,
            agent_ilk: agent_ilk.to_string(),
        },
        agent_node: agent_node.to_string(),
        src_ilk: message.meta.src_ilk.clone(),
        dst_ilk: message.meta.dst_ilk.clone(),
        ich: message.meta.ich.clone(),
        turn,
    })
}

/// Who wrote a turn the agent got (docs/cognition-hands-v1.md §6): its `meta.context.writer`, else
/// its `src_ilk`, which stood for its writer before messages named one.
fn turn_writer(
    message: &Message,
    facts: &impl Fn(&str) -> Option<IlkFacts>,
) -> Option<MemoryTurnParty> {
    match message_writer::writer_of(message.meta.context.as_ref()) {
        Some(writer) => turn_party(writer.ilk.as_deref(), writer.name.as_deref(), facts),
        None => turn_party(message.meta.src_ilk.as_deref(), None, facts),
    }
}

/// A party of a turn: its ILK, its name (the message's, else the identity's), and its kind as the
/// identity says (`IlkFacts::kind`); none with neither an ILK nor a name. Each within the turn's caps.
fn turn_party(
    ilk: Option<&str>,
    name: Option<&str>,
    facts: &impl Fn(&str) -> Option<IlkFacts>,
) -> Option<MemoryTurnParty> {
    let present = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    };
    let ilk = present(ilk);
    let known = ilk.as_deref().and_then(facts);
    let name = present(name).or_else(|| known.as_ref().and_then(|known| known.name.clone()));
    if ilk.is_none() && name.is_none() {
        return None;
    }
    Some(MemoryTurnParty {
        ilk: ilk.map(|ilk| turn_field(&ilk, COGNITION_TURN_NAME_MAX_CHARS)),
        name: name.map(|name| turn_field(&name, COGNITION_TURN_NAME_MAX_CHARS)),
        kind: known
            .as_ref()
            .and_then(IlkFacts::kind)
            .map(ToString::to_string),
    })
}

/// Whether a turn has a person at its other end: the turns cognition analyzes
/// (docs/cognition-hands-v1.md §6). A turn the agent got has one unless its writer is known not to be
/// a person: an ILK the identity names `agent` or `system`, or a writer with no ILK, a node. An
/// answer the agent sent has one unless its `dst_ilk` is known not to be a person. An ILK the
/// identity does not know yet is taken as a person's: a person's first message may come before the
/// replica has their ILK, and a turn analyzed for nothing costs less than a person's turn lost.
fn person_at_other_end(
    message: &Message,
    agent_node: &str,
    facts: impl Fn(&str) -> Option<IlkFacts>,
) -> bool {
    let known_not_a_person = |ilk: &str| {
        facts(ilk.trim()).is_some_and(|facts| !facts.is_person() && facts.kind().is_some())
    };
    if message.routing.src_l2_name.as_deref() == Some(agent_node) {
        return !message
            .meta
            .dst_ilk
            .as_deref()
            .is_some_and(known_not_a_person);
    }
    match message_writer::writer_of(message.meta.context.as_ref()) {
        Some(writer) => writer
            .ilk
            .as_deref()
            .is_some_and(|ilk| !known_not_a_person(ilk)),
        None => !message
            .meta
            .src_ilk
            .as_deref()
            .is_some_and(known_not_a_person),
    }
}

/// Whose feelings a turn moves (docs/cognition-hands-v1.md §10.3): the person who wrote a turn the
/// agent got, by their ILK. None for the agent's own answer, a writer with no ILK, or one the
/// identity knows is not a person; an ILK it does not know yet counts as a person's, as for the
/// analysis.
fn feeling_owner(
    message: &Message,
    agent_node: &str,
    facts: impl Fn(&str) -> Option<IlkFacts>,
) -> Option<MemoryTurnParty> {
    if message.routing.src_l2_name.as_deref() == Some(agent_node) {
        return None;
    }
    let writer = turn_writer(message, &facts)?;
    writer.ilk.as_ref()?;
    match writer.kind.as_deref() {
        Some("agent" | "workflow" | "system") => None,
        _ => Some(writer),
    }
}

/// Who spoke a turn, as the narrator reads it (docs/cognition-hands-v1.md §10.3): `person Ana Pérez
/// (ilk:…)` for a turn the agent got, `the agent, answering person Ana Pérez (ilk:…)` for its own.
fn narrator_speaker(
    message: &Message,
    agent_node: &str,
    facts: impl Fn(&str) -> Option<IlkFacts>,
) -> Option<String> {
    if message.routing.src_l2_name.as_deref() == Some(agent_node) {
        let to = turn_party(message.meta.dst_ilk.as_deref(), None, &facts);
        return Some(match to.as_ref().and_then(party_words) {
            Some(to) => format!("the agent, answering {to}"),
            None => "the agent".to_string(),
        });
    }
    turn_writer(message, &facts).as_ref().and_then(party_words)
}

/// A party in words: `<kind> <name> (<ILK>)`, with either of name and ILK alone, and the kind when
/// the identity gave one. None with neither a name nor an ILK.
fn party_words(party: &MemoryTurnParty) -> Option<String> {
    let named = match (party.name.as_deref(), party.ilk.as_deref()) {
        (Some(name), Some(ilk)) => format!("{name} ({ilk})"),
        (Some(name), None) => name.to_string(),
        (None, Some(ilk)) => ilk.to_string(),
        (None, None) => return None,
    };
    Some(match party.kind.as_deref() {
        Some(kind) => format!("{kind} {named}"),
        None => named,
    })
}

/// The node a turn came in through: where its cycle started, its trace's first entry
/// (`meta.context.trace`, docs/message-trace-v1.md), the IO node a WF forwards it from; otherwise
/// its sender, as its router names it.
fn turn_via(message: &Message) -> Option<String> {
    let named = |node: Option<&str>| {
        node.map(str::trim)
            .filter(|node| !node.is_empty())
            .map(|node| turn_field(node, COGNITION_TURN_NAME_MAX_CHARS))
    };
    named(message_trace::origin(message.meta.context.as_ref()).as_deref())
        .or_else(|| named(message.routing.src_l2_name.as_deref()))
}

/// What an agent chose from a WF's menu with an answer it sent: the option and its params, as its
/// `meta.context.evaluation` names them (`{"reply", "option", "params"}`), within the turn's caps.
/// A blank reply leaves no text, and the WF may not have forwarded it, so the choice says what the
/// answer did.
fn menu_choice(context: Option<&Value>) -> Option<MemoryTurnChoice> {
    let evaluation = context?.get("evaluation")?;
    let option = evaluation
        .get("option")?
        .as_str()
        .map(str::trim)
        .filter(|option| !option.is_empty())?;
    let params = evaluation
        .get("params")
        .filter(|params| params.as_object().is_some_and(|params| !params.is_empty()))
        .map(|params| turn_field(&params.to_string(), COGNITION_TURN_TEXT_MAX_CHARS))
        .unwrap_or_default();
    Some(MemoryTurnChoice {
        option: turn_field(option, COGNITION_TURN_NAME_MAX_CHARS),
        params,
    })
}

/// A value a turn keeps from its message: without U+0000, a control character no reader of a turn
/// wants, and cut at `max_chars` characters.
fn turn_field(value: &str, max_chars: usize) -> String {
    cut_chars(&value.replace('\0', ""), max_chars)
}

/// How many files a turn carried: its attachments, and its text when it came as a file.
fn turn_attachments(payload: &Value) -> u32 {
    TextV1Payload::from_value(payload).map_or(0, |payload| {
        u32::try_from(payload.attachments.len() + usize::from(payload.content_ref.is_some()))
            .unwrap_or(u32::MAX)
    })
}

/// When its router took a turn (the stream record's time), in UTC to the millisecond.
fn turn_ts(ts_ms: u64) -> String {
    i64::try_from(ts_ms)
        .ok()
        .filter(|ms| *ms > 0)
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `value` cut at `max_chars` characters, the last of them an ellipsis when it was cut.
fn cut_chars(value: &str, max_chars: usize) -> String {
    if value.chars().nth(max_chars).is_none() {
        return value.to_string();
    }
    let mut cut: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The intake's turns into what is held (docs/reference/cognition.md §3.4): each communication is
/// written to the file and the region is rewritten with them, before the turns reach the worker's
/// queue.
async fn take_recent_turns(app_state: &CognitionAppState, recent: Vec<RecentTurn>) {
    if recent.is_empty() {
        return;
    }
    let taken = recent.len() as u64;
    let mut kept: Vec<CommunicationKey> = Vec::new();
    for turn in &recent {
        if !kept.contains(&turn.key) {
            kept.push(turn.key.clone());
        }
    }
    let mut loads = StoreLoads::default();
    if let Err(err) = sync_held_state(app_state, &kept, |threads| {
        hold_recent_turns(threads, &app_state.store, recent, &mut loads)
    })
    .await
    {
        tracing::warn!(error = %err, "sy.cognition failed to sync jsr-memory at intake");
    }
    let mut state = app_state.runtime_state.lock().await;
    state.intake_turns_total = state.intake_turns_total.saturating_add(taken);
    loads.count(&mut state);
}

/// The loads of communications from the file one change made, for the runtime counters.
#[derive(Debug, Default)]
struct StoreLoads {
    loaded: u64,
    failed: u64,
}

impl StoreLoads {
    fn count(&self, state: &mut CognitionRuntimeState) {
        state.store_loads_total = state.store_loads_total.saturating_add(self.loaded);
        state.store_load_failures_total =
            state.store_load_failures_total.saturating_add(self.failed);
    }
}

/// A communication as held, or else from the file, or else new. `Err` when the file could not be
/// read: a new one would write over what the file keeps.
fn held_or_loaded<'a>(
    threads: &'a mut HashMap<CommunicationKey, ThreadCognitionState>,
    store: &CognitionStore,
    key: &CommunicationKey,
    loads: &mut StoreLoads,
) -> Result<&'a mut ThreadCognitionState, CognitionError> {
    if !threads.contains_key(key) {
        let stored = store.load(key).inspect_err(|_| loads.failed += 1)?;
        if stored.is_some() {
            loads.loaded += 1;
        }
        threads.insert(key.clone(), stored.unwrap_or_default());
    }
    Ok(threads.get_mut(key).expect("held above"))
}

/// Adds each turn to its communication's last turns, and returns the communications it touched. A
/// communication whose file could not be read keeps its turn out: the worker skips it too.
fn hold_recent_turns(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    store: &CognitionStore,
    recent: Vec<RecentTurn>,
    loads: &mut StoreLoads,
) -> Vec<CommunicationKey> {
    let mut touched: Vec<CommunicationKey> = Vec::new();
    for recent in recent {
        let held = match held_or_loaded(threads, store, &recent.key, loads) {
            Ok(held) => held,
            Err(err) => {
                tracing::warn!(
                    thread_id = %recent.key.thread_id,
                    agent_ilk = %recent.key.agent_ilk,
                    error = %err,
                    "sy.cognition could not read a communication from its file; its turn is not kept"
                );
                continue;
            }
        };
        held.agent_node = recent.agent_node;
        held.src_ilk = recent.src_ilk;
        held.dst_ilk = recent.dst_ilk;
        held.ich = recent.ich;
        push_recent_turn(&mut held.turns, recent.turn);
        if !touched.contains(&recent.key) {
            touched.push(recent.key);
        }
    }
    touched
}

/// Adds a turn to a communication's last turns, unless they hold it already (a batch sent again).
fn push_recent_turn(turns: &mut Vec<MemoryTurn>, turn: MemoryTurn) {
    if turns.contains(&turn) {
        return;
    }
    turns.push(turn);
    bound_recent_turns(turns);
}

/// The oldest turns go past `COGNITION_RECENT_TURNS`, and past `COGNITION_RECENT_TURNS_MAX_BYTES`;
/// the newest always stays.
fn bound_recent_turns(turns: &mut Vec<MemoryTurn>) {
    let excess = turns.len().saturating_sub(COGNITION_RECENT_TURNS);
    turns.drain(..excess);
    let size = |turn: &MemoryTurn| serde_json::to_vec(turn).map_or(0, |bytes| bytes.len() + 1);
    let mut bytes: usize = turns.iter().map(size).sum();
    let mut oldest = 0;
    while turns.len() - oldest > 1 && bytes > COGNITION_RECENT_TURNS_MAX_BYTES {
        bytes -= size(&turns[oldest]);
        oldest += 1;
    }
    turns.drain(..oldest);
}

/// One communication's last turns from two copies of them, the worker's and what is held: each turn
/// of either once, in the order they were taken, bounded as at intake.
fn merge_recent_turns(worker: &[MemoryTurn], held: &[MemoryTurn]) -> Vec<MemoryTurn> {
    let mut merged = worker.to_vec();
    for turn in held {
        if !merged.contains(turn) {
            merged.push(turn.clone());
        }
    }
    merged.sort_by_key(|turn| parse_rfc3339_epoch_ms(&turn.ts).unwrap_or_default());
    bound_recent_turns(&mut merged);
    merged
}

/// The worker's turn back into what is held: its derivations replace the held ones, and what the
/// intake owns stays as the intake left it. The turns are the copy's with those the intake took
/// while the model answered, merged, so none is lost even if what is held was started again.
fn write_back_turn(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    key: &CommunicationKey,
    mut updated: ThreadCognitionState,
) {
    if let Some(held) = threads.get_mut(key) {
        updated.turns = merge_recent_turns(&updated.turns, &held.turns);
        updated.src_ilk = held.src_ilk.take();
        updated.dst_ilk = held.dst_ilk.take();
        updated.ich = held.ich.take();
    }
    threads.insert(key.clone(), updated);
}

/// One turn of a communication (§6.2): the ILK of the agent that owns it, its state (from the file
/// when it is not held), the tagger and the derivations, its file row and the agent's memory
/// package.
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
    // Only a turn with a person at its other end is analyzed (docs/cognition-hands-v1.md §6): a turn
    // between agents, or a WF's own text, stays among the communication's last turns, which the
    // intake took and wrote, and in `turns`.
    if !person_at_other_end(&msg, &turn.agent_node, |ilk| {
        message_writer::ilk_facts(&app_state.hive_id, ilk)
    }) {
        app_state
            .runtime_state
            .lock()
            .await
            .turns_not_analyzed_total += 1;
        tracing::debug!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            agent_ilk = %agent_ilk,
            "sy.cognition took a turn with no person at its other end: not analyzed"
        );
        return;
    }

    // Every region write keeps this communication until the turn is done (`run_turn_worker`).
    *app_state.worker_turn.lock().await = Some(key.clone());
    // Held, or back from the file, where the intake wrote its turn.
    let mut loads = StoreLoads::default();
    let held = held_or_loaded(
        &mut *app_state.thread_states.lock().await,
        &app_state.store,
        &key,
        &mut loads,
    )
    .map(|_| ());
    loads.count(&mut *app_state.runtime_state.lock().await);
    if let Err(err) = held {
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            agent_ilk = %agent_ilk,
            error = %err,
            "sy.cognition could not read a communication from its file; turn skipped"
        );
        return;
    }

    // Whose feelings the turn moves, and who spoke it for the narrator (§10.3).
    let facts = |ilk: &str| message_writer::ilk_facts(&app_state.hive_id, ilk);
    let owner = feeling_owner(&msg, &turn.agent_node, facts);
    let speaker = narrator_speaker(&msg, &turn.agent_node, facts);
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

    // The derivations run on a copy, without the lock: the intake goes on taking turns while the
    // summarizer answers, and they stay (`write_back_turn`). Every region write keeps the worker's
    // communication, so it is still held.
    let Some(mut thread_state) = app_state.thread_states.lock().await.get(&key).cloned() else {
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            agent_ilk = %agent_ilk,
            "sy.cognition no longer holds the communication it loaded; turn skipped"
        );
        return;
    };
    thread_state.agent_node = turn.agent_node.clone();
    let narrative = update_thread_state(
        &key,
        src_ilk.as_deref(),
        dst_ilk.as_deref(),
        owner.as_ref(),
        speaker.as_deref(),
        &tagger,
        &thresholds,
        &api_key,
        &account,
        &semantic_tagger_config,
        &ts,
        &mut thread_state,
        &AiNarrativeSummarizer,
    )
    .await;

    // Its file row goes with the region's rewrite, both from what is held then.
    if let Err(err) = sync_held_state(&app_state, std::slice::from_ref(&key), |threads| {
        write_back_turn(threads, &key, thread_state);
        vec![key.clone()]
    })
    .await
    {
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
            .map(|thread| thread.contexts.len() as u64)
            .sum();
        let open_reasons = threads
            .values()
            .map(|thread| thread.reasons.len() as u64)
            .sum();
        let open_cooccurrences = threads
            .values()
            .map(|thread| thread.cooccurrences.len() as u64)
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
    state.last_trace_id = Some(msg.routing.trace_id.clone());
    state.last_thread_id = Some(thread_id);
    state.last_src_ilk = src_ilk;
    state.last_ich = ich;
    state.last_tags = tagger.tags;
    state.last_reason_signals_canonical = tagger.reason_signals_canonical;
    state.last_reason_signals_extra = tagger.reason_signals_extra;
    if narrative.called {
        state.narrative_summarizer_calls_total =
            state.narrative_summarizer_calls_total.saturating_add(1);
        state.last_narrative_model = Some(semantic_tagger_config.model.clone());
    }
    if narrative.failed {
        state.narrative_summarizer_failures_total =
            state.narrative_summarizer_failures_total.saturating_add(1);
    }
    if narrative.invalid_output {
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

/// Changes what this cognition holds and writes it out, under the region's lock and then the held
/// state's, the one order every writer takes them in, so the intake and the worker never lose each
/// other's writes. `change` updates the held communications and names those it changed: each is
/// written whole to the file, with those a failed write left only in memory, then the region is
/// rewritten from what is held, and what did not make the region is let go from memory. The file
/// keeps it (docs/cognition-hands-v1.md §11.3). The communications just changed (`kept`), the
/// worker's and the unsaved stay held whether or not they fit the region.
async fn sync_held_state<F>(
    app_state: &CognitionAppState,
    kept: &[CommunicationKey],
    change: F,
) -> Result<(), json_router::shm::ShmError>
where
    F: FnOnce(&mut HashMap<CommunicationKey, ThreadCognitionState>) -> Vec<CommunicationKey>,
{
    let mut memory_region = app_state.memory_region.lock().await;
    let mut threads = app_state.thread_states.lock().await;
    let changed = change(&mut threads);
    // The worker's communication stays too, whoever writes: its turn is back only after its model
    // calls.
    let mut kept = kept.to_vec();
    if let Some(key) = app_state.worker_turn.lock().await.clone() {
        if !kept.contains(&key) {
            kept.push(key);
        }
    }
    let written = {
        let mut unsaved = app_state
            .unsaved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut to_save = changed;
        for key in unsaved.drain() {
            if !to_save.contains(&key) {
                to_save.push(key);
            }
        }
        let written = app_state
            .store
            .save(to_save.iter().filter_map(|key| threads.get_key_value(key)));
        if written.is_err() {
            unsaved.extend(to_save.into_iter().filter(|key| threads.contains_key(key)));
        }
        for key in unsaved.iter() {
            if !kept.contains(key) {
                kept.push(key.clone());
            }
        }
        written
    };
    let synced = match memory_region.as_mut() {
        Some(region) => retain_memory_hot_set(&mut threads, &kept).and_then(|hot_set| {
            region.write_snapshot(&hot_set.snapshot)?;
            Ok(Some(hot_set.stats))
        }),
        None => Ok(None),
    };
    drop(threads);
    drop(memory_region);

    let mut state = app_state.runtime_state.lock().await;
    match written {
        Ok(rows) => state.store_writes_total = state.store_writes_total.saturating_add(rows),
        Err(err) => {
            state.store_write_failures_total = state.store_write_failures_total.saturating_add(1);
            tracing::warn!(error = %err, "sy.cognition could not write its communications to its file");
        }
    }
    let stats = synced?;
    let sync_status = match &stats {
        None => "skipped_missing_region",
        Some(stats) if stats.pruned_threads_total > 0 => "ok_pruned",
        Some(_) => "ok",
    };
    if let Some(stats) = stats {
        state.shm_hot_threads_total = stats.selected_threads_total;
        state.shm_pruned_threads_total = stats.pruned_threads_total;
        state.shm_payload_bytes = stats.payload_bytes;
    }
    state.last_shm_sync_status = Some(sync_status.to_string());
    state.last_shm_sync_at = Some(chrono::Utc::now().to_rfc3339());
    Ok(())
}

/// Ranks the communications this cognition holds and keeps, in rank order, each one whose entry
/// still fits the region. The communications just updated (`kept`) go first, so another is dropped
/// before them; one that does not fit alone stays out of the region. Then recency (the last turn, or `last_seen_at`), an active scope, live entities,
/// `turn_count` and the key. Ranked by live entities ahead of recency, a new thread in a full
/// region was dropped right after its own turn and started over on every turn.
fn build_memory_hot_set_snapshot(
    threads: &HashMap<CommunicationKey, ThreadCognitionState>,
    kept: &[CommunicationKey],
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
        let package = build_memory_package_for_thread(&key.thread_id, thread_state);
        let entry = MemoryShmThreadEntry {
            thread_id: key.thread_id.clone(),
            agent_ilk: key.agent_ilk.clone(),
            agent_node: thread_state.agent_node.clone(),
            package,
        };
        let serialized_bytes = serde_json::to_vec(&entry)?.len();
        candidates.push(MemoryHotThreadCandidate {
            key: key.clone(),
            entry,
            serialized_bytes,
            just_updated: kept.contains(key),
            has_active_scope: thread_state.active_scope.is_some(),
            live_entities_total: thread_live_entities_total(thread_state),
            last_seen_epoch_ms: thread_recency_epoch_ms(thread_state),
            turn_count: thread_state.turn_count,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .just_updated
            .cmp(&left.just_updated)
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

/// The one rule for what this cognition holds in memory: build the jsr-memory hot set and let go
/// every communication that did not make it, but those in `kept`, which stay held even when they do
/// not fit the region. Applied at every intake and live turn and at start, so memory stays bounded
/// by the SHM capacity and a batch. A communication let go comes back from the file at its next
/// turn.
fn retain_memory_hot_set(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    kept: &[CommunicationKey],
) -> Result<MemoryHotSetBuild, json_router::shm::ShmError> {
    let hot_set = build_memory_hot_set_snapshot(threads, kept)?;
    threads.retain(|key, _| hot_set.selected_thread_ids.contains(key) || kept.contains(key));
    Ok(hot_set)
}

/// What a communication holds: every item in it is open, since one that closes leaves it.
fn thread_live_entities_total(thread_state: &ThreadCognitionState) -> usize {
    usize::from(thread_state.active_scope.is_some())
        + thread_state.contexts.len()
        + thread_state.reasons.len()
        + thread_state.cooccurrences.len()
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

/// When a communication last had a turn, for the region's ranking and the file's order: the
/// worker's last, or a later one the intake took.
fn thread_recency_epoch_ms(thread_state: &ThreadCognitionState) -> i64 {
    let last_turn = thread_state
        .turns
        .last()
        .and_then(|turn| parse_rfc3339_epoch_ms(&turn.ts))
        .unwrap_or_default();
    thread_last_seen_epoch_ms(thread_state).max(last_turn)
}

fn parse_rfc3339_epoch_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
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

/// A communication's memory package (docs/reference/cognition.md §7): its labels, memories and
/// episodes from the worker, and its last turns from the intake.
fn build_memory_package_for_thread(
    thread_id: &str,
    thread_state: &ThreadCognitionState,
) -> MemoryPackage {
    const MAX_CONTEXTS: usize = 6;
    const MAX_REASONS: usize = 6;
    const MAX_MEMORIES: usize = 6;

    let mut contexts: Vec<&ContextState> = thread_state.contexts.values().collect();
    contexts.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    let mut reasons: Vec<&ReasonState> = thread_state.reasons.values().collect();
    reasons.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    let mut memories: Vec<&MemoryState> = thread_state.memories.values().collect();
    memories.sort_by(|left, right| right.weight.total_cmp(&left.weight));

    // The feelings, the heaviest first, and the most recently felt on a tie (§10.3).
    let mut episodes: Vec<&EpisodeState> = thread_state.episodes.values().collect();
    episodes.sort_by(|left, right| {
        right
            .weight
            .total_cmp(&left.weight)
            .then_with(|| right.last_felt_at.cmp(&left.last_felt_at))
            .then_with(|| left.episode_id.cmp(&right.episode_id))
    });

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
    let dropped_episodes = episodes
        .len()
        .saturating_sub(COGNITION_PACKAGE_MAX_FEELINGS) as u32;

    MemoryPackage {
        package_version: MEMORY_PACKAGE_VERSION,
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
            .take(COGNITION_PACKAGE_MAX_FEELINGS)
            .map(|feeling| EpisodeSummary {
                episode_id: feeling.episode_id.clone(),
                owner: feeling.owner.clone(),
                affect: feeling.affect_id.clone(),
                context: feeling.context_label.clone(),
                title: feeling.title.clone(),
                level: feeling_level(feeling.weight, feeling_step(feeling.base_intensity))
                    .to_string(),
                weight: feeling.weight,
                last_felt_at: feeling.last_felt_at.clone(),
                summary: turn_field(&feeling.summary, COGNITION_FEELING_TEXT_MAX_CHARS),
            })
            .collect(),
        // Bounded at intake (`bound_recent_turns`): they all go.
        turns: thread_state.turns.clone(),
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
                        app_state.store.stats().ok(),
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
                        app_state.store.stats().ok(),
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

/// `store`: how many communications the file keeps, and its bytes, when it could be read.
fn build_status_payload(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_state: &CognitionRuntimeState,
    runtime_paths: &RuntimePaths,
    store: Option<(u64, u64)>,
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
        "intake_turns_total": runtime_state.intake_turns_total,
        "turns_not_analyzed_total": runtime_state.turns_not_analyzed_total,
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
        "store": runtime_paths.store_path
    });
    let store = json!({
        "path": runtime_paths.store_path,
        "communications_total": store.map(|(communications, _)| communications),
        "bytes": store.map(|(_, bytes)| bytes),
        "loaded_at_start_total": runtime_state.loaded_at_start_total,
        "loads_total": runtime_state.store_loads_total,
        "load_failures_total": runtime_state.store_load_failures_total,
        "writes_total": runtime_state.store_writes_total,
        "write_failures_total": runtime_state.store_write_failures_total
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
        "store": store,
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
            "Each communication's state lives in SY.cognition's own file on its hive (cognition.db in its folder: the folder 0700, the file 0600). It is written whole when a turn ends and when new turns come in, loaded at start for the memory region, and loaded again at the next turn of one not held. An item that closes leaves it. SY.storage keeps the messages, not cognition's state. A turn whose communication cannot be read is skipped and counted."
                .to_string(),
        ),
        Value::String(
            "Each communication keeps its last 20 turns (what its agent received and sent, 2,000 characters of text each, 48 KiB in all). They are taken when the stream batch arrives, ahead of the model calls: the file and the memory region get them at once, so the agent's next message carries the turn before it."
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
        "state": {
            "store": runtime_paths.store_path
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
            "store": runtime_paths.store_path
        }
    });
    let runtime = json!({
        "processed_turns_total": runtime_state.processed_turns_total,
        "invalid_turns_total": runtime_state.invalid_turns_total,
        "dropped_turns_queue_full_total": runtime_state.dropped_turns_queue_full_total,
        "no_agent_ilk_total": runtime_state.no_agent_ilk_total,
        "intake_turns_total": runtime_state.intake_turns_total,
        "turns_not_analyzed_total": runtime_state.turns_not_analyzed_total,
        "store_loads_total": runtime_state.store_loads_total,
        "store_load_failures_total": runtime_state.store_load_failures_total,
        "store_writes_total": runtime_state.store_writes_total,
        "store_write_failures_total": runtime_state.store_write_failures_total,
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
        "loaded_at_start_total": runtime_state.loaded_at_start_total,
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
    Ok(RuntimePaths {
        store_path: state_dir.join(state_store::STORE_FILE_NAME),
        state_dir,
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

/// One turn of a communication (`key`: its thread and the agent that owns it, §6.2): its state moves
/// on, and the turn is numbered.
async fn update_thread_state<S: NarrativeSummarizer>(
    key: &CommunicationKey,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    owner: Option<&MemoryTurnParty>,
    speaker: Option<&str>,
    tagger: &SemanticTaggerOutput,
    thresholds: &CognitionThresholds,
    api_key: &str,
    account: &AiAccount,
    semantic_tagger_config: &CognitionSemanticTaggerConfig,
    ts: &str,
    thread_state: &mut ThreadCognitionState,
    narrative: &S,
) -> NarrativeOutcome {
    if thread_state.first_seen_at.is_none() {
        thread_state.first_seen_at = Some(ts.to_string());
    }
    thread_state.last_seen_at = Some(ts.to_string());
    thread_state.turn_count = thread_state.turn_count.saturating_add(1);

    let context_candidates = build_context_candidates(tagger);
    let reason_candidates = build_reason_candidates(tagger);

    update_contexts_for_thread(
        key,
        ts,
        src_ilk,
        dst_ilk,
        thresholds.context_close,
        &context_candidates,
        &mut thread_state.contexts,
    );
    update_reasons_for_thread(
        key,
        ts,
        src_ilk,
        dst_ilk,
        thresholds.reason_close,
        &reason_candidates,
        &mut thread_state.reasons,
    );
    // A co-occurrence pairs a context with a reason, so it closes under the average of the two.
    update_cooccurrences_for_thread(
        key,
        ts,
        (thresholds.context_close + thresholds.reason_close) * 0.5,
        &context_candidates,
        &reason_candidates,
        &thread_state.contexts,
        &thread_state.reasons,
        &mut thread_state.cooccurrences,
    );
    update_scope_binding_for_thread(
        ts,
        src_ilk,
        dst_ilk,
        thread_state.turn_count,
        &thread_state.contexts,
        &thread_state.reasons,
        &mut thread_state.active_scope,
    );
    // Only a person's own turn shows feelings (docs/cognition-hands-v1.md §10.3); the narrator
    // writes the moment's text with the memory's, from the text so far of the heaviest of them.
    let felt = match owner {
        Some(_) => feeling_candidates(tagger, &thread_state.active_scope),
        None => Vec::new(),
    };
    let previous_text = owner
        .and_then(|owner| owner.ilk.as_deref())
        .and_then(|owner_ilk| {
            felt.iter()
                .filter_map(|felt| {
                    thread_state.episodes.get(&feeling_key(
                        owner_ilk,
                        &felt.episode.affect_id,
                        &felt.context_label,
                    ))
                })
                .max_by(|left, right| left.weight.total_cmp(&right.weight))
        })
        .map(|feeling| (feeling.summary.clone(), feeling.reason.clone()));
    let (narrative_outcome, feeling_text) = update_memories_and_episodes_for_thread(
        key,
        ts,
        src_ilk,
        dst_ilk,
        speaker,
        tagger,
        api_key,
        account,
        semantic_tagger_config,
        &thread_state.active_scope,
        &felt,
        previous_text
            .as_ref()
            .map(|(summary, reason)| (summary.as_str(), reason.as_str())),
        &mut thread_state.memories,
        narrative,
    )
    .await;
    if let Some(owner) = owner {
        update_feelings_for_turn(
            key,
            ts,
            owner,
            felt,
            feeling_text.as_ref(),
            &mut thread_state.episodes,
        );
    }
    narrative_outcome
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

/// The turn's contexts: each one it names takes `weight × decay + score`, and each other fades by
/// the decay. One under `close_threshold` closes and leaves the state (docs/cognition-hands-v1.md
/// §11.2): if it comes back, it starts again.
fn update_contexts_for_thread(
    key: &CommunicationKey,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    close_threshold: f64,
    candidates: &[ContextCandidate],
    contexts: &mut HashMap<String, ContextState>,
) {
    let matched: HashSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.label.as_str())
        .collect();

    for candidate in candidates {
        let context = contexts
            .entry(candidate.label.clone())
            .or_insert_with(|| ContextState {
                context_id: stable_entity_id(
                    "context",
                    &[&key.thread_id, &key.agent_ilk, &candidate.label],
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
            });
        context.last_seen_at = ts.to_string();
        context.tags = candidate.tags.clone();
        context.weight = context.weight * COGNITION_CONTEXT_DECAY_FACTOR + candidate.score;
        sample_weight(
            &mut context.weight_samples,
            &mut context.weight_avg_cumulative,
            &mut context.weight_avg_ema,
            context.weight,
            COGNITION_CONTEXT_EMA_ALPHA,
        );
        apply_ilk_participation(
            &mut context.ilk_weights,
            &mut context.ilk_profile,
            src_ilk,
            dst_ilk,
        );
    }

    contexts.retain(|label, context| {
        if matched.contains(label.as_str()) {
            return true;
        }
        context.weight *= COGNITION_CONTEXT_DECAY_FACTOR;
        sample_weight(
            &mut context.weight_samples,
            &mut context.weight_avg_cumulative,
            &mut context.weight_avg_ema,
            context.weight,
            COGNITION_CONTEXT_EMA_ALPHA,
        );
        context.weight >= close_threshold
    });
}

/// The turn's reasons, as its contexts (`update_contexts_for_thread`), with their own decay.
fn update_reasons_for_thread(
    key: &CommunicationKey,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    close_threshold: f64,
    candidates: &[ReasonCandidate],
    reasons: &mut HashMap<String, ReasonState>,
) {
    let matched: HashSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.label.as_str())
        .collect();

    for candidate in candidates {
        let reason = reasons
            .entry(candidate.label.clone())
            .or_insert_with(|| ReasonState {
                reason_id: stable_entity_id(
                    "reason",
                    &[&key.thread_id, &key.agent_ilk, &candidate.label],
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
            });
        reason.last_seen_at = ts.to_string();
        reason.signals_canonical = candidate.signals_canonical.clone();
        reason.signals_extra = candidate.signals_extra.clone();
        reason.weight = reason.weight * COGNITION_REASON_DECAY_FACTOR + candidate.score;
        sample_weight(
            &mut reason.weight_samples,
            &mut reason.weight_avg_cumulative,
            &mut reason.weight_avg_ema,
            reason.weight,
            COGNITION_REASON_EMA_ALPHA,
        );
        apply_ilk_participation(
            &mut reason.ilk_weights,
            &mut reason.ilk_profile,
            src_ilk,
            dst_ilk,
        );
    }

    reasons.retain(|label, reason| {
        if matched.contains(label.as_str()) {
            return true;
        }
        reason.weight *= COGNITION_REASON_DECAY_FACTOR;
        sample_weight(
            &mut reason.weight_samples,
            &mut reason.weight_avg_cumulative,
            &mut reason.weight_avg_ema,
            reason.weight,
            COGNITION_REASON_EMA_ALPHA,
        );
        reason.weight >= close_threshold
    });
}

/// The turn's co-occurrences: each pair of a context and a reason it names, both held, as a context
/// (`update_contexts_for_thread`), with its own decay.
fn update_cooccurrences_for_thread(
    key: &CommunicationKey,
    ts: &str,
    close_threshold: f64,
    context_candidates: &[ContextCandidate],
    reason_candidates: &[ReasonCandidate],
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    cooccurrences: &mut HashMap<String, CooccurrenceState>,
) {
    let mut matched = HashSet::new();

    for context_candidate in context_candidates {
        let Some(context) = contexts.get(&context_candidate.label) else {
            continue;
        };
        for reason_candidate in reason_candidates {
            let Some(reason) = reasons.get(&reason_candidate.label) else {
                continue;
            };

            let pair_key = format!("{}|{}", context.label, reason.label);
            matched.insert(pair_key.clone());
            let score = (context_candidate.score + reason_candidate.score) * 0.5;
            let cooccurrence = cooccurrences
                .entry(pair_key)
                .or_insert_with(|| CooccurrenceState {
                    cooccurrence_id: stable_entity_id(
                        "cooccurrence",
                        &[
                            &key.thread_id,
                            &key.agent_ilk,
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
                });
            cooccurrence.context_id = context.context_id.clone();
            cooccurrence.context_label = context.label.clone();
            cooccurrence.reason_id = reason.reason_id.clone();
            cooccurrence.reason_label = reason.label.clone();
            cooccurrence.last_seen_at = ts.to_string();
            cooccurrence.weight = cooccurrence.weight * COGNITION_COOCCURRENCE_DECAY_FACTOR + score;
            cooccurrence.occurrences = cooccurrence.occurrences.saturating_add(1);
            sample_weight(
                &mut cooccurrence.weight_samples,
                &mut cooccurrence.weight_avg_cumulative,
                &mut cooccurrence.weight_avg_ema,
                cooccurrence.weight,
                COGNITION_COOCCURRENCE_EMA_ALPHA,
            );
        }
    }

    cooccurrences.retain(|pair_key, cooccurrence| {
        if matched.contains(pair_key) {
            return true;
        }
        cooccurrence.weight *= COGNITION_COOCCURRENCE_DECAY_FACTOR;
        sample_weight(
            &mut cooccurrence.weight_samples,
            &mut cooccurrence.weight_avg_cumulative,
            &mut cooccurrence.weight_avg_ema,
            cooccurrence.weight,
            COGNITION_COOCCURRENCE_EMA_ALPHA,
        );
        cooccurrence.weight >= close_threshold
    });
}

/// The turn's scope: the pair of the dominant context and reason it binds to. A sustained shift
/// to another pair cuts it and opens another. Each scope keeps its pair's labels, tags and signals,
/// which its memory and its feelings read, whether or not the pair's items are still held.
fn update_scope_binding_for_thread(
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    turn_count: u64,
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    active_scope: &mut Option<ScopeBindingState>,
) {
    // On a weight tie the scope's own context/reason stays dominant. HashMap order used to
    // pick one, so a tag reinforced together with the incumbent could pose as a topic change.
    let incumbent = active_scope.as_ref();
    let Some(context) = select_dominant_context(
        contexts,
        incumbent.map(|scope| scope.dominant_context_id.as_str()),
    ) else {
        return;
    };
    let Some(reason) = select_dominant_reason(
        reasons,
        incumbent.map(|scope| scope.dominant_reason_id.as_str()),
    ) else {
        return;
    };

    let current_label = format!("{} :: {}", context.label, reason.label);
    let current_ilk_weights = current_turn_ilk_weights(src_ilk, dst_ilk);
    let opened = |binding_energy_ema: f64, label: String, ilk_weights: BTreeMap<String, f64>| {
        ScopeBindingState {
            scope_id: format!("scope:{}", Uuid::new_v4()),
            label,
            dominant_context_id: context.context_id.clone(),
            dominant_context_label: context.label.clone(),
            dominant_context_tags: context.tags.clone(),
            dominant_reason_id: reason.reason_id.clone(),
            dominant_reason_label: reason.label.clone(),
            dominant_reason_signals: reason.signals_canonical.clone(),
            ilk_weights,
            binding_energy_ema,
            opened_at: ts.to_string(),
            last_seen_at: ts.to_string(),
            unbind_streak: 0,
        }
    };

    match active_scope {
        None => {
            *active_scope = Some(opened(1.0, current_label, current_ilk_weights));
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
                *scope = opened(binding, current_label, current_ilk_weights);
            } else if candidate_shift && binding < COGNITION_SCOPE_UNBIND_THRESHOLD {
                // The candidate does not bind to this scope: the shift is pending. The scope
                // keeps its own context/reason so the next turns keep measuring the divergence
                // until the sustain count cuts it (§3.2, §7.2). Relabeling here reset the
                // streak, so a lasting topic change renamed the scope instead of cutting it.
                scope.last_seen_at = ts.to_string();
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
            }
        }
    }
}

/// The memory of the active scope, from the narrator, which also writes the text of the moment the
/// turn's feelings (`felt`) come from, one for all of them, from the text so far. The narrator reads
/// who spoke (`speaker`), so the memory names each person (docs/cognition-hands-v1.md §10.3), and
/// the scope's pair. Nothing without an active scope; no feeling text when the narrator fails.
async fn update_memories_and_episodes_for_thread<S: NarrativeSummarizer>(
    key: &CommunicationKey,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    speaker: Option<&str>,
    tagger: &SemanticTaggerOutput,
    api_key: &str,
    account: &AiAccount,
    semantic_tagger_config: &CognitionSemanticTaggerConfig,
    active_scope: &Option<ScopeBindingState>,
    felt: &[FeelingCandidate],
    previous_feeling_text: Option<(&str, &str)>,
    memories: &mut HashMap<String, MemoryState>,
    narrative: &S,
) -> (NarrativeOutcome, Option<EpisodeNarrative>) {
    let Some(scope) = active_scope.as_ref() else {
        return (NarrativeOutcome::default(), None);
    };

    let narrative_result = match narrative
        .summarize(NarrativeSummarizerAiInput {
            api_key,
            account,
            config: semantic_tagger_config,
            thread_id: &key.thread_id,
            speaker,
            context_label: &scope.dominant_context_label,
            context_tags: &scope.dominant_context_tags,
            reason_label: &scope.dominant_reason_label,
            canonical_signals: &scope.dominant_reason_signals,
            extra_signals: &tagger.reason_signals_extra,
            previous_memory_summary: memories.get(&scope.scope_id).map(|m| m.summary.as_str()),
            episode_affects: felt
                .iter()
                .map(|felt| felt.episode.affect_id.as_str())
                .collect(),
            episode_titles: felt
                .iter()
                .map(|felt| felt.episode.title.as_str())
                .collect(),
            previous_episode_summary: previous_feeling_text.map(|(summary, _)| summary),
            previous_episode_reason: previous_feeling_text.map(|(_, reason)| reason),
        })
        .await
    {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                thread_id = %key.thread_id,
                context = %scope.dominant_context_label,
                reason = %scope.dominant_reason_label,
                error = %err,
                "sy.cognition narrative summarizer failed; skipping memory/episode synthesis for turn"
            );
            let invalid_output = matches!(
                err,
                fluxbee_ai_sdk::AiSdkError::Json(_) | fluxbee_ai_sdk::AiSdkError::Protocol(_)
            );
            return (
                NarrativeOutcome {
                    called: true,
                    failed: !invalid_output,
                    invalid_output,
                },
                None,
            );
        }
    };

    let current_ilk_weights = current_turn_ilk_weights(src_ilk, dst_ilk);
    update_memories_for_thread(
        key,
        ts,
        scope,
        &narrative_result.memory_summary,
        &current_ilk_weights,
        memories,
    );
    (
        NarrativeOutcome {
            called: true,
            failed: false,
            invalid_output: false,
        },
        narrative_result.episode,
    )
}

/// The active scope's memory takes the narrator's text and `weight × decay + energy`; every other
/// memory of the communication fades by the decay. Memories never close.
fn update_memories_for_thread(
    key: &CommunicationKey,
    ts: &str,
    scope: &ScopeBindingState,
    narrative_summary: &str,
    current_ilk_weights: &BTreeMap<String, f64>,
    memories: &mut HashMap<String, MemoryState>,
) {
    let memory_key = scope.scope_id.clone();
    let summary = narrative_summary.to_string();
    let memory = memories
        .entry(memory_key.clone())
        .or_insert_with(|| MemoryState {
            memory_id: stable_entity_id(
                "memory",
                &[&key.thread_id, &key.agent_ilk, &scope.scope_id],
            ),
            scope_id: scope.scope_id.clone(),
            summary: summary.clone(),
            weight: 0.0,
            occurrences: 0,
            dominant_context_id: scope.dominant_context_id.clone(),
            dominant_reason_id: scope.dominant_reason_id.clone(),
            ilk_weights: BTreeMap::new(),
            created_at: ts.to_string(),
            last_seen_at: ts.to_string(),
        });
    memory.summary = summary;
    memory.scope_id = scope.scope_id.clone();
    memory.weight =
        memory.weight * COGNITION_MEMORY_DECAY_FACTOR + scope.binding_energy_ema.max(0.25);
    memory.occurrences = memory.occurrences.saturating_add(1);
    memory.dominant_context_id = scope.dominant_context_id.clone();
    memory.dominant_reason_id = scope.dominant_reason_id.clone();
    merge_ilk_weights(&mut memory.ilk_weights, current_ilk_weights);
    memory.last_seen_at = ts.to_string();

    for (other_key, other_memory) in memories.iter_mut() {
        if other_key != &memory_key {
            other_memory.weight *= COGNITION_MEMORY_DECAY_FACTOR;
        }
    }
}

/// A feeling's key in its communication (docs/cognition-hands-v1.md §10.3): its owner, its affect
/// and its context.
fn feeling_key(owner_ilk: &str, affect_id: &str, context_label: &str) -> String {
    format!("{owner_ilk}|{affect_id}|{context_label}")
}

/// The feelings a person's turn shows (docs/cognition-hands-v1.md §10.3): each affect the gate
/// passes (`build_episode_candidates`), felt around the active scope's context and reason. None
/// without an active scope, as a memory needs.
fn feeling_candidates(
    tagger: &SemanticTaggerOutput,
    active_scope: &Option<ScopeBindingState>,
) -> Vec<FeelingCandidate> {
    let Some(scope) = active_scope.as_ref() else {
        return Vec::new();
    };
    build_episode_candidates(tagger, &scope.dominant_context_label)
        .into_iter()
        .map(|episode| FeelingCandidate {
            episode,
            scope_id: scope.scope_id.clone(),
            context_id: scope.dominant_context_id.clone(),
            context_label: scope.dominant_context_label.clone(),
            reason_id: scope.dominant_reason_id.clone(),
        })
        .collect()
}

/// How many times a context's half-life a feeling's is (`COGNITION_FEELING_INERTIA`).
fn feeling_inertia(affect_id: &str) -> f64 {
    COGNITION_FEELING_INERTIA
        .iter()
        .find(|(affect, _)| *affect == affect_id)
        .map_or(1.0, |(_, inertia)| *inertia)
}

/// What each of its owner's turns keeps of a feeling: a context's decay to the root of the
/// feeling's inertia, so its half-life is that many times a context's.
fn feeling_decay(affect_id: &str) -> f64 {
    COGNITION_CONTEXT_DECAY_FACTOR.powf(1.0 / feeling_inertia(affect_id))
}

/// What a turn that shows a feeling adds to it: its affect's base intensity over 4, so anger and
/// escalation add 2 and urgency 1.75, where a context adds 1.
fn feeling_step(base_intensity: f64) -> f64 {
    base_intensity / 4.0
}

/// A feeling's EMA alpha: a context's, slowed by the feeling's inertia.
fn feeling_ema_alpha(affect_id: &str) -> f64 {
    COGNITION_CONTEXT_EMA_ALPHA / feeling_inertia(affect_id)
}

/// How strong a feeling is now, as the agent reads it: `strong` from three steps up, `present` from
/// one, `faint` under one, a feeling shown once and fading.
fn feeling_level(weight: f64, step: f64) -> &'static str {
    if weight >= 3.0 * step {
        "strong"
    } else if weight >= step {
        "present"
    } else {
        "faint"
    }
}

/// An item's weight averages (FINDINGS A-156): over each turn it is open, the turn that names it
/// and a turn that only decays it, its weight after the turn.
fn sample_weight(samples: &mut u64, cumulative: &mut f64, ema: &mut f64, weight: f64, alpha: f64) {
    *samples = samples.saturating_add(1);
    *cumulative = update_cumulative_average(*cumulative, weight, *samples);
    *ema = update_ema(*ema, weight, alpha);
}

/// Moves the feelings of a turn's owner (docs/cognition-hands-v1.md §10.3):
/// - each feeling of the owner the turn does not show takes `weight × decay`; one under
///   `COGNITION_FEELING_CLOSE_THRESHOLD` closes and leaves the state (§11.2);
/// - each one it shows (`felt`, every affect the gate passed) takes `weight × decay + step`, and the
///   narrator's text of the moment when the narrator wrote one; a feeling it opens without one has
///   no text yet.
///
/// Other people's feelings stay: each person's turns are their own clock.
fn update_feelings_for_turn(
    key: &CommunicationKey,
    ts: &str,
    owner: &MemoryTurnParty,
    felt: Vec<FeelingCandidate>,
    text: Option<&EpisodeNarrative>,
    feelings: &mut HashMap<String, EpisodeState>,
) {
    let Some(owner_ilk) = owner.ilk.clone() else {
        return;
    };
    let felt: Vec<(String, FeelingCandidate)> = felt
        .into_iter()
        .map(|felt| {
            let key = feeling_key(&owner_ilk, &felt.episode.affect_id, &felt.context_label);
            (key, felt)
        })
        .collect();

    feelings.retain(|feeling_key, feeling| {
        if feeling.owner.ilk.as_deref() != Some(owner_ilk.as_str())
            || felt.iter().any(|(felt_key, _)| felt_key == feeling_key)
        {
            return true;
        }
        feeling.weight *= feeling_decay(&feeling.affect_id);
        sample_weight(
            &mut feeling.weight_samples,
            &mut feeling.weight_avg_cumulative,
            &mut feeling.weight_avg_ema,
            feeling.weight,
            feeling_ema_alpha(&feeling.affect_id),
        );
        feeling.weight >= COGNITION_FEELING_CLOSE_THRESHOLD
    });

    for (felt_key, felt) in felt {
        let candidate = felt.episode;
        let feeling = feelings.entry(felt_key).or_insert_with(|| EpisodeState {
            episode_id: stable_entity_id(
                "episode",
                &[
                    &key.thread_id,
                    &key.agent_ilk,
                    &owner_ilk,
                    &candidate.affect_id,
                    &felt.context_label,
                ],
            ),
            owner: owner.clone(),
            affect_id: candidate.affect_id.clone(),
            context_id: felt.context_id.clone(),
            context_label: felt.context_label.clone(),
            scope_id: felt.scope_id.clone(),
            title: candidate.title.clone(),
            summary: String::new(),
            reason: String::new(),
            base_intensity: candidate.base_intensity,
            evidence_strength: candidate.evidence_strength,
            evidence_context_ids: Vec::new(),
            evidence_reason_ids: Vec::new(),
            evidence_signals: Vec::new(),
            weight: 0.0,
            weight_avg_cumulative: 0.0,
            weight_avg_ema: 0.0,
            weight_samples: 0,
            occurrences: 0,
            created_at: ts.to_string(),
            last_felt_at: ts.to_string(),
        });
        // The owner's name as this turn gives it, which the identity may have learned since.
        feeling.owner = owner.clone();
        feeling.last_felt_at = ts.to_string();
        feeling.scope_id = felt.scope_id.clone();
        feeling.title = candidate.title.clone();
        feeling.base_intensity = feeling.base_intensity.max(candidate.base_intensity);
        feeling.evidence_strength = feeling.evidence_strength.max(candidate.evidence_strength);
        feeling.weight = feeling.weight * feeling_decay(&feeling.affect_id)
            + feeling_step(candidate.base_intensity);
        sample_weight(
            &mut feeling.weight_samples,
            &mut feeling.weight_avg_cumulative,
            &mut feeling.weight_avg_ema,
            feeling.weight,
            feeling_ema_alpha(&feeling.affect_id),
        );
        feeling.occurrences = feeling.occurrences.saturating_add(1);
        if let Some(text) = text {
            feeling.summary = text.summary.clone();
            feeling.reason = text.reason.clone();
        }
        if !feeling.evidence_context_ids.contains(&felt.context_id) {
            feeling.evidence_context_ids.push(felt.context_id.clone());
        }
        if !feeling.evidence_reason_ids.contains(&felt.reason_id) {
            feeling.evidence_reason_ids.push(felt.reason_id.clone());
        }
        for signal in &candidate.evidence_signals {
            if !feeling.evidence_signals.contains(signal) {
                feeling.evidence_signals.push(signal.clone());
            }
        }
    }
}

/// The deterministic episode gate: each affect fires on its own combination of canonical and affect
/// signals, so one turn may show several (FINDINGS A-155). The summary and reason are the narrative
/// summarizer's.
fn build_episode_candidates(
    tagger: &SemanticTaggerOutput,
    context_label: &str,
) -> Vec<EpisodeCandidate> {
    let affect_signals: HashSet<&str> = tagger.affect_signals.iter().map(String::as_str).collect();
    let canonical_signals: HashSet<&str> = tagger
        .reason_signals_canonical
        .iter()
        .map(String::as_str)
        .collect();
    let candidate = |affect_id: &str,
                     title: String,
                     evidence_signals: &[&str],
                     base_intensity: f64,
                     evidence_strength: f64| EpisodeCandidate {
        affect_id: affect_id.to_string(),
        title,
        base_intensity,
        evidence_strength,
        evidence_signals: evidence_signals.iter().map(ToString::to_string).collect(),
    };

    let mut out = Vec::new();
    if affect_signals.contains("frustration")
        && (canonical_signals.contains("challenge") || canonical_signals.contains("resolve"))
    {
        out.push(candidate(
            "anger",
            format!("Friction around {context_label}"),
            &["frustration", "challenge"],
            8.0,
            9.0,
        ));
    }
    if affect_signals.contains("escalation")
        && (canonical_signals.contains("protect") || canonical_signals.contains("challenge"))
    {
        out.push(candidate(
            "escalation",
            format!("Escalation around {context_label}"),
            &["escalation", "protect"],
            8.0,
            9.0,
        ));
    }
    if affect_signals.contains("urgency")
        && canonical_signals.contains("resolve")
        && canonical_signals.contains("request")
    {
        out.push(candidate(
            "urgency",
            format!("Urgent push on {context_label}"),
            &["urgency", "request", "resolve"],
            7.0,
            8.0,
        ));
    }
    out.retain(|candidate| {
        candidate.base_intensity >= COGNITION_EPISODE_MIN_INTENSITY
            && candidate.evidence_strength >= COGNITION_EPISODE_MIN_EVIDENCE_STRENGTH
    });
    out
}

fn select_dominant_context<'a>(
    contexts: &'a HashMap<String, ContextState>,
    incumbent_id: Option<&str>,
) -> Option<&'a ContextState> {
    contexts.values().max_by(|left, right| {
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
    reasons.values().max_by(|left, right| {
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
    ilk_profile: &mut BTreeMap<String, IlkProfile>,
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
    const TEST_AGENT_NODE: &str = "AI.sales@motherbee";
    /// 2026-10-30T12:00:00Z, in milliseconds: when a router took a test turn.
    const TEST_TS: u64 = 1_793_361_600_000;

    fn key(thread_id: &str) -> CommunicationKey {
        CommunicationKey {
            thread_id: thread_id.to_string(),
            agent_ilk: TEST_AGENT.to_string(),
        }
    }

    fn text(content: &str) -> Value {
        json!({ "type": "text", "content": content })
    }

    /// A copy of a turn of `TEST_THREAD` as the agent's router hands it to cognition: `src` is
    /// the sender as the router names it.
    fn turn_copy(src: &str, src_ilk: Option<&str>, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some(src.to_string()),
                dst: Destination::Unicast(TEST_AGENT_NODE.to_string()),
                ttl: 8,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                src_ilk: src_ilk.map(str::to_string),
                thread_id: Some(TEST_THREAD.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    /// What the person writes to the agent through IO.api.
    fn person_says(content: &str) -> Message {
        turn_copy("IO.api@motherbee", Some("ilk:person"), text(content))
    }

    /// What a supervisor writes through the supervisors' io.slack, and a WF forwards to the agent.
    fn supervisor_says(content: &str) -> Message {
        let mut message = turn_copy("WF.sales@motherbee", Some("ilk:supervisor"), text(content));
        message.meta.context = Some(json!({"trace": [
            {"t": "2026-10-30T12:05:00.000Z", "from": "IO.slack.supervisores@motherbee",
             "to": "WF.sales@motherbee"},
            {"t": "2026-10-30T12:05:00.050Z", "from": "WF.sales@motherbee",
             "to": TEST_AGENT_NODE, "act": ["forward"]}
        ]}));
        message
    }

    /// What the agent answers.
    fn agent_says(content: &str) -> Message {
        turn_copy(TEST_AGENT_NODE, Some(TEST_AGENT), text(content))
    }

    /// What an identity with no ILK says: nothing.
    fn no_facts(_: &str) -> Option<IlkFacts> {
        None
    }

    /// The turn the intake keeps of `message`, which its router took at `ts_ms`.
    fn taken(message: &Message, ts_ms: u64) -> RecentTurn {
        recent_turn_of(message, TEST_AGENT_NODE, TEST_AGENT, ts_ms, no_facts).expect("a turn")
    }

    fn texts(turns: &[MemoryTurn]) -> Vec<&str> {
        turns.iter().map(|turn| turn.text.as_str()).collect()
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
            let affects = input.episode_affects.join(", ");
            let episode = (!affects.is_empty()).then(|| EpisodeNarrative {
                summary: format!("AI episode summary ({affects})."),
                reason: format!("AI episode reason ({affects})."),
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

    /// A person of the tests, by their ILK and name.
    fn person(ilk: &str, name: &str) -> MemoryTurnParty {
        MemoryTurnParty {
            ilk: Some(ilk.to_string()),
            name: Some(name.to_string()),
            kind: Some("person".to_string()),
        }
    }

    /// One turn of Juan's through the live path, with a canned narrative instead of the AI.
    async fn live_turn(
        thread: &mut ThreadCognitionState,
        seq: u64,
        tags: &[&str],
        canonical: &[&str],
        extra: &[&str],
    ) {
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
    ) {
        live_turn_by(
            thresholds,
            Some(&person("ilk:juan", "Juan")),
            thread,
            seq,
            tags,
            canonical,
            extra,
        )
        .await
    }

    /// One turn through the live path, written by `owner`, a person, or with no owner: the agent's
    /// own answer.
    async fn live_turn_by(
        thresholds: &CognitionThresholds,
        owner: Option<&MemoryTurnParty>,
        thread: &mut ThreadCognitionState,
        seq: u64,
        tags: &[&str],
        canonical: &[&str],
        extra: &[&str],
    ) {
        let tagger = SemanticTaggerOutput {
            tags: strings(tags),
            reason_signals_canonical: strings(canonical),
            reason_signals_extra: strings(extra),
            affect_signals: strings(extra),
        };
        let ts = format!("2026-01-01T{:02}:{:02}:00Z", seq / 60, seq % 60);
        let (src_ilk, dst_ilk) = match owner {
            Some(owner) => (owner.ilk.as_deref(), Some(TEST_AGENT)),
            None => (Some(TEST_AGENT), Some("ilk:juan")),
        };
        let speaker = owner.and_then(party_words);
        update_thread_state(
            &key(TEST_THREAD),
            src_ilk,
            dst_ilk,
            owner,
            speaker.as_deref(),
            &tagger,
            thresholds,
            "test-key",
            &AiAccount::root(UsageRecorder::disabled(), "SY.cognition@motherbee"),
            &CognitionSemanticTaggerConfig::default(),
            &ts,
            thread,
            &CannedNarrative,
        )
        .await;
    }

    /// A folder of its own for a test, removed with it.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("sy-cognition-test-{}", Uuid::new_v4())))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A file of its own for a test cognition, removed with it: the file closes, then its folder
    /// goes.
    struct TestStore {
        store: CognitionStore,
        dir: TestDir,
    }

    impl std::ops::Deref for TestStore {
        type Target = CognitionStore;

        fn deref(&self) -> &CognitionStore {
            &self.store
        }
    }

    fn test_store() -> TestStore {
        let dir = TestDir::new();
        TestStore {
            store: CognitionStore::open(&dir.0).expect("a test file"),
            dir,
        }
    }

    /// The intake's turns into `threads`, from `store` for a communication not held.
    fn hold(
        threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
        store: &CognitionStore,
        recent: Vec<RecentTurn>,
    ) -> Vec<CommunicationKey> {
        hold_recent_turns(threads, store, recent, &mut StoreLoads::default())
    }

    fn sorted_keys<V>(map: &HashMap<String, V>) -> Vec<String> {
        let mut keys: Vec<String> = map.keys().cloned().collect();
        keys.sort();
        keys
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
            store_path: PathBuf::from("/var/lib/fluxbee/test/cognition.db"),
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

    /// A communication written to the file comes back as it was, and its next turn continues it:
    /// the same items under the same ids, their weights and samples, and its scope
    /// (docs/cognition-hands-v1.md §11.3).
    #[tokio::test]
    async fn the_file_keeps_a_communication_its_next_turn_continues() {
        let turn = (
            &["billing"][..],
            &["resolve", "challenge"][..],
            &["frustration"][..],
        );
        let mut reference = ThreadCognitionState::default();
        for seq in 1..=3 {
            live_turn(&mut reference, seq, turn.0, turn.1, turn.2).await;
        }
        reference.agent_node = TEST_AGENT_NODE.to_string();
        reference.turns = vec![taken(&person_says("third double charge"), TEST_TS).turn];

        let store = test_store();
        assert_eq!(
            store
                .save([(&key(TEST_THREAD), &reference)])
                .expect("written"),
            1
        );
        assert!(store.load(&key("thread:other")).expect("read").is_none());
        let mut back = store.load(&key(TEST_THREAD)).expect("read").expect("kept");
        assert_eq!(
            serde_json::to_value(&back).expect("state"),
            serde_json::to_value(&reference).expect("state"),
            "the whole state, as it was"
        );
        let scope = back.active_scope.clone().expect("its scope");
        assert!(back.memories.contains_key(&scope.scope_id));
        assert!(!back.episodes.is_empty());

        live_turn(&mut reference, 4, turn.0, turn.1, turn.2).await;
        live_turn(&mut back, 4, turn.0, turn.1, turn.2).await;
        assert_eq!(
            sorted_keys(&back.contexts),
            sorted_keys(&reference.contexts)
        );
        assert_eq!(
            sorted_keys(&back.episodes),
            sorted_keys(&reference.episodes)
        );
        let billing = &back.contexts["billing"];
        assert_eq!(billing.weight_samples, 4);
        assert!((billing.weight - reference.contexts["billing"].weight).abs() < 1e-9);
        assert_eq!(back.memories[&scope.scope_id].occurrences, 4);
        assert_eq!(
            back.active_scope.as_ref().map(|open| &open.scope_id),
            Some(&scope.scope_id)
        );

        // Written again, the row is replaced, and the most recent come first.
        let mut older = ThreadCognitionState {
            last_seen_at: Some("2025-01-01T00:00:00Z".to_string()),
            ..ThreadCognitionState::default()
        };
        older.turn_count = 1;
        store
            .save([(&key(TEST_THREAD), &back), (&key("thread:older"), &older)])
            .expect("written");
        let recent = store.most_recent(usize::MAX).expect("read");
        let keys: Vec<&str> = recent
            .iter()
            .map(|(key, _)| key.thread_id.as_str())
            .collect();
        assert_eq!(keys, [TEST_THREAD, "thread:older"]);
        assert_eq!(recent[0].1.turn_count, 4);
        assert_eq!(store.most_recent(1).expect("read").len(), 1, "a byte bound");
        assert_eq!(store.stats().expect("stats").0, 2);
    }

    /// Only this node may read its file: the folder is 0700, and the file and its WAL sidecars
    /// 0600 (docs/cognition-hands-v1.md §11.2).
    #[test]
    fn the_file_is_cognitions_alone() {
        use std::os::unix::fs::PermissionsExt;
        let store = test_store();
        store
            .save([(&key(TEST_THREAD), &ThreadCognitionState::default())])
            .expect("written");
        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&store.dir.0), 0o700);
        assert_eq!(mode(store.path()), 0o600);
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = store.path().as_os_str().to_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            if sidecar.exists() {
                assert_eq!(mode(&sidecar), 0o600, "{}", sidecar.display());
            }
        }
        // Opened again, a folder and a file made looser are made what they must be.
        fs::set_permissions(&store.dir.0, fs::Permissions::from_mode(0o755)).expect("chmod");
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).expect("chmod");
        let again = CognitionStore::open(&store.dir.0).expect("opened again");
        assert_eq!(mode(&store.dir.0), 0o700);
        assert_eq!(mode(again.path()), 0o600);
    }

    /// A row that cannot be read starts its communication again and is left out at start, without
    /// stopping the others; a row of an older shape starts again too; a row of a newer shape (after
    /// a rollback) is kept as it is; a row missing a field takes its default.
    #[test]
    fn the_file_reads_what_it_can_and_keeps_a_newer_row() {
        let store = test_store();
        store
            .save([(&key("thread:good"), &ThreadCognitionState::default())])
            .expect("written");
        let side = rusqlite::Connection::open(store.path()).expect("a side connection");
        for (thread, version, state) in [
            ("thread:broken", 1, "{not json"),
            ("thread:older", 0, "{}"),
            ("thread:newer", 2, "{\"turn_count\": 9}"),
            ("thread:partial", 1, "{\"turn_count\": 3}"),
        ] {
            side.execute(
                "INSERT INTO communications (thread_id, agent_ilk, last_seen_ms, state_version, state) \
                 VALUES (?1, ?2, 1, ?3, ?4)",
                rusqlite::params![thread, TEST_AGENT, version, state],
            )
            .expect("a row");
        }
        assert!(store.load(&key("thread:broken")).expect("read").is_none());
        assert!(store.load(&key("thread:older")).expect("read").is_none());
        assert!(
            store.load(&key("thread:newer")).is_err(),
            "a newer row is not taken"
        );
        let partial = store
            .load(&key("thread:partial"))
            .expect("read")
            .expect("kept");
        assert_eq!(partial.turn_count, 3);
        let mut loaded: Vec<String> = store
            .most_recent(usize::MAX)
            .expect("read")
            .into_iter()
            .map(|(key, _)| key.thread_id)
            .collect();
        loaded.sort();
        assert_eq!(loaded, ["thread:good", "thread:partial"]);
        // Nothing the node writes touches the newer row.
        let newer: String = side
            .query_row(
                "SELECT state FROM communications WHERE thread_id = 'thread:newer'",
                [],
                |row| row.get(0),
            )
            .expect("still there");
        assert_eq!(newer, "{\"turn_count\": 9}");
    }

    /// The communications just changed stay held even when the region cannot take them all, so the
    /// worker's write-back never finds its communication gone (the review of 2026-11-06).
    #[tokio::test]
    async fn the_communications_just_changed_stay_held_past_a_full_region() {
        let mut threads = HashMap::new();
        let mut kept = Vec::new();
        for seq in 1..=6u64 {
            let key = key(&format!("thread:{seq}"));
            threads.insert(
                key.clone(),
                thread_of_a_fifth_of_the_region(seq, &["billing"]).await,
            );
            kept.push(key);
        }
        threads.insert(
            key("thread:other"),
            thread_of_a_fifth_of_the_region(7, &["billing"]).await,
        );
        let hot_set = retain_memory_hot_set(&mut threads, &kept).expect("hot set");
        assert_eq!(hot_set.stats.selected_threads_total, 4);
        assert_eq!(threads.len(), 6, "the kept stay, the other goes");
        assert!(kept.iter().all(|key| threads.contains_key(key)));
    }

    /// A write that fails leaves its communications held and taken again by the next write: none is
    /// lost when the file comes back.
    #[tokio::test]
    async fn a_failed_write_is_taken_again() {
        let dir = TestDir::new();
        let (app_state, _router) = test_app_state("cogunsaved", None, &dir.0);
        take_recent_turns(&app_state, vec![taken(&person_says("first"), TEST_TS)]).await;
        // The file fails: the communication is held, so its next turn needs no read.
        let side = rusqlite::Connection::open(app_state.store.path()).expect("a side connection");
        side.execute_batch("DROP TABLE communications")
            .expect("the table goes");
        take_recent_turns(
            &app_state,
            vec![taken(&person_says("second"), TEST_TS + 1_000)],
        )
        .await;
        assert_eq!(
            app_state
                .runtime_state
                .lock()
                .await
                .store_write_failures_total,
            1
        );
        assert!(app_state
            .unsaved
            .lock()
            .expect("unsaved")
            .contains(&key(TEST_THREAD)));

        // The table back, the next write takes the unsaved communication with its own.
        drop(CognitionStore::open(&dir.0).expect("the table again"));
        let mut other = person_says("third");
        other.meta.thread_id = Some("thread:other".to_string());
        take_recent_turns(&app_state, vec![taken(&other, TEST_TS + 2_000)]).await;
        let first = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("written at last");
        assert_eq!(texts(&first.turns), ["first", "second"]);
        assert!(app_state
            .store
            .load(&key("thread:other"))
            .expect("read")
            .is_some());
        assert!(app_state.unsaved.lock().expect("unsaved").is_empty());
    }

    /// The intake takes a turn of a communication it does not hold from the file, its turns and
    /// its labels, and adds the new turn; one the file has not either starts new.
    #[tokio::test]
    async fn the_intake_takes_a_communication_from_the_file() {
        let store = test_store();
        let mut stored = ThreadCognitionState::default();
        live_turn(&mut stored, 1, &["billing"], &["inform"], &[]).await;
        stored.agent_node = TEST_AGENT_NODE.to_string();
        stored.turns = vec![taken(&person_says("hello"), TEST_TS).turn];
        store.save([(&key(TEST_THREAD), &stored)]).expect("written");

        let mut threads = HashMap::new();
        let mut loads = StoreLoads::default();
        let mut other = person_says("anyone?");
        other.meta.thread_id = Some("thread:new".to_string());
        let touched = hold_recent_turns(
            &mut threads,
            &store,
            vec![
                taken(&person_says("and the price?"), TEST_TS + 1_000),
                taken(&other, TEST_TS + 2_000),
            ],
            &mut loads,
        );
        assert_eq!(touched, [key(TEST_THREAD), key("thread:new")]);
        assert_eq!((loads.loaded, loads.failed), (1, 0));
        let held = &threads[&key(TEST_THREAD)];
        assert_eq!(texts(&held.turns), ["hello", "and the price?"]);
        assert_eq!(held.turn_count, 1);
        let package = build_memory_package_for_thread(TEST_THREAD, held);
        assert_eq!(
            package.dominant_context.as_ref().map(|c| c.label.as_str()),
            Some("billing")
        );
        let new = &threads[&key("thread:new")];
        assert_eq!(texts(&new.turns), ["anyone?"]);
        assert_eq!(new.turn_count, 0);
    }

    /// What is held follows the region: a thread outside the jsr-memory hot set leaves memory too,
    /// so memory stays bounded by the SHM capacity. The file keeps it.
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

        let hot_set = retain_memory_hot_set(&mut threads, &[]).expect("hot set");
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
    /// started over on every turn.
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
        let full = retain_memory_hot_set(&mut threads, &[]).expect("hot set");
        assert_eq!(full.stats.pruned_threads_total, 0);

        threads.insert(
            key("thread:new"),
            thread_of_a_fifth_of_the_region(5, &["shipping"]).await,
        );
        assert!(
            thread_live_entities_total(&threads[&key("thread:new")])
                < thread_live_entities_total(&threads[&key("thread:old:1")])
        );
        // No turn thread: the ranking alone keeps it, as at start.
        let hot_set = retain_memory_hot_set(&mut threads, &[]).expect("hot set");
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
        let hot_set = retain_memory_hot_set(&mut threads, &[key("thread:turn")]).expect("hot set");
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
                live_turn(&mut thread, seq, &["shipping"], &["protect"], &[]).await;
                let scope = thread.active_scope.as_ref().expect("scope");
                if scope.scope_id != first.scope_id {
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
            live_turn(&mut thread, seq, told, &["inform"], &[]).await;
            let scope = thread.active_scope.as_ref().expect("scope");
            assert_eq!(scope.scope_id, first.scope_id);
            assert_eq!(scope.dominant_context_label, first.dominant_context_label);
        }
    }

    /// What a feeling keeps: the gate's affect, title, intensity and evidence, and the summarizer's
    /// summary and reason. The summarizer must return both whenever there is a candidate, so there
    /// is no deterministic text behind them.
    #[tokio::test]
    async fn a_feeling_keeps_the_gate_fields_and_the_narrative_text() {
        let mut thread = ThreadCognitionState::default();
        live_turn(
            &mut thread,
            1,
            &["billing"],
            &["resolve", "challenge"],
            &["frustration"],
        )
        .await;
        assert_eq!(thread.episodes.len(), 1);
        let episode = thread.episodes.values().next().expect("a feeling");
        assert_eq!(episode.affect_id, "anger");
        assert_eq!(episode.title, "Friction around billing");
        assert_eq!(episode.summary, "AI episode summary (anger).");
        assert_eq!(episode.reason, "AI episode reason (anger).");
        assert_eq!(episode.base_intensity, 8.0);
        assert_eq!(episode.evidence_strength, 9.0);
        assert!(episode
            .evidence_signals
            .contains(&"frustration".to_string()));
    }

    #[test]
    fn the_episode_gate_needs_its_signal_combination() {
        let tagger = |canonical: &[&str], affect: &[&str]| SemanticTaggerOutput {
            tags: strings(&["billing"]),
            reason_signals_canonical: strings(canonical),
            affect_signals: strings(affect),
            ..SemanticTaggerOutput::default()
        };
        let affects = |canonical: &[&str], affect: &[&str]| {
            build_episode_candidates(&tagger(canonical, affect), "billing")
                .into_iter()
                .map(|candidate| candidate.affect_id)
                .collect::<Vec<_>>()
        };
        assert!(affects(&["inform"], &["frustration"]).is_empty());
        assert!(affects(&["resolve", "challenge"], &[]).is_empty());
        let urgency =
            build_episode_candidates(&tagger(&["resolve", "request"], &["urgency"]), "billing");
        assert_eq!(urgency.len(), 1);
        assert_eq!(urgency[0].affect_id, "urgency");
        assert_eq!(urgency[0].title, "Urgent push on billing");
        assert_eq!(urgency[0].base_intensity, 7.0);
        assert_eq!(urgency[0].evidence_strength, 8.0);
        // Each affect has its own gate, so one turn may show several (the review of 2026-11-05).
        assert_eq!(
            affects(&["challenge", "abandon"], &["frustration", "escalation"]),
            ["anger", "escalation"]
        );
        assert_eq!(
            affects(
                &["resolve", "challenge", "request"],
                &["frustration", "urgency", "escalation"]
            ),
            ["anger", "escalation", "urgency"]
        );
    }

    /// A turn that shows two affects raises both feelings, and lets neither fade; the owner's other
    /// feelings fade. The narrator writes one text for the moment.
    #[tokio::test]
    async fn a_turn_that_shows_two_affects_raises_both() {
        let ana = person("ilk:ana", "Ana");
        let mut thread = ThreadCognitionState::default();
        says(
            &mut thread,
            Some(&ana),
            1,
            (&["billing"], &["resolve", "request"], &["urgency"]),
        )
        .await;
        assert_eq!(feeling(&thread, "ilk:ana", "urgency").weight, 1.75);
        says(
            &mut thread,
            Some(&ana),
            2,
            (
                &["billing"],
                &["challenge", "resolve"],
                &["frustration", "escalation"],
            ),
        )
        .await;
        assert_eq!(thread.episodes.len(), 3);
        for affect in ["anger", "escalation"] {
            let shown = feeling(&thread, "ilk:ana", affect);
            assert_eq!(shown.weight, 2.0, "{affect}");
            assert_eq!(shown.summary, "AI episode summary (anger, escalation).");
        }
        let urgency = feeling(&thread, "ilk:ana", "urgency");
        assert!((urgency.weight - 1.75 * feeling_decay("urgency")).abs() < 1e-12);
    }

    /// Angry on billing: the tagger's signals that pass the anger gate.
    const ANGRY: (&[&str], &[&str], &[&str]) =
        (&["billing"], &["resolve", "challenge"], &["frustration"]);
    /// Calm on billing: no feeling.
    const CALM: (&[&str], &[&str], &[&str]) = (&["billing"], &["inform"], &[]);

    async fn says(
        thread: &mut ThreadCognitionState,
        owner: Option<&MemoryTurnParty>,
        seq: u64,
        (tags, canonical, extra): (&[&str], &[&str], &[&str]),
    ) {
        live_turn_by(
            &CognitionThresholds::default(),
            owner,
            thread,
            seq,
            tags,
            canonical,
            extra,
        )
        .await
    }

    fn feeling<'a>(
        thread: &'a ThreadCognitionState,
        owner: &str,
        affect: &str,
    ) -> &'a EpisodeState {
        &thread.episodes[&feeling_key(owner, affect, "billing")]
    }

    /// A feeling is its owner's (docs/cognition-hands-v1.md §10.3): Ana's anger stays while Pedro
    /// talks, fades only with Ana's own calm turns, by the root of anger's inertia, and closes on the
    /// 64th of them, leaving the state (§11.2). It names her, the context and the step.
    #[tokio::test]
    async fn a_feeling_is_its_owners_and_fades_with_their_own_turns() {
        let (ana, pedro) = (person("ilk:ana", "Ana"), person("ilk:pedro", "Pedro"));
        let mut thread = ThreadCognitionState::default();
        says(&mut thread, Some(&ana), 1, ANGRY).await;
        assert_eq!(thread.episodes.len(), 1);
        let shown = feeling(&thread, "ilk:ana", "anger");
        assert_eq!(shown.owner, ana);
        assert_eq!(shown.context_label, "billing");
        assert_eq!(shown.weight, 2.0);
        assert_eq!(shown.summary, "AI episode summary (anger).");

        for seq in 2..=11 {
            says(&mut thread, Some(&pedro), seq, CALM).await;
        }
        assert_eq!(feeling(&thread, "ilk:ana", "anger").weight, 2.0);

        let decay = feeling_decay("anger");
        assert!((decay - 0.85f64.powf(0.2)).abs() < 1e-12);
        let anger_key = feeling_key("ilk:ana", "anger", "billing");
        let mut closed_on = None;
        for calm in 1..=70u64 {
            says(&mut thread, Some(&ana), 11 + calm, CALM).await;
            let Some(anger) = thread.episodes.get(&anger_key) else {
                closed_on = Some(calm);
                break;
            };
            assert!((anger.weight - 2.0 * decay.powi(calm as i32)).abs() < 1e-9);
            assert_eq!(
                anger.last_felt_at, "2026-01-01T00:01:00Z",
                "last felt on her angry turn"
            );
        }
        assert_eq!(closed_on, Some(64));
        assert!(thread.episodes.is_empty());
    }

    /// A feeling that comes back builds up: anger on four turns in a row reads `strong`, once
    /// `present`, and fading under one step `faint`. Each affect's inertia sets its decay: urgency
    /// twice a context's half-life, anger five times, escalation ten.
    #[tokio::test]
    async fn a_feeling_that_comes_back_builds_up() {
        let ana = person("ilk:ana", "Ana");
        let mut thread = ThreadCognitionState::default();
        says(&mut thread, Some(&ana), 1, ANGRY).await;
        let level = |thread: &ThreadCognitionState| {
            let anger = feeling(thread, "ilk:ana", "anger");
            feeling_level(anger.weight, feeling_step(anger.base_intensity))
        };
        assert_eq!(level(&thread), "present");
        for seq in 2..=4 {
            says(&mut thread, Some(&ana), seq, ANGRY).await;
        }
        let anger = feeling(&thread, "ilk:ana", "anger");
        let d = feeling_decay("anger");
        assert!((anger.weight - 2.0 * (1.0 + d + d * d + d * d * d)).abs() < 1e-9);
        assert_eq!(anger.occurrences, 4);
        assert_eq!(level(&thread), "strong");

        let mut thread = ThreadCognitionState::default();
        says(&mut thread, Some(&ana), 1, ANGRY).await;
        says(&mut thread, Some(&ana), 2, CALM).await;
        assert_eq!(level(&thread), "faint");

        // One that closed and comes back starts again, from its step.
        let anger_key = feeling_key("ilk:ana", "anger", "billing");
        let mut seq = 3;
        while thread.episodes.contains_key(&anger_key) {
            says(&mut thread, Some(&ana), seq, CALM).await;
            seq += 1;
        }
        says(&mut thread, Some(&ana), seq, ANGRY).await;
        let back = feeling(&thread, "ilk:ana", "anger");
        assert_eq!(back.weight, 2.0);
        assert_eq!(back.occurrences, 1);

        let half_life = |affect: &str| 0.5f64.ln() / feeling_decay(affect).ln();
        let context_half_life = 0.5f64.ln() / COGNITION_CONTEXT_DECAY_FACTOR.ln();
        for (affect, inertia) in [("urgency", 2.0), ("anger", 5.0), ("escalation", 10.0)] {
            assert!((half_life(affect) / context_half_life - inertia).abs() < 1e-9);
        }
        assert_eq!(feeling_step(8.0), 2.0);
        assert_eq!(feeling_step(7.0), 1.75);
    }

    /// The agent's own answer shows no feeling and moves none: an agent has no feelings, and the
    /// person's turns are their feelings' clock.
    #[tokio::test]
    async fn the_agents_own_answer_shows_and_moves_no_feeling() {
        let juan = person("ilk:juan", "Juan");
        let mut thread = ThreadCognitionState::default();
        says(&mut thread, Some(&juan), 1, ANGRY).await;
        says(&mut thread, None, 2, ANGRY).await;
        assert_eq!(thread.episodes.len(), 1);
        assert_eq!(feeling(&thread, "ilk:juan", "anger").weight, 2.0);
    }

    fn feeling_state(
        owner: &str,
        affect: &str,
        context: &str,
        weight: f64,
        felt: &str,
    ) -> EpisodeState {
        EpisodeState {
            episode_id: format!("episode:{owner}:{affect}:{context}"),
            owner: person(owner, owner.trim_start_matches("ilk:")),
            affect_id: affect.to_string(),
            context_id: format!("context:{context}"),
            context_label: context.to_string(),
            scope_id: "scope:1".to_string(),
            title: format!("Friction around {context}"),
            summary: "x".repeat(400),
            reason: String::new(),
            base_intensity: 8.0,
            evidence_strength: 9.0,
            evidence_context_ids: Vec::new(),
            evidence_reason_ids: Vec::new(),
            evidence_signals: Vec::new(),
            weight,
            weight_avg_cumulative: weight,
            weight_avg_ema: weight,
            weight_samples: 1,
            occurrences: 1,
            created_at: felt.to_string(),
            last_felt_at: felt.to_string(),
        }
    }

    /// The package carries the feelings, the heaviest first and the most recently felt on a tie, 12
    /// at most, each with its owner, affect, context, level, when last felt and its text cut at 300
    /// characters.
    #[test]
    fn the_package_carries_the_open_feelings_by_weight() {
        let mut thread = ThreadCognitionState::default();
        for index in 0..14u32 {
            let feeling = feeling_state(
                &format!("ilk:p{index}"),
                "anger",
                "billing",
                1.0 + f64::from(index),
                "2026-01-01T00:00:00Z",
            );
            thread.episodes.insert(feeling.episode_id.clone(), feeling);
        }
        let older = feeling_state("ilk:tie", "urgency", "access", 13.5, "2026-01-01T00:00:00Z");
        let newer = feeling_state("ilk:tie", "anger", "access", 13.5, "2026-01-02T00:00:00Z");
        for feeling in [older, newer] {
            thread.episodes.insert(feeling.episode_id.clone(), feeling);
        }

        let package = build_memory_package_for_thread(TEST_THREAD, &thread);
        assert_eq!(package.package_version, MEMORY_PACKAGE_VERSION);
        assert_eq!(package.episodes.len(), COGNITION_PACKAGE_MAX_FEELINGS);
        let first = &package.episodes[0];
        assert_eq!(first.owner.ilk.as_deref(), Some("ilk:p13"));
        assert_eq!(first.weight, 14.0);
        assert_eq!(first.level, "strong");
        assert_eq!(
            first.summary.chars().count(),
            COGNITION_FEELING_TEXT_MAX_CHARS
        );
        assert_eq!(
            (
                package.episodes[1].affect.as_str(),
                package.episodes[2].affect.as_str()
            ),
            ("anger", "urgency"),
            "on a tie, the most recently felt first"
        );
        assert_eq!(
            package
                .truncated
                .as_ref()
                .map(|truncated| truncated.dropped_episodes),
            Some(4)
        );
    }

    /// An item's averages follow its weight over every turn it is open, and its `last_seen_at` stays
    /// at the last turn that named it (FINDINGS A-156).
    #[tokio::test]
    async fn the_averages_follow_the_weight_and_last_seen_stays() {
        let mut thread = ThreadCognitionState::default();
        live_turn(&mut thread, 1, &["billing"], &["inform"], &[]).await;
        live_turn(&mut thread, 2, &["shipping"], &["inform"], &[]).await;
        live_turn(&mut thread, 3, &["shipping"], &["inform"], &[]).await;
        let billing = &thread.contexts["billing"];
        assert_eq!(billing.weight_samples, 3);
        assert!((billing.weight - 0.7225).abs() < 1e-12);
        assert!((billing.weight_avg_cumulative - (1.0 + 0.85 + 0.7225) / 3.0).abs() < 1e-12);
        let ema = 0.25 * 0.7225 + 0.75 * (0.25 * 0.85 + 0.75 * 1.0);
        assert!((billing.weight_avg_ema - ema).abs() < 1e-12);
        assert_eq!(billing.last_seen_at, "2026-01-01T00:01:00Z");
        // A reason named every turn averages its rising weight, no longer the constant score.
        let reason = &thread.reasons["information exchange"];
        assert!((reason.weight_avg_cumulative - (1.0 + 1.75 + 2.3125) / 3.0).abs() < 1e-12);
    }

    /// The narrator reads who spoke, and a turn names whose feelings it moves: a person who wrote to
    /// the agent, not the agent nor a WF; an ILK the identity does not know counts as a person's.
    #[test]
    fn a_turn_names_its_speaker_and_the_owner_of_its_feelings() {
        let facts = |ilk: &str| match ilk {
            "ilk:person" => Some(IlkFacts {
                ilk_type: "human".to_string(),
                name: Some("Ana Pérez".to_string()),
            }),
            "ilk:wf" => Some(IlkFacts {
                ilk_type: "agent".to_string(),
                name: Some("WF.sales@motherbee".to_string()),
            }),
            _ => None,
        };
        let ana = person_says("me cobraron dos veces");
        assert_eq!(
            narrator_speaker(&ana, TEST_AGENT_NODE, facts).as_deref(),
            Some("person Ana Pérez (ilk:person)")
        );
        assert_eq!(
            feeling_owner(&ana, TEST_AGENT_NODE, facts),
            Some(person("ilk:person", "Ana Pérez"))
        );
        let mut answer = agent_says("lo resolvemos");
        answer.meta.dst_ilk = Some("ilk:person".to_string());
        assert_eq!(
            narrator_speaker(&answer, TEST_AGENT_NODE, facts).as_deref(),
            Some("the agent, answering person Ana Pérez (ilk:person)")
        );
        assert_eq!(feeling_owner(&answer, TEST_AGENT_NODE, facts), None);
        let mut wf_text = person_says("Ana no contesta hace 2 días");
        wf_text.meta.context = Some(json!({"writer": {"ilk": "ilk:wf"}}));
        assert_eq!(feeling_owner(&wf_text, TEST_AGENT_NODE, facts), None);
        let unknown = turn_copy("IO.api@motherbee", Some("ilk:new"), text("hola"));
        assert_eq!(
            feeling_owner(&unknown, TEST_AGENT_NODE, facts).and_then(|owner| owner.ilk),
            Some("ilk:new".to_string())
        );
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
                // A closed item leaves the state (docs/cognition-hands-v1.md §11.2).
                let held = [
                    thread.contexts.contains_key("billing"),
                    thread.reasons.contains_key("information exchange"),
                    thread
                        .cooccurrences
                        .contains_key("billing|information exchange"),
                ];
                for (slot, held) in closed.iter_mut().zip(held) {
                    if slot.is_none() && !held {
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

    /// CONFIG_GET names where the turns come from and where its state lives: the router stream and
    /// its own file, with no postgres resource (docs/cognition-hands-v1.md §11.3).
    #[test]
    fn config_get_names_the_stream_and_its_file() {
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
            payload["config"]["state"]["store"],
            json!("/var/lib/fluxbee/test/cognition.db")
        );
        let resources = payload["contract"]["resources"].to_string();
        assert!(!resources.contains("postgres"), "{resources}");
    }

    #[tokio::test]
    async fn a_communications_references_carry_its_agent() {
        let mut thread = ThreadCognitionState::default();
        live_turn(&mut thread, 1, &["billing"], &["inform"], &[]).await;
        assert_eq!(thread.turn_count, 1);
        assert_eq!(
            thread.contexts["billing"].context_id,
            stable_entity_id("context", &[TEST_THREAD, TEST_AGENT, "billing"])
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

    // --- A communication's last turns, taken at intake (docs/reference/cognition.md §3.4) ---

    /// A cognition on a test router, with `region` as its memory region and a file of its own in
    /// `dir`, that knows the ILK of `TEST_AGENT_NODE` as the identity shared memory would answer it.
    fn test_app_state(
        hive_id: &str,
        region: Option<MemoryRegionWriter>,
        dir: &Path,
    ) -> (
        Arc<CognitionAppState>,
        fluxbee_sdk::RouterDispatcherTestHarness,
    ) {
        let node_name = format!("SY.cognition@{hive_id}");
        let (dispatcher, router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            &node_name,
            build_cognition_rpc_profile().expect("profile"),
        );
        let agent = AgentIdentity {
            ilk: TEST_AGENT.to_string(),
            tenant: None,
            seq: 0,
            read_at: std::time::Instant::now(),
        };
        let app_state = Arc::new(CognitionAppState {
            hive_id: hive_id.to_string(),
            node_name: node_name.clone(),
            self_ilk_id: "ilk:cognition".to_string(),
            runtime_paths: test_runtime_paths(),
            store: CognitionStore::open(dir).expect("a test file"),
            control_state: Arc::new(Mutex::new(test_control_state())),
            runtime_state: Arc::new(Mutex::new(CognitionRuntimeState::default())),
            thread_states: Arc::new(Mutex::new(HashMap::new())),
            memory_region: Arc::new(Mutex::new(region)),
            worker_turn: Mutex::new(None),
            unsaved: std::sync::Mutex::new(HashSet::new()),
            vault: VaultClient::for_primary(
                Arc::clone(&dispatcher),
                VaultCallerOwned::new("ilk:cognition".to_string(), node_name),
            ),
            dispatcher,
            agent_ilks: Mutex::new(HashMap::from([(TEST_AGENT_NODE.to_string(), agent)])),
            usage: UsageRecorder::disabled(),
            stream_seen: Mutex::new(HashMap::new()),
        });
        (app_state, router)
    }

    /// A `STREAM_BATCH` from its hive's router stream, with a `cognition.turns` record of
    /// `TEST_AGENT_NODE` for each `(seq, ts_ms, message)`.
    fn turns_batch(hive_id: &str, records: &[(u64, u64, &Message)]) -> Message {
        let stream = format!("RT.gateway.stream@{hive_id}");
        let records = records
            .iter()
            .map(|(seq, ts_ms, message)| fluxbee_sdk::stream::StreamRecord {
                seq: *seq,
                ts_ms: *ts_ms,
                subject: SUBJECT_COGNITION_TURNS.to_string(),
                producer: TEST_AGENT_NODE.to_string(),
                payload: serde_json::to_value(message).expect("message"),
            })
            .collect();
        Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some(stream.clone()),
                dst: Destination::Unicast(format!("SY.cognition@{hive_id}")),
                ttl: 1,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_STREAM_BATCH.to_string()),
                ..Meta::default()
            },
            payload: serde_json::to_value(StreamBatch {
                stream,
                epoch: 7,
                records,
            })
            .expect("batch"),
        }
    }

    /// The package the region holds for `TEST_THREAD` and `TEST_AGENT_NODE`, as its router reads
    /// it at delivery.
    async fn region_package(app_state: &CognitionAppState) -> Option<MemoryPackage> {
        let region = app_state.memory_region.lock().await;
        region
            .as_ref()?
            .read_snapshot()
            .ok()?
            .threads
            .into_iter()
            .find(|entry| entry.thread_id == TEST_THREAD && entry.agent_node == TEST_AGENT_NODE)
            .map(|entry| entry.package)
    }

    /// A memory region of its own for a test cognition, and how to remove it.
    fn test_region(prefix: &str) -> (String, MemoryRegionWriter, impl Fn()) {
        let hive_id = format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let shm_name = memory_shm_name_for_hive(&hive_id).expect("memory shm name");
        let unlink = {
            let shm_name = shm_name.clone();
            move || {
                let name = std::ffi::CString::new(shm_name.as_str()).expect("shm name");
                let _ = nix::sys::mman::shm_unlink(name.as_c_str());
            }
        };
        unlink();
        let region = MemoryRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
            .expect("memory region");
        (hive_id, region, unlink)
    }

    /// The next message the test router got from the cognition.
    async fn next_outgoing(router: &mut fluxbee_sdk::RouterDispatcherTestHarness) -> Message {
        router
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("a message from the cognition")
    }

    /// The intake, with the worker held back (nothing takes its queue): the file and the region carry
    /// a turn as soon as its batch is taken, and the agent's answer right after it, so the next
    /// message delivered to the agent has both. Each turn then waits in the worker's queue, and
    /// each batch is acked. Nothing goes to SY.storage (docs/cognition-hands-v1.md §11.3).
    #[tokio::test]
    async fn the_intake_puts_a_turn_in_the_file_and_the_region_before_the_worker_takes_it() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogtest");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let question = person_says("How much is the Pro plan?");
        let batch = turns_batch(&hive_id, &[(1, TEST_TS, &question)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        let ack = next_outgoing(&mut router).await;
        assert_eq!(ack.meta.msg.as_deref(), Some(MSG_STREAM_ACK));
        assert_eq!(ack.payload["up_to_seq"], json!(1));
        let queued = turn_rx.try_recv().expect("the turn waits for the worker");
        assert_eq!(queued.message.routing.trace_id, question.routing.trace_id);
        let package = region_package(&app_state).await.expect("the package");
        assert_eq!(texts(&package.turns), ["How much is the Pro plan?"]);
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("in the file");
        assert_eq!(texts(&stored.turns), ["How much is the Pro plan?"]);
        assert_eq!(stored.turn_count, 0, "only the intake ran");

        let answer = agent_says("USD 49 a license. How many do you want?");
        let batch = turns_batch(&hive_id, &[(2, TEST_TS + 3_000, &answer)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        let ack = next_outgoing(&mut router).await;
        assert_eq!(ack.meta.msg.as_deref(), Some(MSG_STREAM_ACK));
        assert_eq!(ack.payload["up_to_seq"], json!(2));
        assert!(
            router
                .next_outgoing_within(Duration::from_millis(200))
                .await
                .is_none(),
            "nothing but the acks"
        );
        let queued = turn_rx.try_recv().expect("the turn waits for the worker");
        assert_eq!(queued.message.routing.trace_id, answer.routing.trace_id);
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("in the file");
        assert_eq!(
            texts(&stored.turns),
            [
                "How much is the Pro plan?",
                "USD 49 a license. How many do you want?"
            ]
        );

        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(package.package_version, MEMORY_PACKAGE_VERSION);
        assert_eq!(
            package.turns,
            vec![
                MemoryTurn {
                    direction: MemoryTurnDirection::In,
                    src_ilk: Some("ilk:person".to_string()),
                    via: Some("IO.api@motherbee".to_string()),
                    writer: Some(MemoryTurnParty {
                        ilk: Some("ilk:person".to_string()),
                        ..MemoryTurnParty::default()
                    }),
                    to: None,
                    text: "How much is the Pro plan?".to_string(),
                    attachments: 0,
                    choice: None,
                    ts: "2026-10-30T12:00:00.000Z".to_string(),
                },
                MemoryTurn {
                    direction: MemoryTurnDirection::Out,
                    src_ilk: Some(TEST_AGENT.to_string()),
                    via: None,
                    writer: None,
                    to: None,
                    text: "USD 49 a license. How many do you want?".to_string(),
                    attachments: 0,
                    choice: None,
                    ts: "2026-10-30T12:00:03.000Z".to_string(),
                },
            ]
        );
        assert_eq!(app_state.runtime_state.lock().await.intake_turns_total, 2);
        unlink();
    }

    /// A host reboot empties the memory region. The file keeps the communications, and the start
    /// writes the most recent back into the region before the first batch, so the agent's first
    /// message after the reboot carries its memory (docs/cognition-hands-v1.md §11.3).
    #[tokio::test]
    async fn after_a_reboot_the_region_comes_back_from_the_file() {
        let dir = TestDir::new();
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        {
            let (hive_id, region, unlink) = test_region("cogboot");
            let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
            let said = [person_says("Hello"), agent_says("Hello, how can I help?")];
            for (seq, message) in (1u64..).zip(&said) {
                let batch = turns_batch(&hive_id, &[(seq, TEST_TS + seq * 1_000, message)]);
                take_stream_batch(&app_state, &turn_tx, &batch).await;
                next_outgoing(&mut router).await;
            }
            // The reboot: the region is gone.
            unlink();
        }
        let (hive_id, region, unlink) = test_region("cogboot");
        let (app_state, _router) = test_app_state(&hive_id, Some(region), &dir.0);
        assert!(
            region_package(&app_state).await.is_none(),
            "an empty region"
        );
        load_most_recent(&app_state).await;
        let package = region_package(&app_state)
            .await
            .expect("the package, back from the file");
        assert_eq!(texts(&package.turns), ["Hello", "Hello, how can I help?"]);
        assert_eq!(
            app_state.runtime_state.lock().await.loaded_at_start_total,
            1
        );
        unlink();
    }

    /// A turn with no person at its other end is not analyzed (docs/cognition-hands-v1.md §6): the
    /// intake writes it to the file with its communication's turns, and the worker counts it and
    /// calls no model. Here a WF's own text: its writer is the WF's node.
    #[tokio::test]
    async fn a_turn_with_no_person_at_its_other_end_is_not_analyzed() {
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cognoperson", None, &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let mut reminder = turn_copy(
            "WF.sales@motherbee",
            Some("ilk:person"),
            text("Ana has not answered in 2 days: send a reminder"),
        );
        reminder.meta.context = Some(json!({"writer": {"name": "WF.sales@motherbee"}}));
        let batch = turns_batch("cognoperson", &[(1, TEST_TS, &reminder)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        let ack = next_outgoing(&mut router).await;
        assert_eq!(ack.meta.msg.as_deref(), Some(MSG_STREAM_ACK));

        handle_turn(
            turn_rx.try_recv().expect("the queued turn"),
            Arc::clone(&app_state),
        )
        .await;
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("in the file");
        assert_eq!(
            texts(&stored.turns),
            ["Ana has not answered in 2 days: send a reminder"]
        );
        assert_eq!(stored.turn_count, 0);
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.turns_not_analyzed_total, 1);
        assert_eq!(state.semantic_tagger_calls_total, 0);
    }

    /// A turn the agent got names its writer, and one it sent whom it answered, each with the name
    /// and the kind the identity gives (docs/cognition-hands-v1.md §6). A message with no writer
    /// stands for its `src_ilk`.
    #[test]
    fn a_turn_names_its_writer_and_an_answer_whom_it_went_to() {
        let known = |ilk: &str| match ilk {
            "ilk:ana" => Some(IlkFacts {
                ilk_type: "human".to_string(),
                name: Some("Ana Pérez".to_string()),
            }),
            "ilk:wf" => Some(IlkFacts {
                ilk_type: "agent".to_string(),
                name: Some("WF.sales@motherbee".to_string()),
            }),
            "ilk:invoice" => Some(IlkFacts {
                ilk_type: "agent".to_string(),
                name: Some("AI.invoice@motherbee".to_string()),
            }),
            _ => None,
        };
        let party = |ilk: Option<&str>, name: Option<&str>, kind: Option<&str>| {
            Some(MemoryTurnParty {
                ilk: ilk.map(ToString::to_string),
                name: name.map(ToString::to_string),
                kind: kind.map(ToString::to_string),
            })
        };
        let written = |writer: Value| {
            let mut message = supervisor_says("said");
            message.meta.src_ilk = Some("ilk:ana".to_string());
            message.meta.context.as_mut().expect("context")["writer"] = writer;
            recent_turn_of(&message, TEST_AGENT_NODE, TEST_AGENT, TEST_TS, known)
                .expect("a turn")
                .turn
        };

        let ana = written(json!({"ilk": "ilk:ana"}));
        assert_eq!(
            ana.writer,
            party(Some("ilk:ana"), Some("Ana Pérez"), Some("person"))
        );
        assert_eq!(ana.to, None);
        assert_eq!(
            written(json!({"ilk": "ilk:wf", "name": "WF.sales@motherbee"})).writer,
            party(Some("ilk:wf"), Some("WF.sales@motherbee"), Some("workflow"))
        );
        assert_eq!(
            written(json!({"ilk": "ilk:invoice"})).writer,
            party(
                Some("ilk:invoice"),
                Some("AI.invoice@motherbee"),
                Some("agent")
            )
        );
        // A name the message gives stays; an ILK the identity does not know has no kind.
        assert_eq!(
            written(json!({"ilk": "ilk:temp", "name": "Juan"})).writer,
            party(Some("ilk:temp"), Some("Juan"), None)
        );
        let unnamed = recent_turn_of(
            &person_says("hi"),
            TEST_AGENT_NODE,
            TEST_AGENT,
            TEST_TS,
            known,
        )
        .expect("a turn")
        .turn;
        assert_eq!(unnamed.writer, party(Some("ilk:person"), None, None));

        let mut answer = agent_says("Your credit note is on its way");
        answer.meta.dst_ilk = Some("ilk:ana".to_string());
        let answer = recent_turn_of(&answer, TEST_AGENT_NODE, TEST_AGENT, TEST_TS, known)
            .expect("a turn")
            .turn;
        assert_eq!(answer.writer, None);
        assert_eq!(
            answer.to,
            party(Some("ilk:ana"), Some("Ana Pérez"), Some("person"))
        );
    }

    /// Cognition analyzes a turn with a person at its other end (docs/cognition-hands-v1.md §6): one
    /// a person wrote to the agent, or the agent's answer to a person. An ILK the identity does not
    /// know yet is taken as a person's.
    #[test]
    fn only_a_turn_with_a_person_at_its_other_end_is_analyzed() {
        let known = |ilk: &str| {
            let ilk_type = match ilk {
                "ilk:ana" => "human",
                "ilk:sales" | "ilk:wf" => "agent",
                "ilk:frontdesk" => "system",
                _ => return None,
            };
            Some(IlkFacts {
                ilk_type: ilk_type.to_string(),
                name: None,
            })
        };
        let got = |writer: Option<Value>, src_ilk: Option<&str>| {
            let mut message = turn_copy("WF.sales@motherbee", src_ilk, text("said"));
            message.meta.context = writer.map(|writer| json!({"writer": writer}));
            person_at_other_end(&message, TEST_AGENT_NODE, known)
        };
        assert!(got(Some(json!({"ilk": "ilk:ana"})), Some("ilk:ana")));
        assert!(got(Some(json!({"ilk": "ilk:new"})), Some("ilk:new")));
        assert!(!got(Some(json!({"ilk": "ilk:wf"})), Some("ilk:ana")));
        assert!(!got(
            Some(json!({"ilk": "ilk:sales", "name": "AI.sales@motherbee"})),
            Some("ilk:sales")
        ));
        assert!(!got(Some(json!({"ilk": "ilk:frontdesk"})), Some("ilk:ana")));
        assert!(!got(
            Some(json!({"name": "WF.sales@motherbee"})),
            Some("ilk:ana")
        ));
        assert!(got(None, Some("ilk:ana")));
        assert!(got(None, None));
        assert!(!got(None, Some("ilk:sales")));

        let sent = |dst_ilk: Option<&str>| {
            let mut message = agent_says("answered");
            message.meta.dst_ilk = dst_ilk.map(ToString::to_string);
            person_at_other_end(&message, TEST_AGENT_NODE, known)
        };
        assert!(sent(Some("ilk:ana")));
        assert!(sent(Some("ilk:new")));
        assert!(sent(None));
        assert!(!sent(Some("ilk:sales")));
        assert!(!sent(Some("ilk:frontdesk")));
    }

    /// A communication keeps its last 20 turns in the order its router took them, each with who
    /// spoke: what the agent received, with its sender and the node it came through (the IO node
    /// at the head of a WF's trail, else the sender), and what it sent. The 21st pushes out the
    /// oldest.
    #[test]
    fn a_communication_keeps_its_last_twenty_turns_with_who_spoke() {
        let store = test_store();
        let mut threads = HashMap::new();
        for n in 0..21u64 {
            let content = format!("turn {n}");
            let message = match n % 3 {
                0 => person_says(&content),
                1 => agent_says(&content),
                _ => supervisor_says(&content),
            };
            let turn = taken(&message, TEST_TS + n * 1_000);
            hold(&mut threads, &store, vec![turn]);
        }
        let turns = &threads[&key(TEST_THREAD)].turns;
        let expected: Vec<String> = (1..=20).map(|n| format!("turn {n}")).collect();
        assert_eq!(texts(turns), expected);
        let who = |turn: &MemoryTurn| {
            (
                turn.direction,
                turn.src_ilk.clone(),
                turn.via.clone(),
                turn.ts.clone(),
            )
        };
        assert_eq!(
            who(&turns[0]),
            (
                MemoryTurnDirection::Out,
                Some(TEST_AGENT.to_string()),
                None,
                "2026-10-30T12:00:01.000Z".to_string()
            )
        );
        assert_eq!(
            who(&turns[1]),
            (
                MemoryTurnDirection::In,
                Some("ilk:supervisor".to_string()),
                Some("IO.slack.supervisores@motherbee".to_string()),
                "2026-10-30T12:00:02.000Z".to_string()
            )
        );
        assert_eq!(
            who(&turns[2]),
            (
                MemoryTurnDirection::In,
                Some("ilk:person".to_string()),
                Some("IO.api@motherbee".to_string()),
                "2026-10-30T12:00:03.000Z".to_string()
            )
        );
        // A batch sent again adds nothing.
        let again = taken(&person_says("turn 18"), TEST_TS + 18_000);
        hold(&mut threads, &store, vec![again]);
        assert_eq!(
            threads[&key(TEST_THREAD)].turns.len(),
            COGNITION_RECENT_TURNS
        );
    }

    /// A turn's text is cut at 2,000 characters. A turn with only files is kept, with no text and
    /// its files counted, so the agent's answer to it keeps its question; one with neither text nor
    /// files is left out.
    #[test]
    fn a_turn_keeps_its_text_cut_and_counts_its_files() {
        let long = taken(&person_says(&"á".repeat(2_500)), TEST_TS).turn;
        assert_eq!(long.text.chars().count(), COGNITION_TURN_TEXT_MAX_CHARS);
        assert!(long.text.ends_with('…'));

        let photo = json!({
            "type": "blob_ref",
            "blob_name": "photo_0123456789abcdef.png",
            "size": 10,
            "mime": "image/png",
            "filename_original": "photo.png",
            "spool_day": "2026-10-30"
        });
        let only_files = turn_copy(
            "IO.api@motherbee",
            Some("ilk:person"),
            json!({"type": "text", "content": "", "attachments": [photo]}),
        );
        let files = taken(&only_files, TEST_TS).turn;
        assert_eq!((files.text.as_str(), files.attachments), ("", 1));

        let no_text = turn_copy(
            "IO.api@motherbee",
            Some("ilk:person"),
            json!({"type": "event", "name": "typing"}),
        );
        assert!(recent_turn_of(&no_text, TEST_AGENT_NODE, TEST_AGENT, TEST_TS, no_facts).is_none());
    }

    /// Text of more bytes a character keeps fewer turns: they stay under the bytes a package may
    /// take, and the newest always stays. Twenty turns of 2,000 characters of Latin
    /// text all fit.
    #[test]
    fn the_last_turns_stay_under_their_byte_bound() {
        let size = |turns: &[MemoryTurn]| {
            turns
                .iter()
                .map(|turn| serde_json::to_vec(turn).expect("turn").len() + 1)
                .sum::<usize>()
        };
        let mut wide = Vec::new();
        for n in 0..20u64 {
            let said = person_says(&format!("{n} {}", "字".repeat(1_990)));
            push_recent_turn(&mut wide, taken(&said, TEST_TS + n * 1_000).turn);
        }
        assert!(wide.len() < COGNITION_RECENT_TURNS);
        assert!(size(&wide) <= COGNITION_RECENT_TURNS_MAX_BYTES);
        assert!(wide.last().expect("a turn").text.starts_with("19 "));

        let mut latin = Vec::new();
        for n in 0..20u64 {
            let said = person_says(&"x".repeat(COGNITION_TURN_TEXT_MAX_CHARS));
            push_recent_turn(&mut latin, taken(&said, TEST_TS + n * 1_000).turn);
        }
        assert_eq!(latin.len(), COGNITION_RECENT_TURNS);
        assert!(size(&latin) <= COGNITION_RECENT_TURNS_MAX_BYTES);
    }

    /// Two agents on one thread are two communications: each keeps the turns its own router took,
    /// and the region carries each under its agent node.
    #[test]
    fn two_agents_on_one_thread_keep_their_own_turns() {
        let to_sales = person_says("I want 10 Pro licenses");
        let to_invoice = turn_copy(
            "WF.sales@motherbee",
            Some("ilk:person"),
            text("Invoice 10 Pro licenses"),
        );
        let store = test_store();
        let mut threads = HashMap::new();
        hold(
            &mut threads,
            &store,
            vec![
                recent_turn_of(&to_sales, TEST_AGENT_NODE, "ilk:sales", TEST_TS, no_facts)
                    .expect("a turn"),
                recent_turn_of(
                    &to_invoice,
                    "AI.invoice@motherbee",
                    "ilk:invoice",
                    TEST_TS,
                    no_facts,
                )
                .expect("a turn"),
            ],
        );
        let hot_set = build_memory_hot_set_snapshot(&threads, &[]).expect("hot set");
        assert_eq!(hot_set.snapshot.threads.len(), 2);
        for (node, agent_ilk, said) in [
            (TEST_AGENT_NODE, "ilk:sales", "I want 10 Pro licenses"),
            (
                "AI.invoice@motherbee",
                "ilk:invoice",
                "Invoice 10 Pro licenses",
            ),
        ] {
            let entry = hot_set
                .snapshot
                .threads
                .iter()
                .find(|entry| entry.agent_node == node)
                .expect("its entry");
            assert_eq!(entry.thread_id, TEST_THREAD);
            assert_eq!(entry.agent_ilk, agent_ilk);
            assert_eq!(texts(&entry.package.turns), [said]);
        }
    }

    /// The turns go with their communication into its file and come back from it. After a restart
    /// the intake takes a turn of a communication it does not hold from the file, after its stored
    /// turns, and the worker's turn, written back after its model calls, keeps the turns the intake
    /// took meanwhile.
    #[test]
    fn the_turns_survive_the_file_and_the_workers_write_back() {
        let store = test_store();
        let mut threads = HashMap::new();
        let said = [
            person_says("How much is Pro?"),
            agent_says("USD 49 a license."),
            supervisor_says("20% approved"),
        ];
        for (n, message) in (0u64..).zip(&said) {
            hold(
                &mut threads,
                &store,
                vec![taken(message, TEST_TS + n * 1_000)],
            );
        }
        let held = threads[&key(TEST_THREAD)].clone();
        assert_eq!(held.src_ilk.as_deref(), Some("ilk:supervisor"));
        store.save([(&key(TEST_THREAD), &held)]).expect("written");

        // After a restart the intake takes a turn: the communication comes back from the file.
        let mut restarted = HashMap::new();
        let next = taken(&person_says("Go ahead, invoice me"), TEST_TS + 10_000);
        hold(&mut restarted, &store, vec![next]);
        let loaded = restarted[&key(TEST_THREAD)].clone();
        assert_eq!(
            texts(&loaded.turns),
            [
                "How much is Pro?",
                "USD 49 a license.",
                "20% approved",
                "Go ahead, invoice me"
            ]
        );
        assert_eq!(loaded.src_ilk.as_deref(), Some("ilk:person"));

        // The worker's turn runs on a copy; the intake takes the agent's answer meanwhile.
        let mut updated = loaded.clone();
        updated.turn_count += 1;
        let answer = taken(&agent_says("Done, invoicing you."), TEST_TS + 12_000);
        hold(&mut restarted, &store, vec![answer]);
        write_back_turn(&mut restarted, &key(TEST_THREAD), updated);
        let written = &restarted[&key(TEST_THREAD)];
        assert_eq!(written.turn_count, loaded.turn_count + 1);
        assert_eq!(written.turns.len(), 5);
        assert_eq!(
            written.turns.last().map(|turn| turn.text.as_str()),
            Some("Done, invoicing you.")
        );
        assert_eq!(written.src_ilk.as_deref(), Some(TEST_AGENT));
    }

    /// An intake batch never drops the communication the worker is on, even from a full region:
    /// its turn comes back only after its model calls. And the worker's write-back keeps the turns
    /// of its copy when what is held was started again meanwhile.
    #[tokio::test]
    async fn an_intake_batch_keeps_the_communication_the_worker_is_on() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogkeep");
        let (app_state, _router) = test_app_state(&hive_id, Some(region), &dir.0);
        // Five communications of a fifth of the region each: four fit. The worker is on the
        // oldest, which the ranking would drop first.
        {
            let mut threads = app_state.thread_states.lock().await;
            for seq in 1..=5u64 {
                let mut thread = thread_of_a_fifth_of_the_region(seq, &["billing"]).await;
                thread.agent_node = TEST_AGENT_NODE.to_string();
                threads.insert(key(&format!("thread:{seq}")), thread);
            }
        }
        *app_state.worker_turn.lock().await = Some(key("thread:1"));
        let mut newest = person_says("one more");
        newest.meta.thread_id = Some("thread:5".to_string());
        take_recent_turns(&app_state, vec![taken(&newest, TEST_TS)]).await;
        let threads = app_state.thread_states.lock().await;
        assert!(threads.contains_key(&key("thread:1")), "the worker's turn");
        assert!(threads.contains_key(&key("thread:5")), "the batch's");
        assert_eq!(threads.len(), 4);
        drop(threads);
        unlink();

        // What is held was started again (let go, then a new turn): the copy's turns stay.
        let store = test_store();
        let mut held = HashMap::new();
        let first = taken(&person_says("first"), TEST_TS);
        let mut updated = ThreadCognitionState {
            turn_count: 7,
            ..ThreadCognitionState::default()
        };
        updated.turns.push(first.turn);
        hold(
            &mut held,
            &store,
            vec![taken(&person_says("later"), TEST_TS + 1)],
        );
        write_back_turn(&mut held, &key(TEST_THREAD), updated);
        let written = &held[&key(TEST_THREAD)];
        assert_eq!(texts(&written.turns), ["first", "later"]);
        assert_eq!(written.turn_count, 7);
    }

    /// U+0000 leaves a turn's text and names.
    #[test]
    fn a_turn_keeps_no_u0000() {
        let mut message = person_says("in\u{0}voice");
        message.meta.src_ilk = Some("ilk:per\u{0}son".to_string());
        let turn = taken(&message, TEST_TS).turn;
        assert_eq!(turn.text, "invoice");
        assert_eq!(turn.src_ilk.as_deref(), Some("ilk:person"));
    }

    /// An answer that chose from a WF's menu keeps its choice in its turn, even with a blank reply:
    /// it leaves no text, and the WF may not have forwarded it, so the choice says what it did.
    #[test]
    fn an_answer_keeps_its_menu_choice() {
        let mut chose = agent_says("");
        chose.meta.context = Some(json!({"evaluation": {
            "reply": "",
            "option": "invoice",
            "params": {"product": "Pro", "quantity": 10}
        }}));
        let turn = taken(&chose, TEST_TS).turn;
        assert_eq!(turn.direction, MemoryTurnDirection::Out);
        assert_eq!(turn.text, "");
        assert_eq!(
            turn.choice,
            Some(MemoryTurnChoice {
                option: "invoice".to_string(),
                params: r#"{"product":"Pro","quantity":10}"#.to_string(),
            })
        );

        // Within the turn's caps; an option without params keeps none.
        let mut long = agent_says("Let me check.");
        long.meta.context = Some(json!({"evaluation": {
            "reply": "Let me check.",
            "option": "consult",
            "params": {"question": "x".repeat(3_000)}
        }}));
        let choice = taken(&long, TEST_TS).turn.choice.expect("a choice");
        assert_eq!(choice.params.chars().count(), COGNITION_TURN_TEXT_MAX_CHARS);
        let mut bare = agent_says("Here you are.");
        bare.meta.context = Some(
            json!({"evaluation": {"reply": "Here you are.", "option": "answer", "params": {}}}),
        );
        assert_eq!(
            taken(&bare, TEST_TS).turn.choice,
            Some(MemoryTurnChoice {
                option: "answer".to_string(),
                params: String::new(),
            })
        );

        // What the agent received carries no choice of its own, and a blank answer without one is
        // left out.
        let mut received = person_says("ok");
        received.meta.context = chose.meta.context.clone();
        assert_eq!(taken(&received, TEST_TS).turn.choice, None);
        assert!(recent_turn_of(
            &agent_says(""),
            TEST_AGENT_NODE,
            TEST_AGENT,
            TEST_TS,
            no_facts
        )
        .is_none());
    }
}
