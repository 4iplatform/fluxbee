// STATUS's `turns_consumer` is one `json!` object past the macro's default depth.
#![recursion_limit = "256"]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_ai_sdk::{
    AiAccount, AiProvider, EffectiveAiEngine, HiveAiConfig, TenantSource, UsageRecorder,
};
use fluxbee_sdk::message_trace;
use fluxbee_sdk::message_writer::{self, IlkFacts};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, FeelingSummary, MemoryContextSummary, MemoryPackage,
    MemoryPackageTruncated, MemoryReasonSummary, MemorySummary, MemoryTurn, MemoryTurnChoice,
    MemoryTurnDirection, MemoryTurnParty, Message, Meta, NewsSummary, Routing,
    VaultSecretChangedPayload, VaultSecretInterest, MEMORY_PACKAGE_VERSION,
    MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
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
use json_router::ai_settings;
use json_router::shm::{
    memory_shm_name_for_hive, MemoryRegionWriter, MemoryShmSnapshot, MemoryShmThreadEntry,
    MEMORY_MAX_DATA_SIZE,
};

#[path = "sy_cognition/episodes.rs"]
mod episodes;
#[path = "sy_cognition/narrative_summarizer_ai.rs"]
mod narrative_summarizer_ai;
#[path = "sy_cognition/semantic_tagger_ai.rs"]
mod semantic_tagger_ai;
#[path = "sy_cognition/state_store.rs"]
mod state_store;

use narrative_summarizer_ai::{
    run_narrative_summarizer_ai, FeelingNarrative, NarrativeSummaries, NarrativeSummarizerAiInput,
};
use semantic_tagger_ai::run_semantic_tagger_ai;
use state_store::CognitionStore;

type CognitionError = Box<dyn std::error::Error + Send + Sync>;

const COGNITION_NODE_BASE_NAME: &str = "SY.cognition";
const COGNITION_NODE_VERSION: &str = "2.0";
const COGNITION_CONFIG_SCHEMA_VERSION: u32 = 1;
/// The system message SY.admin sends to have this cognition drop everything it keeps
/// (`cognition_clear`, FINDINGS A-160), and its answer.
const MSG_COGNITION_CLEAR: &str = "COGNITION_CLEAR";
const MSG_COGNITION_CLEAR_RESPONSE: &str = "COGNITION_CLEAR_RESPONSE";
/// The one sender a clear is taken from, as its router names it: SY.admin on the motherbee, the
/// node that commands the edges too.
const COGNITION_CLEAR_AUTHORITY: &str = json_router::router::system_policy::EDGE_CONTROL_AUTHORITY;
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
/// The affects a turn can show its writer feels: the feeling gate's words (`build_feeling_candidate`).
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
    /// The provider gave no answer (`ModelFailure::Provider`).
    provider_error: bool,
    /// The episode the narrator proposed, for the gate (docs/cognition-hands-v1.md §12.4).
    episode: Option<episodes::EpisodeProposal>,
}

/// What a failed model call of cognition's was, for its counters (FINDINGS A-184).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelFailure {
    /// The provider gave no answer: an error status after its retries, no connection, none in
    /// time, an answer with no text, or the model's refusal. Cognition does not judge it: it is
    /// no invalid output.
    Provider,
    /// The model answered, and its answer is not the object asked for: no JSON, other JSON, or
    /// one cut at its token limit.
    InvalidOutput,
    /// Anything else.
    Other,
}

fn model_failure(err: &fluxbee_ai_sdk::AiSdkError) -> ModelFailure {
    if err.is_provider_failure() {
        return ModelFailure::Provider;
    }
    match err {
        fluxbee_ai_sdk::AiSdkError::Json(_)
        | fluxbee_ai_sdk::AiSdkError::Protocol(_)
        | fluxbee_ai_sdk::AiSdkError::Truncated { .. }
        | fluxbee_ai_sdk::AiSdkError::InvalidStructuredOutput { .. } => ModelFailure::InvalidOutput,
        _ => ModelFailure::Other,
    }
}

/// What the narrator needs to propose an episode (docs/cognition-hands-v1.md §12.4): whom the turn
/// is to, and the habit of each lens among the turn's people.
#[derive(Debug, Clone, Default)]
struct EpisodeAsk {
    counterpart: Option<String>,
    habits: Vec<(&'static str, u32)>,
}

/// The narrative step of a turn (memory and feeling text). Production asks the AI; tests
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
    /// Where the provider and the model came from, read at the start
    /// (docs/hive-config-update-v1.md §4): the installation file, with its version and hash, or
    /// hive.yaml while the hive holds none (`ai_settings::ai_source_json`). Null when they could
    /// not be read.
    ai_settings_source: Value,
    /// Why the `ai` settings cannot be used, when they cannot: an installation file no reader can
    /// use (never a fallback to hive.yaml), or hive.yaml's `ai` while the hive holds none. Every
    /// turn then skips its semantic work with this reason, as without a key.
    ai_settings_error: Option<String>,
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
    active_feelings_total: u64,
    /// The communications loaded from the file at start (docs/cognition-hands-v1.md §11.3).
    loaded_at_start_total: u64,
    /// Turns whose agent node has no ILK in the identity shared memory: no memory for them.
    no_agent_ilk_total: u64,
    /// Turns the intake added to their communication's last turns, ahead of the worker.
    intake_turns_total: u64,
    /// Turns the worker did not analyze: no person at their other end (docs/cognition-hands-v1.md
    /// §6). They stay among their communication's last turns.
    turns_not_analyzed_total: u64,
    /// The agents' refusals of turns they never ran (`ai_runtime_busy`, FINDINGS A-292), the turns
    /// those refusals dropped from their communications' last turns, and the refused turns the
    /// worker did not analyze, or whose analysis it dropped.
    refusals_total: u64,
    refused_turns_dropped_total: u64,
    refused_turns_not_analyzed_total: u64,
    /// The person's turns the worker waited on for their agent's answer or refusal before it went
    /// on with them (`wait_for_the_agent`, FINDINGS A-332), and those of them whose wait ran its
    /// whole grace with neither. A wait is counted only when the answer was not there at its
    /// first look, and a cognition that cannot call a model waits too.
    agent_waits_total: u64,
    agent_waits_grace_over_total: u64,
    /// Records of a stream batch at or below the last seq taken for that stream and epoch.
    stream_repeats_skipped_total: u64,
    /// Communications loaded from the file after start, and the loads that failed: a turn whose
    /// communication could not load is skipped.
    store_loads_total: u64,
    store_load_failures_total: u64,
    /// Communications with no agent the first agent on their thread took (§12.6).
    conversations_taken_total: u64,
    /// Communications written to the file, and the writes that failed.
    store_writes_total: u64,
    store_write_failures_total: u64,
    /// The turns a WF relayed between people (docs/cognition-hands-v1.md §12.4), and those taken
    /// twice, once for an agent and once relayed, analyzed once.
    relay_turns_total: u64,
    duplicate_turns_skipped_total: u64,
    /// The episodes: kept now, made here, proposed and refused by the gate, taken from other hives,
    /// and refused from them.
    episodes_live_total: u64,
    episodes_made_total: u64,
    episodes_gated_out_total: u64,
    episodes_received_total: u64,
    episodes_refused_total: u64,
    /// The clears it took (`COGNITION_CLEAR`), and when the last one came.
    clears_total: u64,
    last_clear_at: Option<String>,
    shm_hot_threads_total: u64,
    shm_pruned_threads_total: u64,
    shm_payload_bytes: u64,
    last_shm_sync_status: Option<String>,
    last_shm_sync_at: Option<String>,
    semantic_tagger_calls_total: u64,
    semantic_tagger_failures_total: u64,
    semantic_tagger_invalid_outputs_total: u64,
    semantic_tagger_provider_errors_total: u64,
    last_semantic_model: Option<String>,
    last_semantic_impl: Option<String>,
    narrative_summarizer_calls_total: u64,
    narrative_summarizer_failures_total: u64,
    narrative_summarizer_invalid_outputs_total: u64,
    narrative_summarizer_provider_errors_total: u64,
    last_narrative_model: Option<String>,
}

impl CognitionRuntimeState {
    /// After a clear: it holds nothing, and what its last turn left here names a communication the
    /// clear dropped. The totals stay: they count what it did.
    fn cleared(&mut self, at: String) {
        self.last_trace_id = None;
        self.last_thread_id = None;
        self.last_src_ilk = None;
        self.last_ich = None;
        self.last_tags.clear();
        self.last_reason_signals_canonical.clear();
        self.last_reason_signals_extra.clear();
        self.open_contexts_total = 0;
        self.open_reasons_total = 0;
        self.open_cooccurrences_total = 0;
        self.active_threads_total = 0;
        self.active_scopes_total = 0;
        self.active_memories_total = 0;
        self.active_feelings_total = 0;
        self.clears_total = self.clears_total.saturating_add(1);
        self.last_clear_at = Some(at);
    }
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
    /// The communications with no agent the first agent on their thread took, and the rows of them
    /// the file still has (`Takes`). Locked under `thread_states`, never before it.
    takes: std::sync::Mutex<Takes>,
    /// The turns its agents refused without running them (`Refusals`, FINDINGS A-292). Locked under
    /// `thread_states` and after `takes`, or alone; never before either.
    refusals: std::sync::Mutex<Refusals>,
    /// The messages its agents answered or refused lately (`Answers`, FINDINGS A-332), and the
    /// worker's wake when the intake takes more (`wait_for_the_agent`). Locked alone.
    answers: std::sync::Mutex<Answers>,
    agents_answered: Notify,
    vault: VaultClient,
    dispatcher: Arc<RouterDispatcher>,
    /// Agent node → its ILK and tenant, read from the identity shared memory.
    agent_ilks: Mutex<HashMap<String, AgentIdentity>>,
    /// Where each model call's usage record goes (docs/router-stream-v1.md §7.2).
    usage: UsageRecorder,
    /// The last record seq taken from each stream, by stream name and epoch.
    stream_seen: Mutex<HashMap<(String, u64), u64>>,
    /// The episodes this cognition keeps, the public news of every hive (docs/cognition-hands-v1.md
    /// §12.4): the same its file keeps.
    episodes: std::sync::Mutex<episodes::EpisodeBook>,
    /// How many clears it took (`COGNITION_CLEAR`). Each turn carries the count of its intake: a
    /// turn taken before a clear is not analyzed, and one the worker was on when a clear came brings
    /// nothing back, its communication nor its episode. A clear counts one more under every lock the
    /// worker writes under (`clear_held`), and the worker reads it under the same lock.
    clears: AtomicU64,
}

/// A communication (docs/router-stream-v1.md §6.2): the thread between its interlocutors, owned by
/// the agent that serves it. Memory and its references are per communication, never per hive or
/// node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CommunicationKey {
    thread_id: String,
    agent_ilk: String,
}

/// A copy of a turn its router made for this cognition: the message, who holds its communication
/// (`holder_of`, none for an agent without an ILK in the identity memory), whether a WF relayed it
/// between people, its record's seq, whether the intake found its communication held it already
/// (`turn_key`), the clears taken before its intake (`CognitionAppState::clears`), and when the
/// intake took it, which the worker's wait for the agent counts from (`wait_for_the_agent`).
#[derive(Debug)]
struct CognitionTurn {
    message: Message,
    holder: Option<Holder>,
    relay: bool,
    seq: u64,
    duplicate: bool,
    clears: u64,
    taken_at: time::Instant,
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
    feelings: HashMap<String, FeelingState>,
    /// Its last turns, oldest first (`push_recent_turn`): what its agent received and sent.
    turns: Vec<HeldTurn>,
    /// The keys of its last turns (`turn_key`), so one message copied twice is one turn.
    turn_keys: Vec<String>,
}

/// A turn as its communication holds it (docs/reference/cognition.md §3.4): the turn its package
/// shows, and what only this cognition reads of it, which never goes in the package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HeldTurn {
    #[serde(flatten)]
    turn: MemoryTurn,
    /// Its key (`turn_key`), which leaves the communication's keys with it when its agent's refusal
    /// drops it (FINDINGS A-292), so the message sent again under the same key is a turn again.
    /// Empty in a turn held before 0.1.137.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    key: String,
    /// The messages its agent got that it stands for, when the turn is the agent's copy of one
    /// (`request_of`), and the agent did not refuse: the agent's refusal of one names it
    /// (`refused_request`). A WF can send the agent one text twice in one cycle, under one key: the
    /// turn stands for each copy (`said_to_the_agent_too`). Empty on the agent's own answers, on a
    /// relay its agent did not get or refused, and in a turn held before 0.1.137.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requests: Vec<String>,
    /// The copies of it its agent refused while another one, which the agent took, kept the turn:
    /// when the agent refuses the last one too, the refusal is theirs as well (`refuse_turn`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refused: Vec<String>,
    /// Where a WF relayed it besides its agent, once the agent's copy made it the agent's
    /// (`said_to_the_agent_too`, `relayed_too`): the relay it is again when the agent refuses that
    /// copy (docs/cognition-hands-v1.md §12.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relayed: Option<Relayed>,
}

/// Where a WF relayed a turn (docs/cognition-hands-v1.md §12.9): the person its message named, and
/// the IO node it went to (`relayed_of`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Relayed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<MemoryTurnParty>,
    to_via: String,
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
struct FeelingState {
    feeling_id: String,
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

/// The deterministic half of a feeling. Its summary and reason come from the narrative
/// summarizer, which must return both whenever a candidate exists.
#[derive(Debug, Clone)]
struct AffectCandidate {
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
    affect: AffectCandidate,
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
    /// Where the hive's dist is, and so its installation file (docs/hive-config-update-v1.md §4).
    #[serde(default)]
    dist: Option<ai_settings::DistPathSection>,
    /// Read as raw YAML: the provider and model are the installation file's once the hive holds
    /// one, and this section is typed only until then (`cognition_ai_settings`).
    #[serde(default)]
    ai: Option<serde_yaml::Value>,
}

/// SY.cognition's provider and model, read at the start: the installation file's once the hive
/// holds one, its hive.yaml's until then; with where they came from. The error is the reason they
/// cannot be used, in its own words.
fn cognition_ai_settings(hive: &HiveFile) -> Result<(EffectiveAiEngine, Value), String> {
    let settings = ai_settings::reader_ai_settings(hive.dist.as_ref(), hive.ai.as_ref())
        .map_err(|err| ai_settings::plain_error(&err))?;
    let engine =
        ai_settings::reader_engine(&settings).map_err(|err| ai_settings::plain_error(&err))?;
    tracing::info!(
        source = %ai_settings::ai_source_label(&settings.source),
        provider = %engine.provider,
        model = %engine.model,
        "sy.cognition ai settings"
    );
    Ok((engine, ai_settings::ai_source_json(&settings.source)))
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
    // docs/hive-config-update-v1.md §4: settings no reader can use never stop the node, which keeps
    // its communications' turns; every turn's semantic work is skipped with the reason instead.
    let (ai_engine, ai_settings_source, ai_settings_error) = match cognition_ai_settings(&hive) {
        Ok((engine, source)) => (engine, source, None),
        Err(reason) => {
            tracing::error!(
                reason = %reason,
                "sy.cognition cannot use its ai settings: every turn skips its semantic work with \
                 this reason until they are fixed and the node restarts"
            );
            (HiveAiConfig::fallback(), Value::Null, Some(reason))
        }
    };
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
    initial_control_state.ai_settings_source = ai_settings_source;
    initial_control_state.ai_settings_error = ai_settings_error;
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
        takes: std::sync::Mutex::new(Takes::default()),
        refusals: std::sync::Mutex::new(Refusals::default()),
        answers: std::sync::Mutex::new(Answers::default()),
        agents_answered: Notify::new(),
        vault: vault_client,
        dispatcher: Arc::clone(&dispatcher),
        agent_ilks: Mutex::new(HashMap::new()),
        usage: UsageRecorder::start(Arc::clone(&dispatcher)),
        stream_seen: Mutex::new(HashMap::new()),
        episodes: std::sync::Mutex::new(episodes::EpisodeBook::default()),
        clears: AtomicU64::new(0),
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
    // The live episodes come first, so the packages carry their news from the start.
    load_live_episodes(&app_state).await;
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
                log_refusals_since_last_line(&app_state, std::time::Instant::now());
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
                if msg.meta.msg.as_deref() == Some(episodes::MSG_COGNITION_EPISODE) {
                    take_episode(&app_state, &msg).await;
                    continue;
                }
                // Here, as the batches are: no intake runs while a clear does.
                if msg.meta.msg.as_deref() == Some(MSG_COGNITION_CLEAR) {
                    take_clear(&app_state, &msg).await;
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
    let loaded = match app_state
        .store
        .most_recent(2 * MEMORY_MAX_DATA_SIZE, NO_AGENT_ILK)
    {
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
/// a turn and counts it. An agent's refusal of a turn it never ran (`is_busy_answer`, FINDINGS
/// A-292) is no turn: it goes to its communication after the turns before it, which include the
/// one it refused (its router copies a turn for its agent before the agent can answer it), and
/// never to the worker. Each message an agent answered or refused is noted for the worker, which
/// may be waiting for it (`Answers`, FINDINGS A-332).
async fn take_stream_batch(
    app_state: &CognitionAppState,
    turn_tx: &mpsc::Sender<CognitionTurn>,
    msg: &Message,
) {
    let taken_at = time::Instant::now();
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
    let (mut repeats, mut dropped, mut refusals) = (0u64, 0u64, 0u64);
    let mut turns = Vec::new();
    let mut recent: Vec<Intake> = Vec::new();
    let mut relays = Vec::new();
    // The messages its agents answered or refused in this batch (A-332).
    let mut answered: Vec<(CommunicationKey, String)> = Vec::new();
    // A clear runs in the main loop too, so none comes while this batch is taken.
    let clears = app_state.clears.load(Ordering::SeqCst);
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
        let relay = is_workflow_node(&record.producer);
        let refusal = !relay && is_busy_answer(&message, &record.producer);
        let holder = match message
            .meta
            .thread_id
            .as_deref()
            .filter(|thread_id| !thread_id.trim().is_empty())
        {
            // A relay's communication is found once this batch's agents' turns are held.
            Some(_) if relay => {
                relays.push((turns.len(), record.producer, record.ts_ms));
                None
            }
            Some(thread_id) => holder_of(app_state, thread_id, &record.producer).await,
            None => None,
        };
        // In the batch's order, after the turn it refused; never to the worker.
        if refusal {
            refusals += 1;
            if let Some(refusal) = holder
                .as_ref()
                .and_then(|holder| refusal_of(&message, holder))
            {
                answered.push((refusal.key.clone(), refusal.request.clone()));
                recent.push(Intake::Refusal(refusal));
            }
            continue;
        }
        if let Some(holder) = holder.as_ref() {
            answered.extend(answer_of(&message, holder));
            recent.extend(
                intake_turn(app_state, &message, holder, record.seq, record.ts_ms)
                    .map(Intake::Turn),
            );
        }
        turns.push(CognitionTurn {
            message,
            holder,
            relay,
            seq: record.seq,
            duplicate: false,
            clears,
            taken_at,
        });
    }
    // The next message its router delivers to the agent carries these turns, whatever the
    // worker's backlog. A turn its communication held already is not analyzed again.
    let mut duplicates = take_recent_turns(app_state, recent).await;
    // Once the refusals are held: the worker that wakes for one finds what it left (A-332).
    if !answered.is_empty() {
        let mut answers = app_state
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (key, request) in answered {
            answers.record(taken_at, key, request);
        }
        drop(answers);
        app_state.agents_answered.notify_waiters();
    }
    // A relay joins the communication whose agent served its thread last, this batch's turns
    // included: a WF that sends one message to an agent and to a person puts both copies in one
    // batch, whichever it sends first, and the relay is that agent's turn once
    // (docs/cognition-hands-v1.md §12.4).
    let mut relayed: Vec<Intake> = Vec::new();
    for (at, producer, ts_ms) in relays {
        let turn: &mut CognitionTurn = &mut turns[at];
        let thread_id = turn.message.meta.thread_id.clone().unwrap_or_default();
        turn.holder = holder_of(app_state, &thread_id, &producer).await;
        if let Some(holder) = turn.holder.as_ref() {
            relayed.extend(
                intake_turn(app_state, &turn.message, holder, turn.seq, ts_ms).map(Intake::Turn),
            );
        }
    }
    duplicates.extend(take_recent_turns(app_state, relayed).await);
    for mut turn in turns {
        turn.duplicate = duplicates.contains(&turn.seq);
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
    if repeats > 0 || dropped > 0 || refusals > 0 {
        let mut state = app_state.runtime_state.lock().await;
        state.stream_repeats_skipped_total += repeats;
        state.dropped_turns_queue_full_total += dropped;
        state.refusals_total += refusals;
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
    /// Its record's seq in the stream batch, and its key (`turn_key`).
    seq: u64,
    turn_key: String,
    /// On the agent's copy of a message it got, that message, as the agent's refusal of it would
    /// name it (`request_of`); empty on the agent's answers and on a relay.
    request: String,
    agent_node: String,
    src_ilk: Option<String>,
    dst_ilk: Option<String>,
    ich: Option<String>,
    turn: MemoryTurn,
}

/// What the intake keeps of a copy of a turn: `None` without a thread id or without an ILK for its
/// agent node in the identity shared memory, as the worker finds it (`agent_identity_of`, the
/// worker counts both), and without text or files.
fn intake_turn(
    app_state: &CognitionAppState,
    message: &Message,
    holder: &Holder,
    seq: u64,
    ts_ms: u64,
) -> Option<RecentTurn> {
    recent_turn_of(
        message,
        holder.agent_node(),
        holder.agent_ilk(),
        seq,
        ts_ms,
        |ilk| message_writer::ilk_facts(&app_state.hive_id, ilk),
    )
}

/// A copy of a turn as its communication keeps it. The agent sent it (`out`) when its router names
/// the agent node as the sender; otherwise the agent received it (`in`), through `turn_via`. A turn
/// the agent got names its writer, and one it sent whom it answered (`turn_party`, with what `facts`
/// says of each ILK). A relay names where it went (`relay_destination`), and the person when the
/// message names them: it was said to them, not to the agent. An answer that chose from a WF's menu
/// keeps its choice (`menu_choice`). A turn with neither text, files nor a choice is left out: the
/// package would show it empty. The agent's copy of a message it got names that message
/// (`request_of`), so the agent's refusal of it finds it (FINDINGS A-292).
fn recent_turn_of(
    message: &Message,
    agent_node: &str,
    agent_ilk: &str,
    seq: u64,
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
    let to_via = match direction {
        MemoryTurnDirection::In => relay_destination(message),
        MemoryTurnDirection::Out => None,
    };
    let request = match (direction, &to_via) {
        (MemoryTurnDirection::In, None) => request_of(message),
        _ => String::new(),
    };
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
        to: match (direction, &to_via) {
            (MemoryTurnDirection::In, None) => None,
            _ => turn_party(message.meta.dst_ilk.as_deref(), None, &facts),
        },
        to_via,
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
        seq,
        turn_key: turn_key(message),
        request,
        agent_node: agent_node.to_string(),
        src_ilk: message.meta.src_ilk.clone(),
        dst_ilk: message.meta.dst_ilk.clone(),
        ich: message.meta.ich.clone(),
        turn,
    })
}

/// The code of the AI runtime's answer to a turn it refused without running it, since 0.1.136
/// (`busy_reply`, crates/fluxbee_ai_sdk/src/runtime.rs), and the one act of the agent's entry in that
/// answer's trace (docs/message-trace-v1.md §5).
const AI_RUNTIME_BUSY: &str = "ai_runtime_busy";
const AI_RUNTIME_BUSY_ACT: &str = "busy";

/// Whether a copy of what `agent_node` sent is its refusal of a turn it never ran (FINDINGS A-292):
/// the AI runtime's answer `ai_runtime_busy` since 0.1.136 (`busy_reply`,
/// crates/fluxbee_ai_sdk/src/runtime.rs), by its exact shape, and nothing looser. Its router names
/// the agent as its sender; its payload is the runtime's error and nothing else, `{"type": "error",
/// "code": "ai_runtime_busy", "message": <text>, "retryable": true}`; and its trace ends with the
/// agent's entry, whose one act is `busy`. Anything else the agent sends is a turn, as before:
/// another error the runtime answers is an answer to a turn it ran.
fn is_busy_answer(message: &Message, agent_node: &str) -> bool {
    let sent_by_the_agent =
        !agent_node.is_empty() && message.routing.src_l2_name.as_deref() == Some(agent_node);
    let busy_payload = message.payload.as_object().is_some_and(|payload| {
        payload.len() == 4
            && payload.get("type").and_then(Value::as_str) == Some("error")
            && payload.get("code").and_then(Value::as_str) == Some(AI_RUNTIME_BUSY)
            && payload.get("message").is_some_and(Value::is_string)
            && payload.get("retryable").and_then(Value::as_bool) == Some(true)
    });
    let busy_entry = message_trace::entries(message.meta.context.as_ref())
        .last()
        .is_some_and(|entry| entry.from == agent_node && entry.act == [AI_RUNTIME_BUSY_ACT]);
    sent_by_the_agent && busy_payload && busy_entry
}

/// The message an agent got, as its refusal names it (FINDINGS A-292): its cycle, its `trace_id`,
/// and its trace as it reached the agent, which tells the messages of one cycle apart (a WF that
/// writes to the agent again on its answer writes under the same `trace_id`). The agent's answer
/// carries that trace with its own entry after it (docs/message-trace-v1.md §5,
/// `refused_request`).
fn request_of(message: &Message) -> String {
    request_id(
        &message.routing.trace_id,
        &message_trace::entries(message.meta.context.as_ref()),
    )
}

/// The message an agent's refusal names (`is_busy_answer`): the answer's cycle, and its trace before
/// the agent's entry, the message's as it reached the agent (`request_of`). None without a cycle.
fn refused_request(answer: &Message) -> Option<String> {
    if answer.routing.trace_id.is_empty() {
        return None;
    }
    let mut trace = message_trace::entries(answer.meta.context.as_ref());
    trace.pop()?;
    Some(request_id(&answer.routing.trace_id, &trace))
}

/// A message by its cycle and its trace: `<trace_id>|<hash of the trace's entries>`.
fn request_id(trace_id: &str, trace: &[message_trace::TraceEntry]) -> String {
    let entries = serde_json::to_vec(trace).unwrap_or_default();
    format!("{trace_id}|{:016x}", fnv1a(&entries))
}

/// FNV-1a, 64 bits: a short, stable hash for a turn's keys, never a security one.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// An agent's refusal of a turn it never ran, as the intake takes it (`is_busy_answer`): the
/// agent's communication and the message refused (`refused_request`). None without a thread, or
/// for an agent with no ILK in the identity memory (`holder_of`): such a turn was never held.
fn refusal_of(answer: &Message, holder: &Holder) -> Option<Refusal> {
    let Holder::Agent { identity, .. } = holder else {
        return None;
    };
    let thread_id = answer
        .meta
        .thread_id
        .clone()
        .filter(|thread_id| !thread_id.trim().is_empty())?;
    Some(Refusal {
        key: CommunicationKey {
            thread_id,
            agent_ilk: identity.ilk.clone(),
        },
        request: refused_request(answer)?,
        trace_id: answer.routing.trace_id.clone(),
    })
}

/// The message an agent's answer answers (FINDINGS A-332), as its refusal names one
/// (`refused_request`): the answer's cycle, and its trace before the agent's entry. The AI runtime
/// writes that entry, the trace's last, as it sends (docs/message-trace-v1.md §5). None when the
/// trace's last entry is not `agent_node`'s: an agent that writes none names no message.
fn answered_request(answer: &Message, agent_node: &str) -> Option<String> {
    message_trace::entries(answer.meta.context.as_ref())
        .last()
        .is_some_and(|entry| entry.from == agent_node)
        .then(|| refused_request(answer))
        .flatten()
}

/// An agent's answer to a message it got, as the intake notes it for the worker (`Answers`,
/// FINDINGS A-332): the agent's communication and the message answered (`answered_request`). None
/// for what the agent did not send, without a thread, or for an agent with no ILK in the identity
/// memory (`holder_of`), whose turns the worker skips.
fn answer_of(message: &Message, holder: &Holder) -> Option<(CommunicationKey, String)> {
    let Holder::Agent { node, identity } = holder else {
        return None;
    };
    if message.routing.src_l2_name.as_deref() != Some(node.as_str()) {
        return None;
    }
    let thread_id = message
        .meta
        .thread_id
        .clone()
        .filter(|thread_id| !thread_id.trim().is_empty())?;
    Some((
        CommunicationKey {
            thread_id,
            agent_ilk: identity.ilk.clone(),
        },
        answered_request(message, node)?,
    ))
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

/// What the intake takes from a batch, in its records' order: a turn, or an agent's refusal of a
/// turn it never ran (FINDINGS A-292), which comes after the turn it refused.
#[derive(Debug)]
enum Intake {
    Turn(RecentTurn),
    Refusal(Refusal),
}

impl From<RecentTurn> for Intake {
    fn from(turn: RecentTurn) -> Self {
        Self::Turn(turn)
    }
}

impl Intake {
    fn key(&self) -> &CommunicationKey {
        match self {
            Self::Turn(turn) => &turn.key,
            Self::Refusal(refusal) => &refusal.key,
        }
    }
}

/// An agent's refusal of a turn it never ran (`refusal_of`): the agent's communication, the message
/// it refused (`refused_request`), and its cycle, for the log.
#[derive(Debug)]
struct Refusal {
    key: CommunicationKey,
    request: String,
    trace_id: String,
}

/// What an intake batch did besides holding its turns, for the runtime counters.
#[derive(Debug, Default)]
struct IntakeTally {
    /// The records of the turns their communications held already (`turn_key`).
    duplicates: Vec<u64>,
    /// The turns its agents' refusals dropped from their communications' last turns (A-292).
    refused_dropped: u64,
}

/// How many refusals this cognition remembers (`Refusals`), the oldest out first. A refusal matters
/// while the turn it refused may wait in the worker's queue, or in the copy of its communication
/// the worker took for one turn. The refusals of turns the full queue dropped count as well: under
/// load the queue (`COGNITION_TURN_QUEUE`, 256 turns of up to two model calls, about 5 s each)
/// takes some 20 minutes to drain while the agents go on refusing, and this many cover that drain
/// at about three refusals a second, in about a megabyte. Past that, a refused turn that still
/// waits is analyzed, as before 0.1.137, and its line stays out of the last turns.
const COGNITION_REFUSALS_KEPT: usize = COGNITION_TAKES_KEPT;

/// The turns its agents refused without running them (FINDINGS A-292), each by the agent's
/// communication and the message refused (`refused_request`), with what the refusal left of the
/// turn (`Refused`): the worker analyzes none of them but as the relay a WF made of one too, and
/// its write-back never brings one back into its communication's last turns
/// (`merge_recent_turns`). In memory only: what could bring one back, the worker's queue and its
/// copy, does not outlive the process, and the file never has a turn after its refusal dropped it.
/// With them, when the journal last named them (`RefusalLog`).
#[derive(Debug, Default)]
struct Refusals {
    recent: VecDeque<Refused>,
    log: RefusalLog,
}

/// A refusal `Refusals` keeps: the agent's communication, the message refused, and the relay a WF
/// made of that message too, which the turn is again (`refuse_turn`); none when nothing of the turn
/// is left in the communication.
#[derive(Debug)]
struct Refused {
    key: CommunicationKey,
    request: String,
    relayed: Option<Box<Relayed>>,
}

impl Refusals {
    fn record(&mut self, key: &CommunicationKey, request: String, relayed: Option<&Relayed>) {
        self.recent.push_back(Refused {
            key: key.clone(),
            request,
            relayed: relayed.cloned().map(Box::new),
        });
        while self.recent.len() > COGNITION_REFUSALS_KEPT {
            self.recent.pop_front();
        }
    }

    /// The agent of `key`'s refusal of `request` (`request_of`), if it refused it. An agent's
    /// communication is never taken (§12.6), so its turns, and its refusals, are all under its own
    /// key.
    fn of(&self, key: &CommunicationKey, request: &str) -> Option<&Refused> {
        if request.is_empty() {
            return None;
        }
        self.recent
            .iter()
            .rev()
            .find(|refused| refused.key == *key && refused.request == request)
    }

    /// Whether the agent of `key` refused `request`.
    fn holds(&self, key: &CommunicationKey, request: &str) -> bool {
        self.of(key, request).is_some()
    }

    /// Whether the agent of `key` refused `request` and nothing of the turn is left: no turn of its
    /// communication, nor the relay a WF made of the message too.
    fn dropped(&self, key: &CommunicationKey, request: &str) -> bool {
        self.of(key, request)
            .is_some_and(|refused| refused.relayed.is_none())
    }
}

/// How long, from its intake, the worker waits for an agent's answer to a person's message the
/// agent got before it analyzes that turn (`wait_for_the_agent`, FINDINGS A-332). A busy AI runtime
/// refuses a turn at once (`ai_runtime_busy`, in 7 to 12 ms), and on 8.x cognition took the refusal
/// 0.04 to 0.26 s after it (A-292's gate): 2 s is some eight times the slowest, with room for a slow
/// write of the batch before it. The wait ends as soon as the agent answers or refuses, so it holds
/// back a turn the agent took by at most this, only when the worker had nothing else to do: a turn
/// that waited in the queue longer than this does not wait again.
const COGNITION_REFUSAL_GRACE: Duration = Duration::from_secs(2);

/// How many answers `Answers` keeps at most, the oldest out first: 2,048 answers a second over the
/// grace. Past that, a wait for an answer that left runs out its grace, as with no answer.
const COGNITION_ANSWERS_KEPT: usize = 4096;

/// The messages its agents answered lately, their refusals included (FINDINGS A-332): each by the
/// agent's communication and the message as the agent's copy of it names it (`request_of`), with
/// when the intake took the answer. In memory only. The worker waits for the answer to a person's
/// turn at most `COGNITION_REFUSAL_GRACE` from the turn's intake (`wait_for_the_agent`), and an
/// answer comes after the message it answers, in its agent's router's stream, so one taken longer
/// ago than that is never looked for again, and leaves.
#[derive(Debug, Default)]
struct Answers {
    recent: VecDeque<Answer>,
}

/// An answer `Answers` keeps.
#[derive(Debug)]
struct Answer {
    at: time::Instant,
    key: CommunicationKey,
    request: String,
}

impl Answers {
    fn record(&mut self, at: time::Instant, key: CommunicationKey, request: String) {
        while self
            .recent
            .front()
            .is_some_and(|answer| at.saturating_duration_since(answer.at) > COGNITION_REFUSAL_GRACE)
        {
            self.recent.pop_front();
        }
        self.recent.push_back(Answer { at, key, request });
        while self.recent.len() > COGNITION_ANSWERS_KEPT {
            self.recent.pop_front();
        }
    }

    /// Whether the agent of `key` answered or refused `request` (`request_of`).
    fn holds(&self, key: &CommunicationKey, request: &str) -> bool {
        !request.is_empty()
            && self
                .recent
                .iter()
                .rev()
                .any(|answer| answer.key == *key && answer.request == request)
    }
}

/// The least time between two of the journal's lines on the agents' refusals (`RefusalLog`), as the
/// AI runtime names its own.
const COGNITION_REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// When the journal names the agents' refusals the intake takes: the first at once, then at most
/// one line every `COGNITION_REFUSAL_LOG_INTERVAL`, with how many came since the last; the end of a
/// burst at the heartbeat, once its line is due (`log_refusals_since_last_line`). Under load they
/// come at the rate the agents refuse turns, and a line for each would flood the journal.
#[derive(Debug, Default)]
struct RefusalLog {
    logged_at: Option<std::time::Instant>,
    unlogged: u64,
}

impl RefusalLog {
    /// Counts one; when its line is due, how many to name in it (this one and those since the
    /// last).
    fn note(&mut self, now: std::time::Instant) -> Option<u64> {
        self.unlogged += 1;
        self.flush(now)
    }

    /// How many were counted and not named yet, once their line is due: the end of a burst, which
    /// no later refusal would name. None when there are none, or the last line is too recent.
    fn flush(&mut self, now: std::time::Instant) -> Option<u64> {
        if self.unlogged == 0
            || self
                .logged_at
                .is_some_and(|at| now.duration_since(at) < COGNITION_REFUSAL_LOG_INTERVAL)
        {
            return None;
        }
        self.logged_at = Some(now);
        Some(std::mem::take(&mut self.unlogged))
    }
}

/// The heartbeat's line on the agents' refusals the intake took since the journal's last one, once
/// that line is due (`RefusalLog`): the end of a burst.
fn log_refusals_since_last_line(app_state: &CognitionAppState, now: std::time::Instant) {
    let since = app_state
        .refusals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .log
        .flush(now);
    if let Some(refusals) = since {
        tracing::info!(
            refusals,
            "sy.cognition took more agents' refusals of turns they never ran (ai_runtime_busy) since its last line on them"
        );
    }
}

/// The intake's turns and refusals into what is held (docs/reference/cognition.md §3.4): each
/// communication is written to the file and the region is rewritten with them, before the turns
/// reach the worker's queue. The records of the turns held already (duplicates).
async fn take_recent_turns(app_state: &CognitionAppState, intake: Vec<Intake>) -> Vec<u64> {
    if intake.is_empty() {
        return Vec::new();
    }
    let taken = intake
        .iter()
        .filter(|item| matches!(item, Intake::Turn(_)))
        .count() as u64;
    let mut kept: Vec<CommunicationKey> = Vec::new();
    for item in &intake {
        if !kept.contains(item.key()) {
            kept.push(item.key().clone());
        }
    }
    let mut loads = StoreLoads::default();
    let mut tally = IntakeTally::default();
    if let Err(err) = sync_held_state(app_state, &kept, |threads| {
        let mut takes = app_state
            .takes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut refusals = app_state
            .refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        hold_recent_turns(
            threads,
            &app_state.store,
            &mut takes,
            &mut refusals,
            intake,
            &mut loads,
            &mut tally,
        )
    })
    .await
    {
        tracing::warn!(error = %err, "sy.cognition failed to sync jsr-memory at intake");
    }
    let mut state = app_state.runtime_state.lock().await;
    state.intake_turns_total = state.intake_turns_total.saturating_add(taken);
    state.duplicate_turns_skipped_total = state
        .duplicate_turns_skipped_total
        .saturating_add(tally.duplicates.len() as u64);
    state.refused_turns_dropped_total = state
        .refused_turns_dropped_total
        .saturating_add(tally.refused_dropped);
    loads.count(&mut state);
    tally.duplicates
}

/// The loads of communications from the file one change made, for the runtime counters.
#[derive(Debug, Default)]
struct StoreLoads {
    loaded: u64,
    failed: u64,
    /// Communications with no agent an agent took (`take_conversation`).
    taken: u64,
}

impl StoreLoads {
    fn count(&self, state: &mut CognitionRuntimeState) {
        state.store_loads_total = state.store_loads_total.saturating_add(self.loaded);
        state.store_load_failures_total =
            state.store_load_failures_total.saturating_add(self.failed);
        state.conversations_taken_total =
            state.conversations_taken_total.saturating_add(self.taken);
    }
}

/// How many takes the held state remembers (`Takes`), oldest out first. A take matters while a turn
/// held in the communication it took waits for the worker, whose queue holds `COGNITION_TURN_QUEUE`;
/// each take is a thread reaching its first agent, so this many outlast any such wait.
const COGNITION_TAKES_KEPT: usize = 4096;

/// A thread's communication with no agent that the first agent on the thread took
/// (docs/cognition-hands-v1.md §12.6): the agent's new communication started from it, and it left
/// memory and the file. Every turn of it from then on is the taker's.
#[derive(Debug, Clone)]
struct Take {
    taken: CommunicationKey,
    taker: CommunicationKey,
}

/// The takes the held state made, recorded under its lock by whoever made them, the intake or the
/// worker (`take_conversation`); and the rows of the taken communications that the file still has,
/// deleted in the transaction that writes their takers (`sync_held_state`).
#[derive(Debug, Default)]
struct Takes {
    recent: VecDeque<Take>,
    removals: Vec<CommunicationKey>,
}

impl Takes {
    fn record(&mut self, take: Take) {
        if !self.removals.contains(&take.taken) {
            self.removals.push(take.taken.clone());
        }
        self.recent.push_back(take);
        while self.recent.len() > COGNITION_TAKES_KEPT {
            self.recent.pop_front();
        }
    }

    /// Where a turn of `key` goes: the communication that took `key`, else `key`. The intake follows
    /// it for each turn, and the worker at each step, since a take can come while the models
    /// answer: a taken communication is never made again.
    fn follow(&self, key: &CommunicationKey) -> CommunicationKey {
        self.taker_of(key).unwrap_or(key).clone()
    }

    /// The communication that took `key` last, if one did.
    fn taker_of(&self, key: &CommunicationKey) -> Option<&CommunicationKey> {
        self.recent
            .iter()
            .rev()
            .find(|take| take.taken == *key)
            .map(|take| &take.taker)
    }

    /// Whether the file's row of `key` is a taken communication's, to be deleted: it is its
    /// taker's now, and is never read again.
    fn removal_pending(&self, key: &CommunicationKey) -> bool {
        self.removals.contains(key)
    }
}

/// A communication as held, or else from the file, or else new. `Err` when the file could not be
/// read: a new one would write over what the file keeps. A taken communication's row, waiting to
/// be deleted, is not read (`Takes::removal_pending`).
///
/// An agent's new communication starts from what people said on its thread before any agent
/// served it here (docs/cognition-hands-v1.md §12.6, the operator's call of 2026-11-07): it takes
/// the thread's communication with no agent (`take_conversation`).
fn held_or_loaded<'a>(
    threads: &'a mut HashMap<CommunicationKey, ThreadCognitionState>,
    store: &CognitionStore,
    takes: &mut Takes,
    key: &CommunicationKey,
    loads: &mut StoreLoads,
) -> Result<&'a mut ThreadCognitionState, CognitionError> {
    if !threads.contains_key(key) {
        let stored = if takes.removal_pending(key) {
            None
        } else {
            store.load(key).inspect_err(|_| loads.failed += 1)?
        };
        let state = match stored {
            Some(state) => {
                loads.loaded += 1;
                state
            }
            None if key.agent_ilk != NO_AGENT_ILK => {
                take_conversation(threads, store, takes, key, loads).unwrap_or_default()
            }
            None => ThreadCognitionState::default(),
        };
        threads.insert(key.clone(), state);
    }
    Ok(threads.get_mut(key).expect("held above"))
}

/// The communication with no agent of `taker`'s thread, held or else from the file, taken out of
/// both for `taker`, the agent's new communication there (§12.6): its turns, their keys and what
/// was made of them. Moved, not copied, so nothing counts twice: the agent's copy of a WF's message
/// relayed to a person is the same turn. The take is recorded (`Takes`), and the row leaves the
/// file in the transaction that writes `taker`, which every caller writes now. `None` when people
/// said nothing there before, or when its row cannot be read: the agent starts without it, and the
/// row stays.
fn take_conversation(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    store: &CognitionStore,
    takes: &mut Takes,
    taker: &CommunicationKey,
    loads: &mut StoreLoads,
) -> Option<ThreadCognitionState> {
    let no_agent = CommunicationKey {
        thread_id: taker.thread_id.clone(),
        agent_ilk: NO_AGENT_ILK.to_string(),
    };
    // Only the thread's first agent takes it: what people said beside another agent's conversation
    // stays theirs, as each agent sees only its own (§12.6's first question).
    let another_agent = threads.keys().any(|key| {
        key.thread_id == taker.thread_id && key.agent_ilk != NO_AGENT_ILK && key != taker
    }) || match store.has_agent_on_thread(&taker.thread_id, NO_AGENT_ILK) {
        Ok(found) => found,
        Err(err) => {
            tracing::warn!(
                thread_id = %taker.thread_id,
                error = %err,
                "sy.cognition could not read whether a thread had an agent; its people's turns stay with no agent"
            );
            true
        }
    };
    if another_agent {
        return None;
    }
    let taken = match threads.remove(&no_agent) {
        Some(held) => held,
        None if takes.removal_pending(&no_agent) => return None,
        None => match store.load(&no_agent) {
            Ok(stored) => stored?,
            Err(err) => {
                loads.failed += 1;
                tracing::warn!(
                    thread_id = %taker.thread_id,
                    error = %err,
                    "sy.cognition could not read what people said on a thread before its first agent; the agent starts without it"
                );
                return None;
            }
        },
    };
    takes.record(Take {
        taken: no_agent,
        taker: taker.clone(),
    });
    loads.taken += 1;
    tracing::info!(
        thread_id = %taker.thread_id,
        agent_ilk = %taker.agent_ilk,
        turns = taken.turns.len(),
        "sy.cognition: the first agent on a thread takes what people said there before"
    );
    Some(taken)
}

/// Adds each turn to its communication's last turns, and takes each refusal into its agent's
/// communication (`take_refusal`), in the order they came; returns the communications it touched.
/// A communication whose file could not be read keeps its turn out: the worker skips it too.
fn hold_recent_turns(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    store: &CognitionStore,
    takes: &mut Takes,
    refusals: &mut Refusals,
    intake: Vec<Intake>,
    loads: &mut StoreLoads,
    tally: &mut IntakeTally,
) -> Vec<CommunicationKey> {
    let mut touched: Vec<CommunicationKey> = Vec::new();
    for item in intake {
        let mut recent = match item {
            Intake::Turn(recent) => recent,
            Intake::Refusal(refusal) => {
                if take_refusal(threads, store, refusals, &refusal, loads, tally)
                    && !touched.contains(&refusal.key)
                {
                    touched.push(refusal.key);
                }
                continue;
            }
        };
        // A turn of a communication an agent took is the taker's (§12.6).
        recent.key = takes.follow(&recent.key);
        let taken_before = loads.taken;
        let held = match held_or_loaded(threads, store, takes, &recent.key, loads) {
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
        // One that took its thread's people is written now, whatever this turn is: its row goes in
        // the transaction that deletes the one it took (§12.6).
        if loads.taken > taken_before && !touched.contains(&recent.key) {
            touched.push(recent.key.clone());
        }
        // The agent's node is the intake's, whatever the turn: one that took the thread's people
        // has none yet, and without it neither the region nor a later relay finds the agent.
        if !recent.agent_node.is_empty() {
            held.agent_node = recent.agent_node.clone();
        }
        // One message relayed by a WF to a person and copied for an agent too is one turn, and so
        // is the agent's own answer a WF relays in a later cycle, under that cycle's trace
        // (docs/cognition-hands-v1.md §12.4). When the agent's copy comes after the relay, the
        // turn was said to the agent too (§12.9); either way the turn keeps where the relay went,
        // and stands for each copy the agent got, for the agent's refusal of one. A held turn this
        // changed is written.
        if held.turn_keys.contains(&recent.turn_key) || echoes_own_answer(held, &recent) {
            let changed = if recent.turn.to_via.is_none() {
                said_to_the_agent_too(&mut held.turns, &recent)
            } else {
                relayed_too(&mut held.turns, &recent)
            };
            if changed && !touched.contains(&recent.key) {
                touched.push(recent.key.clone());
            }
            tally.duplicates.push(recent.seq);
            continue;
        }
        held.turn_keys.push(recent.turn_key.clone());
        let excess = held
            .turn_keys
            .len()
            .saturating_sub(2 * COGNITION_RECENT_TURNS);
        held.turn_keys.drain(..excess);
        held.src_ilk = recent.src_ilk;
        held.dst_ilk = recent.dst_ilk;
        held.ich = recent.ich;
        push_recent_turn(
            &mut held.turns,
            HeldTurn {
                turn: recent.turn,
                key: recent.turn_key,
                requests: Some(recent.request)
                    .filter(|request| !request.is_empty())
                    .into_iter()
                    .collect(),
                refused: Vec::new(),
                relayed: None,
            },
        );
        if !touched.contains(&recent.key) {
            touched.push(recent.key);
        }
    }
    touched
}

/// An agent's refusal of a turn it never ran (FINDINGS A-292), into its communication: the turn
/// that stands for the agent's copy of the message it refused leaves the last turns with its key
/// (`refuse_turn`), so no package shows that message as said to the agent and unanswered, and the
/// message sent again is one turn. What it left is remembered for the worker (`Refusals`), and so
/// is a refusal no held turn stood for; one another copy kept the turn for is not: the worker
/// analyzes that turn. Named in the journal at most once an interval (`RefusalLog`). Whether the
/// communication changed. A communication neither held nor in the file had no such turn, and none
/// is started.
fn take_refusal(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    store: &CognitionStore,
    refusals: &mut Refusals,
    refusal: &Refusal,
    loads: &mut StoreLoads,
    tally: &mut IntakeTally,
) -> bool {
    let refused = match threads.entry(refusal.key.clone()) {
        std::collections::hash_map::Entry::Occupied(held) => {
            refuse_turn(held.into_mut(), &refusal.request)
        }
        std::collections::hash_map::Entry::Vacant(vacant) => match store.load(&refusal.key) {
            Ok(Some(stored)) => {
                loads.loaded += 1;
                refuse_turn(vacant.insert(stored), &refusal.request)
            }
            Ok(None) => None,
            Err(err) => {
                loads.failed += 1;
                tracing::warn!(
                    thread_id = %refusal.key.thread_id,
                    agent_ilk = %refusal.key.agent_ilk,
                    trace_id = %refusal.trace_id,
                    error = %err,
                    "sy.cognition could not read a communication from its file; the turn its agent refused stays in it"
                );
                None
            }
        },
    };
    let (left, changed) = match &refused {
        None => {
            refusals.record(&refusal.key, refusal.request.clone(), None);
            ("no_turn_held", false)
        }
        Some(RefusedTurn::Kept) => ("kept_for_another_copy", true),
        Some(RefusedTurn::Dropped { requests }) => {
            for request in requests {
                refusals.record(&refusal.key, request.clone(), None);
            }
            tally.refused_dropped += 1;
            ("dropped", true)
        }
        Some(RefusedTurn::Relayed { relayed, requests }) => {
            for request in requests {
                refusals.record(&refusal.key, request.clone(), Some(relayed));
            }
            ("relay_again", true)
        }
    };
    match refusals.log.note(std::time::Instant::now()) {
        Some(since) => tracing::info!(
            thread_id = %refusal.key.thread_id,
            agent_ilk = %refusal.key.agent_ilk,
            trace_id = %refusal.trace_id,
            turn = left,
            refusals = since,
            "sy.cognition took an agent's refusal of a turn it never ran (ai_runtime_busy); refusals counts it and those since its last line on them"
        ),
        None => tracing::debug!(
            thread_id = %refusal.key.thread_id,
            agent_ilk = %refusal.key.agent_ilk,
            trace_id = %refusal.trace_id,
            turn = left,
            "sy.cognition took an agent's refusal of a turn it never ran (ai_runtime_busy)"
        ),
    }
    changed
}

/// What an agent's refusal did to its communication's last turns (`refuse_turn`), with the messages
/// whose refusal it is: the one refused, and the copies of it the agent refused while another kept
/// the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefusedTurn {
    /// Another copy of the message, which the agent took, keeps the turn.
    Kept,
    /// The turn left them, with its key.
    Dropped { requests: Vec<String> },
    /// A WF relayed the message to a person too: the turn is that relay again.
    Relayed {
        relayed: Relayed,
        requests: Vec<String>,
    },
}

/// The agent's refusal of `request` in its communication: the last turn that stands for that copy
/// of a message (`HeldTurn::requests`), one turn for one refusal. While it stands for another copy,
/// which the agent took, it stays (a WF can send the agent one text twice in one cycle). Otherwise
/// it leaves the last turns, and its key the communication's keys, so the message sent again under
/// the same key (a WF's send again in the same cycle) is a turn again. One a WF relayed to a person
/// too stays, as that relay: it was said to them, not to the agent (docs/cognition-hands-v1.md
/// §12.9). None when no held turn stands for that copy: it had no text nor files, or came before
/// 0.1.137.
fn refuse_turn(held: &mut ThreadCognitionState, request: &str) -> Option<RefusedTurn> {
    let at = held
        .turns
        .iter()
        .rposition(|turn| turn.requests.iter().any(|taken| taken == request))?;
    let turn = &mut held.turns[at];
    turn.requests.retain(|taken| taken != request);
    if !turn.requests.is_empty() {
        turn.refused.push(request.to_string());
        return Some(RefusedTurn::Kept);
    }
    let mut requests = std::mem::take(&mut turn.refused);
    requests.push(request.to_string());
    if let Some(relayed) = turn.relayed.take() {
        turn.turn.to = relayed.to.clone();
        turn.turn.to_via = Some(relayed.to_via.clone());
        return Some(RefusedTurn::Relayed { relayed, requests });
    }
    let dropped = held.turns.remove(at);
    if !dropped.key.is_empty() {
        held.turn_keys.retain(|key| *key != dropped.key);
    }
    Some(RefusedTurn::Dropped { requests })
}

/// Whether a turn is its agent's own answer coming back: the agent wrote it, received it, and the
/// communication holds an answer with its text.
fn echoes_own_answer(held: &ThreadCognitionState, recent: &RecentTurn) -> bool {
    recent.turn.direction == MemoryTurnDirection::In
        && recent
            .turn
            .writer
            .as_ref()
            .and_then(|writer| writer.ilk.as_deref())
            .is_some_and(|ilk| ilk.trim().eq_ignore_ascii_case(&recent.key.agent_ilk))
        && held.turns.iter().any(|held| {
            held.turn.direction == MemoryTurnDirection::Out && held.turn.text == recent.turn.text
        })
}

/// The agent's copy of a turn its communication holds already: the turn stands for that copy too
/// (`HeldTurn::requests`), for the agent's refusal of it (FINDINGS A-292). A relay the agent's copy
/// comes after was said to the agent too, so it no longer names where else it went
/// (docs/cognition-hands-v1.md §12.9), and keeps it (`relayed`). A WF that forwards one message to
/// an IO node and to the agent has its router copy each, and the two can come in two batches,
/// either first; and a WF can send the agent one text twice in one cycle, under one key. Found by
/// its key; by its writer, node and text only in a relay held before 0.1.137, which has no key.
/// Whether a held turn changed.
fn said_to_the_agent_too(turns: &mut [HeldTurn], copy: &RecentTurn) -> bool {
    if copy.turn.direction != MemoryTurnDirection::In || copy.request.is_empty() {
        return false;
    }
    let Some(held) = turns.iter_mut().rev().find(|held| {
        held.turn.direction == MemoryTurnDirection::In
            && if held.key.is_empty() {
                held.turn.to_via.is_some()
                    && held.turn.writer == copy.turn.writer
                    && held.turn.via == copy.turn.via
                    && held.turn.text == copy.turn.text
            } else {
                held.key == copy.turn_key
            }
    }) else {
        return false;
    };
    let mut changed = false;
    if let Some(to_via) = held.turn.to_via.take() {
        held.relayed = Some(Relayed {
            to: held.turn.to.take(),
            to_via,
        });
        changed = true;
    }
    if !held.requests.contains(&copy.request) {
        held.requests.push(copy.request.clone());
        changed = true;
    }
    changed
}

/// A relay whose agent's copy came first (`said_to_the_agent_too` is the other order): the turn
/// stays the agent's, and keeps where the relay went (`relayed`), the line it is again when the
/// agent refuses its copies. Found by its key, which the copies share. Whether a held turn changed.
fn relayed_too(turns: &mut [HeldTurn], relay: &RecentTurn) -> bool {
    let Some(to_via) = relay.turn.to_via.clone() else {
        return false;
    };
    let Some(held) = turns.iter_mut().rev().find(|held| {
        held.key == relay.turn_key
            && !held.requests.is_empty()
            && held.relayed.is_none()
            && held.turn.to_via.is_none()
    }) else {
        return false;
    };
    held.relayed = Some(Relayed {
        to: relay.turn.to.clone(),
        to_via,
    });
    true
}

/// Adds a turn to a communication's last turns, in the order they were taken, unless they hold it
/// already (a batch sent again).
fn push_recent_turn(turns: &mut Vec<HeldTurn>, turn: HeldTurn) {
    if turns.contains(&turn) {
        return;
    }
    turns.push(turn);
    turns.sort_by_key(|held| parse_rfc3339_epoch_ms(&held.turn.ts).unwrap_or_default());
    bound_recent_turns(turns);
}

/// The oldest turns go past `COGNITION_RECENT_TURNS`, and past `COGNITION_RECENT_TURNS_MAX_BYTES`
/// as the package carries them; the newest always stays.
fn bound_recent_turns(turns: &mut Vec<HeldTurn>) {
    let excess = turns.len().saturating_sub(COGNITION_RECENT_TURNS);
    turns.drain(..excess);
    let size = |held: &HeldTurn| serde_json::to_vec(&held.turn).map_or(0, |bytes| bytes.len() + 1);
    let mut bytes: usize = turns.iter().map(size).sum();
    let mut oldest = 0;
    while turns.len() - oldest > 1 && bytes > COGNITION_RECENT_TURNS_MAX_BYTES {
        bytes -= size(&turns[oldest]);
        oldest += 1;
    }
    turns.drain(..oldest);
}

/// One communication's last turns from two copies of them, the worker's and what is held: each turn
/// of either once, in the order they were taken, bounded as at intake. A turn in both is the held
/// one: only the intake changes turns, and it may have learned more of one since the worker's copy
/// (`said_to_the_agent_too`, `refuse_turn`). A turn of the copy only that its agent refused
/// (`refused`) stays out: the intake dropped it meanwhile (FINDINGS A-292).
fn merge_recent_turns(
    worker: &[HeldTurn],
    held: &[HeldTurn],
    refused: impl Fn(&HeldTurn) -> bool,
) -> Vec<HeldTurn> {
    let mut merged = held.to_vec();
    for turn in worker {
        if !refused(turn) && !merged.iter().any(|kept| same_turn(kept, turn)) {
            merged.push(turn.clone());
        }
    }
    merged.sort_by_key(|held| parse_rfc3339_epoch_ms(&held.turn.ts).unwrap_or_default());
    bound_recent_turns(&mut merged);
    merged
}

/// Whether two copies are one turn: taken at the same time, the same way, from the same writer and
/// sender, with the same text and files. Where a relay went is left out: the intake may clear it.
fn same_turn(a: &HeldTurn, b: &HeldTurn) -> bool {
    let (a, b) = (&a.turn, &b.turn);
    a.ts == b.ts
        && a.direction == b.direction
        && a.src_ilk == b.src_ilk
        && a.via == b.via
        && a.writer == b.writer
        && a.text == b.text
        && a.attachments == b.attachments
        && a.choice == b.choice
}

/// The worker's turn back into what is held: its derivations replace the held ones, and what the
/// intake owns stays as the intake left it. The turns are the copy's with those the intake took
/// while the model answered, merged, so none is lost even if what is held was started again; none
/// its agent refused comes back (`refusals`, by any copy of a message the turn stood for).
fn write_back_turn(
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
    key: &CommunicationKey,
    mut updated: ThreadCognitionState,
    refusals: &Refusals,
) {
    if let Some(held) = threads.get_mut(key) {
        updated.turns = merge_recent_turns(&updated.turns, &held.turns, |turn| {
            turn.requests
                .iter()
                .chain(&turn.refused)
                .any(|request| refusals.holds(key, request))
        });
        updated.turn_keys = std::mem::take(&mut held.turn_keys);
        // The intake's node, which a communication that took its thread's people while the models
        // answered has and the worker's copy of the one it took has not (§12.6).
        if !held.agent_node.is_empty() {
            updated.agent_node = std::mem::take(&mut held.agent_node);
        }
        updated.src_ilk = held.src_ilk.take();
        updated.dst_ilk = held.dst_ilk.take();
        updated.ich = held.ich.take();
    }
    threads.insert(key.clone(), updated);
}

/// The communication the worker's turn goes to, held: `key`, or the agent's that took it
/// (`Takes::follow`, docs/cognition-hands-v1.md §12.6), back from the file when it is not held
/// (`held_or_loaded`). `None`, and nothing loaded, when a clear came after `clears`, its
/// turn's: the turn belongs to what the clear dropped. Read under the held state's lock, which a
/// clear holds while it counts one more. One this makes new takes its thread's people as the
/// intake's do, and is written at once with the taken row's deletion: a take never lives only in
/// memory. A new one with no node gets `agent_node`, the turn's agent's.
async fn hold_for_worker(
    app_state: &CognitionAppState,
    key: &CommunicationKey,
    agent_node: &str,
    clears: u64,
    loads: &mut StoreLoads,
) -> Result<Option<CommunicationKey>, CognitionError> {
    {
        let threads = app_state.thread_states.lock().await;
        if app_state.clears.load(Ordering::SeqCst) != clears {
            return Ok(None);
        }
        let target = app_state
            .takes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .follow(key);
        if threads.contains_key(&target) {
            return Ok(Some(target));
        }
    }
    let mut held: Result<Option<CommunicationKey>, CognitionError> = Ok(None);
    let synced = sync_held_state(app_state, std::slice::from_ref(key), |threads| {
        if app_state.clears.load(Ordering::SeqCst) != clears {
            return Vec::new();
        }
        let mut takes = app_state
            .takes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target = takes.follow(key);
        let taken_before = loads.taken;
        match held_or_loaded(threads, &app_state.store, &mut takes, &target, loads) {
            Ok(state) => {
                if state.agent_node.is_empty() && !agent_node.is_empty() {
                    state.agent_node = agent_node.to_string();
                }
                let took = loads.taken > taken_before;
                held = Ok(Some(target.clone()));
                if took {
                    vec![target]
                } else {
                    Vec::new()
                }
            }
            Err(err) => {
                held = Err(err);
                Vec::new()
            }
        }
    })
    .await;
    if let Err(err) = synced {
        tracing::warn!(error = %err, "sy.cognition failed to sync jsr-memory holding a turn's communication");
    }
    held
}

/// What came of a worker's write-back (`write_back_worker_turn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteBack {
    Written,
    /// A clear came while the models answered: the turn belongs to what it dropped.
    AfterClear,
    /// The communication it goes to was let go from memory and its file row cannot be read: the
    /// result is dropped, never written over the row.
    Unreadable,
    /// Its agent refused the turn without running it while the models answered (FINDINGS A-292):
    /// the turn is no turn of its communication, and what it derived is dropped. A turn that is
    /// the relay a WF made of the message too again keeps it: it was analyzed once.
    Refused,
}

/// The worker's turn back into what is held (`write_back_turn`), its file row and the region's
/// rewrite with it, unless a clear came after `clears`, its turn's, while the model answered, or
/// its agent refused `request`, the message the turn is (`request_of`), and nothing of the turn is
/// left in the communication (`Refusals::dropped`): then nothing comes back.
/// It goes to `key`, or to the agent's communication that took it while the models answered
/// (`Takes::follow`), back from the file first when it is not held, so its turns are merged and
/// never written over. What came of it, and the region's write.
async fn write_back_worker_turn(
    app_state: &CognitionAppState,
    key: &CommunicationKey,
    updated: ThreadCognitionState,
    clears: u64,
    request: &str,
) -> (WriteBack, Result<(), json_router::shm::ShmError>) {
    let mut outcome = WriteBack::AfterClear;
    let mut loads = StoreLoads::default();
    let synced = sync_held_state(app_state, std::slice::from_ref(key), |threads| {
        // Under the region's lock and the held state's, which a clear holds while it counts one
        // more, and so does every take and every refusal the intake takes.
        if app_state.clears.load(Ordering::SeqCst) != clears {
            return Vec::new();
        }
        let mut takes = app_state
            .takes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let refusals = app_state
            .refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target = takes.follow(key);
        if refusals.dropped(&target, request) {
            outcome = WriteBack::Refused;
            return Vec::new();
        }
        if let Err(err) = held_or_loaded(threads, &app_state.store, &mut takes, &target, &mut loads)
        {
            tracing::warn!(
                thread_id = %target.thread_id,
                agent_ilk = %target.agent_ilk,
                error = %err,
                "sy.cognition could not read the communication a turn's result goes to; the result is dropped"
            );
            outcome = WriteBack::Unreadable;
            return Vec::new();
        }
        write_back_turn(threads, &target, updated, &refusals);
        outcome = WriteBack::Written;
        vec![target]
    })
    .await;
    loads.count(&mut *app_state.runtime_state.lock().await);
    (outcome, synced)
}

/// How the worker's wait for an agent's answer ended (`wait_for_the_agent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentWait {
    /// No wait: the agent had answered or refused already, or the turn's grace was over.
    NotNeeded,
    /// The agent answered or refused within the grace.
    Answered,
    /// The grace ran out with neither.
    GraceOver,
}

/// The worker's wait for the agent of `key` to answer or refuse `request`, a person's message it
/// got (FINDINGS A-332), until `COGNITION_REFUSAL_GRACE` after the turn's intake (`taken_at`). A
/// busy AI runtime refuses a turn at once, and a refusal the intake takes while the tagger answers
/// leaves that call wasted (A-292 drops its result): so the analysis starts once the agent answered
/// or refused, or once the grace is over, whichever comes first. The intake wakes it at each
/// answer it takes (`agents_answered`). A refused turn is then left out as before
/// (`Refusals`); one the agent took is analyzed. Never cancelling a call already made: its provider
/// bills it whatever cognition does, and a call dropped is recorded `abandoned`, without tokens.
async fn wait_for_the_agent(
    app_state: &CognitionAppState,
    key: &CommunicationKey,
    request: &str,
    taken_at: time::Instant,
) -> AgentWait {
    let until = taken_at + COGNITION_REFUSAL_GRACE;
    let mut waited = false;
    loop {
        // Registered before the look, so an answer the intake takes right after it wakes the wait.
        let mut answered = std::pin::pin!(app_state.agents_answered.notified());
        answered.as_mut().enable();
        if app_state
            .answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .holds(key, request)
        {
            return if waited {
                AgentWait::Answered
            } else {
                AgentWait::NotNeeded
            };
        }
        if time::Instant::now() >= until {
            return if waited {
                AgentWait::GraceOver
            } else {
                AgentWait::NotNeeded
            };
        }
        waited = true;
        tokio::select! {
            () = answered => {}
            () = time::sleep_until(until) => {}
        }
    }
}

/// One turn of a communication (§6.2): the ILK of the agent that owns it, its state (from the file
/// when it is not held), the tagger and the derivations, its file row and the agent's memory
/// package. A person's message the agent got waits first for the agent's answer, a short grace at
/// most (`wait_for_the_agent`, FINDINGS A-332).
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
    let Some(holder) = turn.holder else {
        app_state.runtime_state.lock().await.no_agent_ilk_total += 1;
        tracing::debug!(
            thread_id = %thread_id,
            "sy.cognition skipping a turn of an agent without an ILK in the identity shared memory"
        );
        return;
    };
    if turn.relay {
        let mut state = app_state.runtime_state.lock().await;
        state.relay_turns_total = state.relay_turns_total.saturating_add(1);
    }
    // One message copied for an agent and relayed to a person too is analyzed once
    // (docs/cognition-hands-v1.md §12.4).
    if turn.duplicate {
        return;
    }
    let agent_node = holder.agent_node().to_string();
    let agent_ilk = holder.agent_ilk().to_string();
    let key = CommunicationKey {
        thread_id: thread_id.clone(),
        agent_ilk: agent_ilk.clone(),
    };
    // The message the turn is, when it is the agent's copy of one it got (`request_of`): the
    // agent's refusal of it names it (FINDINGS A-292). A relay, or the agent's own answer, is none.
    let request = if turn.relay || msg.routing.src_l2_name.as_deref() == Some(agent_node.as_str()) {
        String::new()
    } else {
        request_of(&msg)
    };
    let person = person_at_other_end(&msg, &agent_node, |ilk| {
        message_writer::ilk_facts(&app_state.hive_id, ilk)
    });
    // A person's message the agent got, which the agent may still refuse, is analyzed once the
    // agent answered or refused it, or once its grace is over (FINDINGS A-332): a refusal the
    // intake takes while the tagger answers wastes that call. A turn no model reads waits for
    // nothing. A cognition that cannot call a model (`ai_settings_error`, no key) waits all the
    // same: the wait costs no call, and a refused turn is then counted as refused, not as a
    // tagger failure.
    if person && !request.is_empty() {
        let started = time::Instant::now();
        let waited = wait_for_the_agent(&app_state, &key, &request, turn.taken_at).await;
        if waited != AgentWait::NotNeeded {
            let mut state = app_state.runtime_state.lock().await;
            state.agent_waits_total = state.agent_waits_total.saturating_add(1);
            if waited == AgentWait::GraceOver {
                state.agent_waits_grace_over_total =
                    state.agent_waits_grace_over_total.saturating_add(1);
            }
            drop(state);
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                agent_ilk = %agent_ilk,
                waited_ms = started.elapsed().as_millis() as u64,
                ended = ?waited,
                "sy.cognition waited for the agent's answer to a person's turn before analyzing it"
            );
        }
    }
    // A turn its agent refused without running it is no turn of its communication: the intake
    // dropped it, and it is not analyzed. One a WF relayed to a person too is that relay again:
    // this copy came first, the relay's record was its duplicate, and this copy is analyzed as the
    // relay (docs/cognition-hands-v1.md §12.9). One refused while it is analyzed derives nothing
    // (`WriteBack::Refused`).
    let refused = app_state
        .refusals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .of(&key, &request)
        .map(|refused| refused.relayed.as_deref().cloned());
    let relayed_again = match refused {
        Some(None) => {
            app_state
                .runtime_state
                .lock()
                .await
                .refused_turns_not_analyzed_total += 1;
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                agent_ilk = %agent_ilk,
                "sy.cognition took a turn its agent refused without running it: not analyzed"
            );
            return;
        }
        Some(Some(relayed)) => Some(relayed),
        None => None,
    };
    // Only a turn with a person at its other end is analyzed (docs/cognition-hands-v1.md §6): a turn
    // between agents, or a WF's own text, stays among the communication's last turns, which the
    // intake took and wrote, and in `turns`.
    if !person {
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
    let held = hold_for_worker(&app_state, &key, &agent_node, turn.clears, &mut loads).await;
    loads.count(&mut *app_state.runtime_state.lock().await);
    let target = match held {
        Ok(Some(target)) => target,
        Ok(None) => {
            tracing::info!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                "sy.cognition skipped a turn taken before a clear"
            );
            return;
        }
        Err(err) => {
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                agent_ilk = %agent_ilk,
                error = %err,
                "sy.cognition could not read a communication from its file; turn skipped"
            );
            return;
        }
    };
    // The first agent on the thread took the communication the intake held this turn in (§12.6):
    // the turn is that agent's now, analyzed in its communication and paid by its tenant.
    let (holder, key) = if target == key {
        (holder, key)
    } else {
        *app_state.worker_turn.lock().await = Some(target.clone());
        (
            taker_holder(&app_state, &target).await.unwrap_or(holder),
            target,
        )
    };
    let agent_node = holder.agent_node().to_string();
    let agent_ilk = holder.agent_ilk().to_string();

    // Whose feelings the turn moves, and who spoke it for the narrator (§10.3).
    let facts = |ilk: &str| message_writer::ilk_facts(&app_state.hive_id, ilk);
    let owner = feeling_owner(&msg, &agent_node, facts);
    let speaker = narrator_speaker(&msg, &agent_node, facts);
    // Where a relay went, and so whom it was written to, which the tagger reads between people
    // (§12.9): its message's, or the relay a WF made too of a message its agent refused.
    let relayed = match &relayed_again {
        Some(relayed) => Some(relayed.clone()),
        None if turn.relay => relayed_of(&msg, facts),
        None => None,
    };
    let addressee = relayed.as_ref().map(relay_addressee);
    let text = extract_turn_text(&msg.payload).unwrap_or_default();
    let (thresholds, semantic_tagger_config, ai_settings_error) = {
        let control_state = app_state.control_state.lock().await;
        (
            control_state.thresholds.clone(),
            control_state.semantic_tagger.clone(),
            control_state.ai_settings_error.clone(),
        )
    };
    let ts = chrono::Utc::now().to_rfc3339();
    let src_ilk = msg.meta.src_ilk.clone();
    // A relay's is the person its message named: the agent's copy analyzed as the relay a WF made
    // of it too takes that one's.
    let dst_ilk = match &relayed_again {
        Some(relayed) => relayed.to.as_ref().and_then(|to| to.ilk.clone()),
        None => msg.meta.dst_ilk.clone(),
    };
    let ich = msg.meta.ich.clone();
    {
        let mut state = app_state.runtime_state.lock().await;
        state.semantic_tagger_calls_total = state.semantic_tagger_calls_total.saturating_add(1);
        state.last_semantic_model = Some(semantic_tagger_config.model.clone());
        state.last_semantic_impl = Some(format!("{}_sdk", semantic_tagger_config.provider));
    }

    // docs/hive-config-update-v1.md §4: `ai` settings no reader can use skip the turn's semantic
    // work, with their reason, as a missing key does; never a fallback to another `ai`.
    if let Some(reason) = ai_settings_error {
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
            reason = %reason,
            "sy.cognition semantic tagger skipped turn because its ai settings cannot be used"
        );
        return;
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

    // Who holds the turn's result pays for its calls (§7.3, docs/cognition-hands-v1.md §12.3): the
    // communication's agent's tenant or, in one with no agent, the writer's.
    let Some(payer) = payer_of(&app_state.hive_id, &holder, &msg) else {
        tracing::debug!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            "sy.cognition skipping a turn with no agent and no writer's ILK to pay for it"
        );
        return;
    };
    let account = AiAccount::charge(
        app_state.usage.clone(),
        &app_state.node_name,
        payer.tenant,
        payer.source,
    )
    .with_node_ilk(Some(app_state.self_ilk_id.clone()))
    .with_counterpart(Some(payer.ilk))
    .with_trace(Some(msg.routing.trace_id.clone()))
    .with_thread(Some(thread_id.clone()));
    let tagger = match run_semantic_tagger_ai(semantic_tagger_ai::SemanticTaggerAiInput {
        api_key: &api_key,
        account: &account,
        text: &text,
        src_ilk: src_ilk.as_deref(),
        dst_ilk: dst_ilk.as_deref(),
        ich: ich.as_deref(),
        to: addressee.as_deref(),
        config: &semantic_tagger_config,
    })
    .await
    {
        Ok(output) => output,
        Err(err) => {
            let mut state = app_state.runtime_state.lock().await;
            match model_failure(&err) {
                ModelFailure::Provider => {
                    state.semantic_tagger_provider_errors_total = state
                        .semantic_tagger_provider_errors_total
                        .saturating_add(1);
                }
                ModelFailure::InvalidOutput => {
                    state.semantic_tagger_invalid_outputs_total = state
                        .semantic_tagger_invalid_outputs_total
                        .saturating_add(1);
                }
                ModelFailure::Other => {
                    state.semantic_tagger_failures_total =
                        state.semantic_tagger_failures_total.saturating_add(1);
                }
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

    // A refusal that came while the tagger answered: no summarizer call for a turn whose result the
    // write-back would drop (`WriteBack::Refused`, FINDINGS A-292).
    let refused = app_state
        .refusals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .dropped(&key, &request);
    if refused {
        app_state
            .runtime_state
            .lock()
            .await
            .refused_turns_not_analyzed_total += 1;
        tracing::debug!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            agent_ilk = %agent_ilk,
            "sy.cognition dropped a turn its agent refused without running it while the tagger answered"
        );
        return;
    }

    // The derivations run on a copy, without the lock: the intake goes on taking turns while the
    // summarizer answers, and they stay (`write_back_turn`). Every region write keeps the worker's
    // communication, and the one that took it (`sync_held_state`), so it is still held: the copy is
    // of the one that took it when a take came while the tagger answered (§12.6). The turn's calls
    // stay with the payer they started with.
    let copied = {
        let threads = app_state.thread_states.lock().await;
        let target = app_state
            .takes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .follow(&key);
        threads.get(&target).cloned().map(|state| (target, state))
    };
    let Some((target, mut thread_state)) = copied else {
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            agent_ilk = %agent_ilk,
            "sy.cognition no longer holds the communication it loaded; turn skipped"
        );
        return;
    };
    if target != key {
        *app_state.worker_turn.lock().await = Some(target.clone());
    }
    let key = target;
    if !agent_node.is_empty() {
        thread_state.agent_node = agent_node.clone();
    }
    // Whom the turn is to, and the people an episode of it would be about (§12.4).
    let (counterpart, people) = episode_people(
        &msg,
        &agent_node,
        &agent_ilk,
        relayed.as_ref(),
        &thread_state.turns,
        facts,
    );
    let ask = EpisodeAsk {
        counterpart: counterpart.as_ref().and_then(party_words),
        habits: app_state
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .habits(&people, now_epoch_ms_i64()),
    };
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
        &ask,
        &AiNarrativeSummarizer,
    )
    .await;
    let leading = leading_contexts(&thread_state.contexts);

    let (written_back, synced) =
        write_back_worker_turn(&app_state, &key, thread_state, turn.clears, &request).await;
    if let Err(err) = synced {
        tracing::warn!(
            error = %err,
            trace_id = %msg.routing.trace_id,
            thread_id = %thread_id,
            "sy.cognition failed to sync jsr-memory"
        );
    }
    match written_back {
        WriteBack::Written => {}
        WriteBack::AfterClear => {
            tracing::info!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                "sy.cognition dropped what a turn derived: a clear came while it was analyzed"
            );
            return;
        }
        WriteBack::Refused => {
            app_state
                .runtime_state
                .lock()
                .await
                .refused_turns_not_analyzed_total += 1;
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                "sy.cognition dropped what a turn derived: its agent refused it without running it while it was analyzed"
            );
            return;
        }
        // Warned where it was found.
        WriteBack::Unreadable => return,
    }
    if let Some(proposal) = narrative.episode.as_ref() {
        publish_episode(
            &app_state,
            proposal,
            &leading,
            people,
            &thread_id,
            turn.clears,
        )
        .await;
    }

    let (
        active_threads_total,
        open_contexts_total,
        open_reasons_total,
        open_cooccurrences_total,
        active_scopes_total,
        active_memories_total,
        active_feelings_total,
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
        let active_feelings = threads
            .values()
            .map(|thread| thread.feelings.len() as u64)
            .sum();
        (
            active_threads,
            open_contexts,
            open_reasons,
            open_cooccurrences,
            active_scopes,
            active_memories,
            active_feelings,
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
    if narrative.provider_error {
        state.narrative_summarizer_provider_errors_total = state
            .narrative_summarizer_provider_errors_total
            .saturating_add(1);
    }
    state.active_threads_total = active_threads_total;
    state.open_contexts_total = open_contexts_total;
    state.open_reasons_total = open_reasons_total;
    state.open_cooccurrences_total = open_cooccurrences_total;
    state.active_scopes_total = active_scopes_total;
    state.active_memories_total = active_memories_total;
    state.active_feelings_total = active_feelings_total;
}

/// The key's agent ILK of a communication with no agent (docs/cognition-hands-v1.md §12.4): what
/// people said to each other through a WF on a thread no agent served on this hive. Its package
/// reaches no one, so the region leaves it out.
const NO_AGENT_ILK: &str = "none";

/// Who holds a copied turn's communication, and so pays for its analysis
/// (docs/cognition-hands-v1.md §12.3).
#[derive(Debug, Clone)]
enum Holder {
    /// The agent its router copied it for or, for a relay between people (a WF's message to an IO
    /// node), the agent that served its thread last on this hive.
    Agent {
        node: String,
        identity: AgentIdentity,
    },
    /// No agent serves the thread here: each turn's writer pays.
    NoAgent,
}

impl Holder {
    fn agent_node(&self) -> &str {
        match self {
            Self::Agent { node, .. } => node,
            Self::NoAgent => "",
        }
    }

    fn agent_ilk(&self) -> &str {
        match self {
            Self::Agent { identity, .. } => &identity.ilk,
            Self::NoAgent => NO_AGENT_ILK,
        }
    }
}

/// Whether a node is a WF, whose messages to IO nodes its router copies for this cognition: a relay
/// between people (docs/cognition-hands-v1.md §12.4).
fn is_workflow_node(name: &str) -> bool {
    name.get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("WF."))
}

/// The IO node a relay went to: the destination of what a WF sent an IO node, the only copy whose
/// router names an IO node as its destination (`copied_for_cognition_at_entry`, src/router/mod.rs).
/// None for any other turn: what an agent gets is sent to it (docs/cognition-hands-v1.md §12.9).
fn relay_destination(message: &Message) -> Option<String> {
    match &message.routing.dst {
        Destination::Unicast(dst) if is_io_node(dst.trim()) => {
            Some(turn_field(dst.trim(), COGNITION_TURN_NAME_MAX_CHARS))
        }
        _ => None,
    }
}

/// Whether a node is an IO node, by its name, as the router tells one (`is_io_node`,
/// src/router/mod.rs).
fn is_io_node(name: &str) -> bool {
    name.split_once('@')
        .map_or(name, |(role, _)| role)
        .starts_with("IO.")
}

/// Where a relay went (docs/cognition-hands-v1.md §12.9): the IO node its router names as its
/// destination (`relay_destination`), and the person its message names, as its turn keeps them
/// (`recent_turn_of`). None for any other turn.
fn relayed_of(message: &Message, facts: impl Fn(&str) -> Option<IlkFacts>) -> Option<Relayed> {
    Some(Relayed {
        to_via: relay_destination(message)?,
        to: turn_party(message.meta.dst_ilk.as_deref(), None, &facts),
    })
}

/// Whom a relay was written to, in words, for the tagger: the person it named and the IO node it
/// went to, or the node alone (docs/cognition-hands-v1.md §12.9).
fn relay_addressee(relayed: &Relayed) -> String {
    match relayed.to.as_ref().and_then(party_words) {
        Some(person) => format!("{person} via {}", relayed.to_via),
        None => relayed.to_via.clone(),
    }
}

/// Whose communication a copy of a turn joins (docs/cognition-hands-v1.md §12.4). Its router copies
/// an agent's turns for that agent, and a WF's messages to IO nodes, a relay between people, for
/// the thread. A relay joins the communication whose agent served the thread last on this hive (a
/// solution's WF runs on its agents' hive), or else the thread's communication with no agent. None
/// for an agent with no ILK in the identity memory.
async fn holder_of(
    app_state: &CognitionAppState,
    thread_id: &str,
    producer: &str,
) -> Option<Holder> {
    if !is_workflow_node(producer) {
        let identity = agent_identity_of(app_state, producer).await?;
        return Some(Holder::Agent {
            node: producer.to_string(),
            identity,
        });
    }
    let held = app_state
        .thread_states
        .lock()
        .await
        .iter()
        .filter(|(key, state)| {
            key.thread_id == thread_id
                && key.agent_ilk != NO_AGENT_ILK
                && !state.agent_node.is_empty()
        })
        .max_by_key(|(_, state)| last_turn_epoch_ms(state))
        .map(|(_, state)| (last_turn_epoch_ms(state), state.agent_node.clone()));
    let stored = app_state
        .store
        .last_agent_on_thread(thread_id, NO_AGENT_ILK)
        .unwrap_or_else(|err| {
            tracing::warn!(thread_id, error = %err, "sy.cognition could not read its file for a relay's agent");
            None
        });
    let node = match (held, stored) {
        (Some((held_ms, _)), Some((stored_ms, stored_node))) if stored_ms > held_ms => stored_node,
        (Some((_, node)), _) | (None, Some((_, node))) => node,
        (None, None) => return Some(Holder::NoAgent),
    };
    match agent_identity_of(app_state, &node).await {
        Some(identity) => Some(Holder::Agent { node, identity }),
        None => Some(Holder::NoAgent),
    }
}

/// The agent of a communication that took its thread's people (§12.6), by the node its intake
/// wrote: `None` when it has no node yet, or its node is now another agent's.
async fn taker_holder(app_state: &CognitionAppState, taker: &CommunicationKey) -> Option<Holder> {
    let node = app_state
        .thread_states
        .lock()
        .await
        .get(taker)
        .map(|state| state.agent_node.clone())
        .filter(|node| !node.is_empty())?;
    let identity = agent_identity_of(app_state, &node).await?;
    identity
        .ilk
        .eq_ignore_ascii_case(&taker.agent_ilk)
        .then_some(Holder::Agent { node, identity })
}

/// Who pays for a turn's model calls (docs/cognition-hands-v1.md §12.3): the tenant the usage
/// record claims, why, and the ILK SY.storage checks it by.
struct Payer {
    tenant: Option<String>,
    source: TenantSource,
    ilk: String,
}

/// The holder of a turn's result pays: the communication's agent, or, with no agent, the turn's
/// writer, by their ILK. None with no agent and no writer's ILK.
fn payer_of(hive_id: &str, holder: &Holder, message: &Message) -> Option<Payer> {
    match holder {
        Holder::Agent { identity, .. } => Some(Payer {
            tenant: identity.tenant.clone(),
            source: TenantSource::Agent,
            ilk: identity.ilk.clone(),
        }),
        Holder::NoAgent => {
            let writer = message_writer::writer_of(message.meta.context.as_ref())
                .and_then(|writer| writer.ilk)
                .or_else(|| message.meta.src_ilk.clone())
                .map(|ilk| ilk.trim().to_string())
                .filter(|ilk| !ilk.is_empty())?;
            let tenant = fluxbee_sdk::identity::find_ilk_by_id_from_hive_id(hive_id, &writer)
                .ok()
                .flatten()
                .map(|found| found.tenant_id)
                .filter(|tenant| !tenant.trim().is_empty());
            Some(Payer {
                tenant,
                source: TenantSource::Writer,
                ilk: writer,
            })
        }
    }
}

/// A turn's key: its cycle (trace id), its writer and its text. One message a WF relays to a person
/// and is also copied for an agent keeps one key (docs/cognition-hands-v1.md §12.4).
fn turn_key(message: &Message) -> String {
    let writer = message_writer::writer_of(message.meta.context.as_ref())
        .and_then(|writer| writer.ilk)
        .or_else(|| message.meta.src_ilk.clone())
        .unwrap_or_default();
    let text = extract_turn_text(&message.payload).unwrap_or_default();
    format!(
        "{}|{}|{:016x}",
        message.routing.trace_id,
        writer.trim(),
        fnv1a(text.as_bytes())
    )
}

/// Whom a turn is to, and the ILKs of the people an episode of it would be about: its writer and
/// whom it is to (docs/cognition-hands-v1.md §12.4). The agent's answer is to its `dst_ilk`; a turn
/// the agent got is to the agent; a turn a WF relayed (`relayed`, where it went: `relayed_of`) is
/// to the person its message names, else to the last other person who wrote through the IO node it
/// went to, else to that node, which names no one (§12.9).
fn episode_people(
    message: &Message,
    agent_node: &str,
    agent_ilk: &str,
    relayed: Option<&Relayed>,
    turns: &[HeldTurn],
    facts: impl Fn(&str) -> Option<IlkFacts>,
) -> (Option<MemoryTurnParty>, Vec<String>) {
    let sent = !agent_node.is_empty() && message.routing.src_l2_name.as_deref() == Some(agent_node);
    let writer = if sent {
        Some(agent_ilk.to_string())
    } else {
        turn_writer(message, &facts).and_then(|writer| writer.ilk)
    };
    let another_person = |party: &MemoryTurnParty| {
        party.ilk.is_some()
            && party.ilk != writer
            && !matches!(party.kind.as_deref(), Some("agent" | "workflow" | "system"))
    };
    let counterpart = if sent {
        turn_party(message.meta.dst_ilk.as_deref(), None, &facts)
    } else if let Some(relayed) = relayed {
        Some(
            relayed
                .to
                .clone()
                .filter(another_person)
                .or_else(|| {
                    turns
                        .iter()
                        .rev()
                        .map(|held| &held.turn)
                        .filter(|turn| {
                            turn.direction == MemoryTurnDirection::In
                                && turn.via.as_deref() == Some(relayed.to_via.as_str())
                        })
                        .filter_map(|turn| turn.writer.clone())
                        .find(another_person)
                })
                .unwrap_or_else(|| MemoryTurnParty {
                    ilk: None,
                    name: Some(relayed.to_via.clone()),
                    kind: None,
                }),
        )
    } else if agent_ilk != NO_AGENT_ILK {
        Some(MemoryTurnParty {
            ilk: Some(agent_ilk.to_string()),
            name: Some(agent_node.to_string()),
            kind: Some("agent".to_string()),
        })
    } else {
        None
    };
    let people = episodes::people_of(
        writer
            .as_deref()
            .into_iter()
            .chain(counterpart.as_ref().and_then(|party| party.ilk.as_deref())),
    );
    (counterpart, people)
}

/// The communication's two leading contexts, the heaviest: an episode rests on one of them.
fn leading_contexts(contexts: &HashMap<String, ContextState>) -> Vec<String> {
    let mut leading: Vec<&ContextState> = contexts.values().collect();
    leading.sort_by(|left, right| {
        right
            .weight
            .total_cmp(&left.weight)
            .then_with(|| left.label.cmp(&right.label))
    });
    leading
        .into_iter()
        .take(2)
        .map(|context| context.label.clone())
        .collect()
}

/// Now, in milliseconds since the epoch, as the episodes count it.
fn now_epoch_ms_i64() -> i64 {
    i64::try_from(json_router::shm::now_epoch_ms()).unwrap_or(i64::MAX)
}

/// The people of a communication, whose news its package carries (docs/cognition-hands-v1.md
/// §12.4): those its turns name, the writers of what its agent got and whom it answered, and the
/// owners of its feelings. Not its agent, nor any ILK the identity knows as an agent's, a WF's or
/// the system's.
fn people_of_conversation(thread_state: &ThreadCognitionState, agent_ilk: &str) -> Vec<String> {
    let parties = thread_state
        .turns
        .iter()
        .map(|held| &held.turn)
        .filter_map(|turn| match turn.direction {
            MemoryTurnDirection::In => turn.writer.as_ref(),
            MemoryTurnDirection::Out => turn.to.as_ref(),
        })
        .chain(thread_state.feelings.values().map(|feeling| &feeling.owner))
        .filter(|party| !matches!(party.kind.as_deref(), Some("agent" | "workflow" | "system")))
        .filter_map(|party| party.ilk.as_deref())
        .filter(|ilk| *ilk != agent_ilk);
    episodes::distinct_ilks(parties)
}

/// An episode the narrator proposed, through the gate (`gate_episode`): kept here, in memory and in
/// the file, and sent to every hive's cognition (docs/cognition-hands-v1.md §12.4). It must be about
/// a person: an ILK the identity knows as no one else's, or does not know yet. `clears` are its
/// turn's: an episode a clear came before goes nowhere (`keep_episode`).
async fn publish_episode(
    app_state: &CognitionAppState,
    proposal: &episodes::EpisodeProposal,
    leading: &[String],
    people: Vec<String>,
    thread_id: &str,
    clears: u64,
) {
    let about_a_person = people.iter().any(|ilk| {
        message_writer::ilk_facts(&app_state.hive_id, ilk)
            .is_none_or(|facts| facts.is_person() || facts.kind().is_none())
    });
    let now_ms = now_epoch_ms_i64();
    let habit = app_state
        .episodes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .habit(&proposal.lens, &people, now_ms);
    let verdict = if about_a_person {
        episodes::gate_episode(proposal, leading, habit)
    } else {
        Err(episodes::GateRefusal::NotAboutAPerson)
    };
    let intensity = match verdict {
        Ok(intensity) => intensity,
        Err(refusal) => {
            let mut state = app_state.runtime_state.lock().await;
            state.episodes_gated_out_total = state.episodes_gated_out_total.saturating_add(1);
            // At info, with the gate's numbers: whether such a proposal should be news is the
            // operator's call on them (docs/cognition-hands-v1.md §12.6). Never its title or summary,
            // nor a model's free text: an unknown lens shows as such, and the labels are capped.
            tracing::info!(
                thread_id,
                lens = episodes::lens_for_log(&proposal.lens),
                refused = refusal.as_str(),
                base_intensity = proposal.base_intensity,
                novelty = proposal.novelty,
                habit,
                intensity = episodes::final_intensity(proposal, habit),
                evidence_strength = proposal.evidence_strength,
                evidence_contexts = ?episodes::labels_for_log(&proposal.evidence_contexts),
                leading_contexts = ?episodes::labels_for_log(leading),
                "sy.cognition: a proposed episode did not pass the gate"
            );
            return;
        }
    };
    let episode = episodes::episode_of(
        Uuid::new_v4().to_string(),
        proposal,
        intensity,
        now_ms,
        people,
        episodes::EpisodeOrigin {
            thread_id: thread_id.to_string(),
            node: app_state.node_name.clone(),
        },
    );
    if !keep_episode(app_state, episode.clone(), clears).await {
        tracing::info!(
            thread_id,
            "sy.cognition dropped an episode: a clear came while its turn was analyzed"
        );
        return;
    }
    let sender = app_state.dispatcher.sender_snapshot();
    let sent = match episode_broadcast(&sender, &episode) {
        Ok(message) => sender.send(message).await.map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    };
    if let Err(err) = sent {
        tracing::warn!(episode_id = %episode.episode_id, error = %err, "sy.cognition could not send an episode to the other hives");
    }
    let mut state = app_state.runtime_state.lock().await;
    state.episodes_made_total = state.episodes_made_total.saturating_add(1);
    tracing::info!(
        episode_id = %episode.episode_id,
        lens = %episode.lens,
        intensity = episode.intensity,
        people = episode.people.len(),
        thread_id,
        "sy.cognition made an episode and sent it to every hive"
    );
}

/// The broadcast that carries an episode to every hive's cognition: a system message, as SY.vault's
/// notice is, its receivers checking who sent it (`take_episode`).
fn episode_broadcast(
    sender: &NodeSender,
    episode: &episodes::Episode,
) -> Result<Message, serde_json::Error> {
    Ok(Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(episodes::MSG_COGNITION_EPISODE.to_string()),
            target: Some(episodes::EPISODE_BROADCAST_TARGET.to_string()),
            ..Meta::default()
        },
        payload: serde_json::to_value(episode)?,
    })
}

/// Keeps an episode once, in memory and in the file, and writes the region so the packages carry
/// it. False when it had it already, or when a clear came after `clears`, the clears it was made
/// under: what it was made from is what the clear dropped. Checked under the book's lock, which a
/// clear holds while it empties the file and the book.
async fn keep_episode(
    app_state: &CognitionAppState,
    episode: episodes::Episode,
    clears: u64,
) -> bool {
    let (kept, live) = {
        let mut book = app_state
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if app_state.clears.load(Ordering::SeqCst) != clears {
            return false;
        }
        if let Err(err) = app_state.store.save_episode(&episode) {
            tracing::warn!(episode_id = %episode.episode_id, error = %err, "sy.cognition could not write an episode to its file");
        }
        (book.insert(episode), book.len() as u64)
    };
    app_state.runtime_state.lock().await.episodes_live_total = live;
    if kept {
        if let Err(err) = sync_held_state(app_state, &[], |_| Vec::new()).await {
            tracing::warn!(error = %err, "sy.cognition failed to write jsr-memory with an episode");
        }
    }
    kept
}

/// An episode another hive's cognition made (docs/cognition-hands-v1.md §12.4): taken only from a
/// cognition, as its router names the sender, read, checked (`Episode::checked`) and kept once.
async fn take_episode(app_state: &CognitionAppState, msg: &Message) {
    let from = msg.routing.src_l2_name.as_deref().unwrap_or_default();
    let episode = from
        .starts_with("SY.cognition@")
        .then(|| serde_json::from_value::<episodes::Episode>(msg.payload.clone()).ok())
        .flatten()
        .and_then(episodes::Episode::checked);
    let Some(episode) = episode else {
        let mut state = app_state.runtime_state.lock().await;
        state.episodes_refused_total = state.episodes_refused_total.saturating_add(1);
        tracing::warn!(
            from,
            "sy.cognition refused an episode: not a cognition's, or it does not read"
        );
        return;
    };
    let episode_id = episode.episode_id.clone();
    // A clear runs in the main loop too, so none comes while this one is kept.
    let clears = app_state.clears.load(Ordering::SeqCst);
    if keep_episode(app_state, episode, clears).await {
        let mut state = app_state.runtime_state.lock().await;
        state.episodes_received_total = state.episodes_received_total.saturating_add(1);
        tracing::info!(episode_id = %episode_id, from, "sy.cognition took an episode from another hive");
    }
}

/// At start, the episodes not faded come back from the file, the faded ones leave it.
async fn load_live_episodes(app_state: &CognitionAppState) {
    let now_ms = now_epoch_ms_i64();
    if let Err(err) = app_state.store.sweep_episodes(now_ms) {
        tracing::warn!(error = %err, "sy.cognition could not sweep its faded episodes at start");
    }
    let found = match app_state.store.live_episodes(now_ms) {
        Ok(found) => found,
        Err(err) => {
            tracing::warn!(error = %err, "sy.cognition could not read its episodes at start");
            return;
        }
    };
    let live = {
        let mut book = app_state
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for episode in found {
            book.insert(episode);
        }
        book.len() as u64
    };
    app_state.runtime_state.lock().await.episodes_live_total = live;
    tracing::info!(live, "sy.cognition loaded its live episodes from its file");
}

/// `COGNITION_CLEAR` (FINDINGS A-160): SY.admin on the motherbee has this cognition drop everything
/// it keeps (`clear_everything`), as the factory reset does to the motherbee's by removing its file
/// with the node stopped. Any other sender is refused, and nothing is touched. Either way it
/// answers.
async fn take_clear(app_state: &CognitionAppState, msg: &Message) {
    let answer = match authorize_clear(msg) {
        Ok(()) => clear_everything(app_state).await,
        Err(detail) => {
            tracing::warn!(detail = %detail, "sy.cognition refused a clear");
            json!({ "ok": false, "error": "UNAUTHORIZED", "detail": detail })
        }
    };
    let sender = app_state.dispatcher.sender_snapshot();
    let reply = build_system_reply(&sender, msg, MSG_COGNITION_CLEAR_RESPONSE, answer);
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(error = %err, "sy.cognition could not answer a clear");
    }
}

/// A clear is taken only from SY.admin on the motherbee, as its router names the sender, and never
/// from a tap's copy, which keeps the name of a sender that did not address it here. The router
/// does not gate it: it is none of the system policy's protected actions.
fn authorize_clear(msg: &Message) -> Result<(), String> {
    if msg.meta.via_tap {
        return Err(format!(
            "{MSG_COGNITION_CLEAR} is not taken from a tap's copy"
        ));
    }
    match msg.routing.src_l2_name.as_deref().map(str::trim) {
        Some(src) if src == COGNITION_CLEAR_AUTHORITY => Ok(()),
        Some("") | None => Err(format!(
            "{MSG_COGNITION_CLEAR} requires router-stamped source"
        )),
        Some(src) => Err(format!("{MSG_COGNITION_CLEAR} not authorized from {src}")),
    }
}

/// What a clear removed: the communications and episodes its file kept, and those it held in
/// memory, which the region was written from.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Cleared {
    file_communications: u64,
    file_episodes: u64,
    held_communications: u64,
    held_episodes: u64,
}

/// Drops everything this cognition keeps and answers what it removed: its file's communications
/// and episodes, what it holds in memory, those a failed write left there included, and its memory
/// region, written empty. It keeps running, empty. A file it cannot clear leaves everything as it
/// was; a region it cannot write is said, the rest cleared.
async fn clear_everything(app_state: &CognitionAppState) -> Value {
    let has_region = app_state.memory_region.lock().await.is_some();
    let mut cleared: Result<Cleared, CognitionError> = Ok(Cleared::default());
    let synced = sync_held_state(app_state, &[], |threads| {
        cleared = clear_held(app_state, threads);
        Vec::new()
    })
    .await;
    let cleared = match cleared {
        Ok(cleared) => cleared,
        Err(err) => {
            tracing::warn!(error = %err, "sy.cognition could not clear its file; it keeps everything");
            return json!({
                "ok": false,
                "error": "STORE_ERROR",
                "detail": format!("its file could not be cleared, and nothing was: {err}"),
            });
        }
    };
    app_state
        .runtime_state
        .lock()
        .await
        .cleared(chrono::Utc::now().to_rfc3339());
    tracing::info!(
        file_communications = cleared.file_communications,
        file_episodes = cleared.file_episodes,
        held_communications = cleared.held_communications,
        held_episodes = cleared.held_episodes,
        "sy.cognition cleared everything it kept"
    );
    let removed = json!({
        "file": {
            "communications": cleared.file_communications,
            "episodes": cleared.file_episodes,
        },
        "memory": {
            "communications": cleared.held_communications,
            "episodes": cleared.held_episodes,
        },
    });
    match synced {
        Ok(()) => json!({
            "ok": true,
            "node_name": app_state.node_name,
            "removed": removed,
            "region": if has_region { "written_empty" } else { "none" },
        }),
        Err(err) => {
            tracing::warn!(error = %err, "sy.cognition cleared its file and memory, and could not write its memory region");
            json!({
                "ok": false,
                "error": "REGION_ERROR",
                "detail": format!("its file and memory are cleared, and its memory region could not be written: {err}"),
                "node_name": app_state.node_name,
                "removed": removed,
            })
        }
    }
}

/// The clear itself, inside `sync_held_state`'s change: under the region's lock and the held
/// state's, the order every writer takes them in, and under the book's, which an episode is kept
/// under. Every write of the worker checks the clears under one of them, so a turn it is on finds
/// this one done and brings nothing back. The file first: when it cannot be cleared, nothing is.
fn clear_held(
    app_state: &CognitionAppState,
    threads: &mut HashMap<CommunicationKey, ThreadCognitionState>,
) -> Result<Cleared, CognitionError> {
    let mut book = app_state
        .episodes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (file_communications, file_episodes) = app_state.store.clear()?;
    app_state.clears.fetch_add(1, Ordering::SeqCst);
    let held_episodes = book.clear() as u64;
    drop(book);
    let held_communications = threads.len() as u64;
    threads.clear();
    app_state
        .unsaved
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    // The file is empty: no row is left to delete, nor any communication a turn could follow to,
    // nor a turn a refusal could name.
    *app_state
        .takes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Takes::default();
    *app_state
        .refusals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Refusals::default();
    Ok(Cleared {
        file_communications,
        file_episodes,
        held_communications,
        held_episodes,
    })
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
    // What the change wrote stays, ahead of the rest (a write-back that followed a take writes
    // the communication that took the worker's, §12.6).
    let mut kept = kept.to_vec();
    for key in &changed {
        if !kept.contains(key) {
            kept.push(key.clone());
        }
    }
    let worker_turn = app_state.worker_turn.lock().await.clone();
    let written = {
        let mut takes = app_state
            .takes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The worker's communication stays too, whoever writes, and the one that took it if one
        // did while the models answered: its turn is back only after its model calls.
        if let Some(key) = worker_turn {
            let taker = takes.taker_of(&key).cloned();
            for key in std::iter::once(key).chain(taker) {
                if !kept.contains(&key) {
                    kept.push(key);
                }
            }
        }
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
        // A taken communication's row leaves the file in the transaction that writes its taker,
        // which is among these, or unsaved with it since the write that failed (§12.6).
        let removals = takes.removals.clone();
        let written = app_state.store.save(
            to_save.iter().filter_map(|key| threads.get_key_value(key)),
            &removals,
        );
        if written.is_ok() {
            takes.removals.retain(|key| !removals.contains(key));
        } else {
            unsaved.extend(to_save.into_iter().filter(|key| threads.contains_key(key)));
        }
        for key in unsaved.iter() {
            if !kept.contains(key) {
                kept.push(key.clone());
            }
        }
        written
    };
    // The faded episodes leave memory and the file; the rest are the packages' news, as of now.
    let now_ms = now_epoch_ms_i64();
    let (synced, episodes_live) = {
        let mut book = app_state
            .episodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if book.sweep(now_ms) > 0 {
            if let Err(err) = app_state.store.sweep_episodes(now_ms) {
                tracing::warn!(error = %err, "sy.cognition could not sweep its faded episodes");
            }
        }
        let hive_id = app_state.hive_id.as_str();
        let news_of = |key: &CommunicationKey, thread_state: &ThreadCognitionState| {
            book.news_for(
                &people_of_conversation(thread_state, &key.agent_ilk),
                now_ms,
                |ilk| {
                    turn_party(Some(ilk), None, &|ilk: &str| {
                        message_writer::ilk_facts(hive_id, ilk)
                    })
                    .unwrap_or_else(|| MemoryTurnParty {
                        ilk: Some(ilk.to_string()),
                        ..MemoryTurnParty::default()
                    })
                },
            )
        };
        let synced = match memory_region.as_mut() {
            Some(region) => {
                retain_memory_hot_set(&mut threads, &kept, &news_of).and_then(|hot_set| {
                    region.write_snapshot(&hot_set.snapshot)?;
                    Ok(Some(hot_set.stats))
                })
            }
            None => Ok(None),
        };
        (synced, book.len() as u64)
    };
    drop(threads);
    drop(memory_region);

    let mut state = app_state.runtime_state.lock().await;
    state.episodes_live_total = episodes_live;
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
    news_of: &dyn Fn(&CommunicationKey, &ThreadCognitionState) -> (Vec<NewsSummary>, u32),
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
        // A communication with no agent: its package would reach no one.
        if key.agent_ilk == NO_AGENT_ILK {
            continue;
        }
        let package = build_memory_package_for_thread(
            &key.thread_id,
            thread_state,
            news_of(key, thread_state),
        );
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

    // A communication with no agent is no candidate, so it is never counted as pruned.
    let candidates_total = candidates.len();
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
    let pruned_threads_total = candidates_total.saturating_sub(selected_entries.len()) as u64;

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
    news_of: &dyn Fn(&CommunicationKey, &ThreadCognitionState) -> (Vec<NewsSummary>, u32),
) -> Result<MemoryHotSetBuild, json_router::shm::ShmError> {
    let hot_set = build_memory_hot_set_snapshot(threads, kept, news_of)?;
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
        + thread_state.feelings.len()
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
    thread_last_seen_epoch_ms(thread_state).max(last_turn_epoch_ms(thread_state))
}

/// When a router took a communication's last turn: whose agent served a thread last, whatever the
/// worker's backlog (docs/cognition-hands-v1.md §12.4).
fn last_turn_epoch_ms(thread_state: &ThreadCognitionState) -> i64 {
    thread_state
        .turns
        .last()
        .and_then(|held| parse_rfc3339_epoch_ms(&held.turn.ts))
        .unwrap_or_default()
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
/// feelings from the worker, and its last turns from the intake.
fn build_memory_package_for_thread(
    thread_id: &str,
    thread_state: &ThreadCognitionState,
    (news, dropped_news): (Vec<NewsSummary>, u32),
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
    let mut feelings: Vec<&FeelingState> = thread_state.feelings.values().collect();
    feelings.sort_by(|left, right| {
        right
            .weight
            .total_cmp(&left.weight)
            .then_with(|| right.last_felt_at.cmp(&left.last_felt_at))
            .then_with(|| left.feeling_id.cmp(&right.feeling_id))
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
    let dropped_feelings = feelings
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
        feelings: feelings
            .into_iter()
            .take(COGNITION_PACKAGE_MAX_FEELINGS)
            .map(|feeling| FeelingSummary {
                feeling_id: feeling.feeling_id.clone(),
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
        // The news of the communication's people (docs/cognition-hands-v1.md §12.4).
        news,
        // Bounded at intake (`bound_recent_turns`): they all go, as the agent reads them, without
        // what only this cognition reads of them (`HeldTurn`).
        turns: thread_state
            .turns
            .iter()
            .map(|held| held.turn.clone())
            .collect(),
        truncated: if dropped_contexts == 0
            && dropped_reasons == 0
            && dropped_memories == 0
            && dropped_feelings == 0
            && dropped_news == 0
        {
            None
        } else {
            Some(MemoryPackageTruncated {
                applied: true,
                dropped_contexts,
                dropped_reasons,
                dropped_memories,
                dropped_feelings,
                dropped_news,
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
        "source": control_state.ai_secret_source.as_str(),
        "settings": cognition_ai_settings_json(control_state)
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
            "The router's copies of the turns and the shape of the file's state remain unchanged while semantic capability is degraded.",
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
        "refusals_total": runtime_state.refusals_total,
        "refused_turns_dropped_total": runtime_state.refused_turns_dropped_total,
        "refused_turns_not_analyzed_total": runtime_state.refused_turns_not_analyzed_total,
        "agent_waits_total": runtime_state.agent_waits_total,
        "agent_waits_grace_over_total": runtime_state.agent_waits_grace_over_total,
        "relay_turns_total": runtime_state.relay_turns_total,
        "duplicate_turns_skipped_total": runtime_state.duplicate_turns_skipped_total,
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
        "active_feelings_total": runtime_state.active_feelings_total,
        "semantic_tagger_calls_total": runtime_state.semantic_tagger_calls_total,
        "semantic_tagger_failures_total": runtime_state.semantic_tagger_failures_total,
        "semantic_tagger_invalid_outputs_total": runtime_state.semantic_tagger_invalid_outputs_total,
        "semantic_tagger_provider_errors_total": runtime_state.semantic_tagger_provider_errors_total,
        "last_semantic_model": runtime_state.last_semantic_model,
        "last_semantic_impl": runtime_state.last_semantic_impl,
        "narrative_summarizer_calls_total": runtime_state.narrative_summarizer_calls_total,
        "narrative_summarizer_failures_total": runtime_state.narrative_summarizer_failures_total,
        "narrative_summarizer_invalid_outputs_total": runtime_state.narrative_summarizer_invalid_outputs_total,
        "narrative_summarizer_provider_errors_total": runtime_state.narrative_summarizer_provider_errors_total,
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
        "conversations_taken_total": runtime_state.conversations_taken_total,
        "writes_total": runtime_state.store_writes_total,
        "write_failures_total": runtime_state.store_write_failures_total
    });
    let episodes = json!({
        "live_total": runtime_state.episodes_live_total,
        "made_total": runtime_state.episodes_made_total,
        "gated_out_total": runtime_state.episodes_gated_out_total,
        "received_total": runtime_state.episodes_received_total,
        "refused_total": runtime_state.episodes_refused_total
    });
    let clears = json!({
        "total": runtime_state.clears_total,
        "last_at": runtime_state.last_clear_at
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
        "episodes": episodes,
        "clears": clears,
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
            "AI credentials live in SY.vault. The installation's AI provider selects resource_type=openai|anthropic."
                .to_string(),
        ),
        Value::String(
            "The provider and the model are read at the start from the installation file, or from this hive's hive.yaml while it holds none (ai.settings says which); settings that cannot be used skip every turn's semantic work with their reason (ai.settings.error)."
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
            "resolved_from": format!("vault://resource_type={}", control_state.semantic_tagger.provider),
            "settings": cognition_ai_settings_json(control_state)
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
        "refusals_total": runtime_state.refusals_total,
        "refused_turns_dropped_total": runtime_state.refused_turns_dropped_total,
        "refused_turns_not_analyzed_total": runtime_state.refused_turns_not_analyzed_total,
        "agent_waits_total": runtime_state.agent_waits_total,
        "agent_waits_grace_over_total": runtime_state.agent_waits_grace_over_total,
        "store_loads_total": runtime_state.store_loads_total,
        "store_load_failures_total": runtime_state.store_load_failures_total,
        "conversations_taken_total": runtime_state.conversations_taken_total,
        "store_writes_total": runtime_state.store_writes_total,
        "store_write_failures_total": runtime_state.store_write_failures_total,
        "relay_turns_total": runtime_state.relay_turns_total,
        "duplicate_turns_skipped_total": runtime_state.duplicate_turns_skipped_total,
        "episodes_live_total": runtime_state.episodes_live_total,
        "episodes_made_total": runtime_state.episodes_made_total,
        "episodes_gated_out_total": runtime_state.episodes_gated_out_total,
        "episodes_received_total": runtime_state.episodes_received_total,
        "episodes_refused_total": runtime_state.episodes_refused_total,
        "clears_total": runtime_state.clears_total,
        "last_clear_at": runtime_state.last_clear_at,
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
        "active_feelings_total": runtime_state.active_feelings_total,
        "loaded_at_start_total": runtime_state.loaded_at_start_total,
        "shm_hot_threads_total": runtime_state.shm_hot_threads_total,
        "shm_pruned_threads_total": runtime_state.shm_pruned_threads_total,
        "shm_payload_bytes": runtime_state.shm_payload_bytes,
        "last_shm_sync_status": runtime_state.last_shm_sync_status,
        "last_shm_sync_at": runtime_state.last_shm_sync_at,
        "semantic_tagger_calls_total": runtime_state.semantic_tagger_calls_total,
        "semantic_tagger_failures_total": runtime_state.semantic_tagger_failures_total,
        "semantic_tagger_invalid_outputs_total": runtime_state.semantic_tagger_invalid_outputs_total,
        "semantic_tagger_provider_errors_total": runtime_state.semantic_tagger_provider_errors_total,
        "last_semantic_model": runtime_state.last_semantic_model,
        "last_semantic_impl": runtime_state.last_semantic_impl,
        "narrative_summarizer_calls_total": runtime_state.narrative_summarizer_calls_total,
        "narrative_summarizer_failures_total": runtime_state.narrative_summarizer_failures_total,
        "narrative_summarizer_invalid_outputs_total": runtime_state.narrative_summarizer_invalid_outputs_total,
        "narrative_summarizer_provider_errors_total": runtime_state.narrative_summarizer_provider_errors_total,
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
    if control_state.ai_settings_error.is_some() {
        return "degraded_ai_settings_invalid";
    }
    match control_state.ai_secret_source {
        CognitionAiSecretSource::Missing => "degraded_no_ai_provider",
        CognitionAiSecretSource::LocalFile => "ready_semantic_ai",
    }
}

fn cognition_degraded_reasons(control_state: &CognitionControlState) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if control_state.ai_settings_error.is_some() {
        reasons.push("ai_settings_invalid");
    }
    if control_state.ai_secret_source == CognitionAiSecretSource::Missing {
        reasons.push("ai_provider_missing");
    }
    reasons
}

/// Where the provider and the model came from (docs/hive-config-update-v1.md §4), and why they
/// cannot be used when they cannot: `{from, version, sha256}` or `{from: "hive_yaml"}`, with
/// `error`.
fn cognition_ai_settings_json(control_state: &CognitionControlState) -> Value {
    let mut settings = match &control_state.ai_settings_source {
        Value::Object(_) => control_state.ai_settings_source.clone(),
        _ => json!({}),
    };
    if let Some(error) = &control_state.ai_settings_error {
        settings["error"] = json!(error);
    }
    settings
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
        ai_settings_source: Value::Null,
        ai_settings_error: None,
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
            "AI provider/model are the installation's: its installation file holds them \
             (docs/hive-config-update-v1.md §4), not this node's config"
                .into(),
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
            "config.semantic_tagger.provider/model are the installation's: its installation file \
             holds them (docs/hive-config-update-v1.md §4)"
                .into(),
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
    ask: &EpisodeAsk,
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
                    thread_state.feelings.get(&feeling_key(
                        owner_ilk,
                        &felt.affect.affect_id,
                        &felt.context_label,
                    ))
                })
                .max_by(|left, right| left.weight.total_cmp(&right.weight))
        })
        .map(|feeling| (feeling.summary.clone(), feeling.reason.clone()));
    let leading = leading_contexts(&thread_state.contexts);
    let (narrative_outcome, feeling_text) = update_memories_and_feelings_for_thread(
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
        ask,
        &leading,
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
            &mut thread_state.feelings,
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
async fn update_memories_and_feelings_for_thread<S: NarrativeSummarizer>(
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
    ask: &EpisodeAsk,
    leading: &[String],
    previous_feeling_text: Option<(&str, &str)>,
    memories: &mut HashMap<String, MemoryState>,
    narrative: &S,
) -> (NarrativeOutcome, Option<FeelingNarrative>) {
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
            feeling_affects: felt
                .iter()
                .map(|felt| felt.affect.affect_id.as_str())
                .collect(),
            feeling_titles: felt.iter().map(|felt| felt.affect.title.as_str()).collect(),
            previous_feeling_summary: previous_feeling_text.map(|(summary, _)| summary),
            previous_feeling_reason: previous_feeling_text.map(|(_, reason)| reason),
            counterpart: ask.counterpart.as_deref(),
            leading_contexts: leading,
            episode_habits: &ask.habits,
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
                "sy.cognition narrative summarizer failed; skipping memory/feeling synthesis for turn"
            );
            let failure = model_failure(&err);
            return (
                NarrativeOutcome {
                    called: true,
                    failed: failure == ModelFailure::Other,
                    invalid_output: failure == ModelFailure::InvalidOutput,
                    provider_error: failure == ModelFailure::Provider,
                    episode: None,
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
            provider_error: false,
            episode: narrative_result.episode,
        },
        narrative_result.feeling,
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
/// passes (`build_feeling_candidates`), felt around the active scope's context and reason. None
/// without an active scope, as a memory needs.
fn feeling_candidates(
    tagger: &SemanticTaggerOutput,
    active_scope: &Option<ScopeBindingState>,
) -> Vec<FeelingCandidate> {
    let Some(scope) = active_scope.as_ref() else {
        return Vec::new();
    };
    build_feeling_candidates(tagger, &scope.dominant_context_label)
        .into_iter()
        .map(|affect| FeelingCandidate {
            affect,
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
    text: Option<&FeelingNarrative>,
    feelings: &mut HashMap<String, FeelingState>,
) {
    let Some(owner_ilk) = owner.ilk.clone() else {
        return;
    };
    let felt: Vec<(String, FeelingCandidate)> = felt
        .into_iter()
        .map(|felt| {
            let key = feeling_key(&owner_ilk, &felt.affect.affect_id, &felt.context_label);
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
        let candidate = felt.affect;
        let feeling = feelings.entry(felt_key).or_insert_with(|| FeelingState {
            feeling_id: stable_entity_id(
                "feeling",
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

/// The deterministic feeling gate: each affect fires on its own combination of canonical and affect
/// signals, so one turn may show several (FINDINGS A-155). The summary and reason are the narrative
/// summarizer's.
fn build_feeling_candidates(
    tagger: &SemanticTaggerOutput,
    context_label: &str,
) -> Vec<AffectCandidate> {
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
                     evidence_strength: f64| AffectCandidate {
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
        recent_turn_of(message, TEST_AGENT_NODE, TEST_AGENT, 0, ts_ms, no_facts).expect("a turn")
    }

    /// A turn held with nothing but itself, as one held before 0.1.137.
    impl From<MemoryTurn> for HeldTurn {
        fn from(turn: MemoryTurn) -> Self {
            Self {
                turn,
                key: String::new(),
                requests: Vec::new(),
                refused: Vec::new(),
                relayed: None,
            }
        }
    }

    /// A turn's text, as a package carries the turn or as a communication holds it.
    trait TurnText {
        fn turn_text(&self) -> &str;
    }

    impl TurnText for MemoryTurn {
        fn turn_text(&self) -> &str {
            &self.text
        }
    }

    impl TurnText for HeldTurn {
        fn turn_text(&self) -> &str {
            &self.turn.text
        }
    }

    fn texts<T: TurnText>(turns: &[T]) -> Vec<&str> {
        turns.iter().map(TurnText::turn_text).collect()
    }

    /// Answers like the AI does, from the input: memory text always, feeling text exactly when
    /// the turn carries a feeling candidate (the contract `parse_narrative_summaries` enforces).
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
            let affects = input.feeling_affects.join(", ");
            let feeling = (!affects.is_empty()).then(|| FeelingNarrative {
                summary: format!("AI feeling summary ({affects})."),
                reason: format!("AI feeling reason ({affects})."),
            });
            async move {
                Ok(NarrativeSummaries {
                    memory_summary,
                    feeling,
                    episode: None,
                })
            }
        }
    }

    /// A narrator whose provider fails the way `0` makes it.
    struct FailingNarrative(fn() -> fluxbee_ai_sdk::AiSdkError);

    impl NarrativeSummarizer for FailingNarrative {
        fn summarize<'a>(
            &'a self,
            _input: NarrativeSummarizerAiInput<'a>,
        ) -> impl Future<Output = Result<NarrativeSummaries, fluxbee_ai_sdk::AiSdkError>> + Send + 'a
        {
            let err = (self.0)();
            async move { Err(err) }
        }
    }

    /// A provider's error is no invalid output: cognition does not judge it, and counts it apart
    /// (FINDINGS A-184). An answer that is not the object asked for is one.
    #[tokio::test]
    async fn a_provider_error_is_no_invalid_output() {
        let provider = || {
            fluxbee_ai_sdk::AiSdkError::Provider(fluxbee_ai_sdk::ProviderError {
                provider: fluxbee_ai_sdk::AiProvider::OpenAi,
                status: 429,
                error_type: "rate_limit_error".to_string(),
                param: None,
                request_id: None,
                message: "slow down".to_string(),
            })
        };
        let refused = || fluxbee_ai_sdk::AiSdkError::Refused {
            provider: fluxbee_ai_sdk::AiProvider::Anthropic,
            reason: "stop_reason=refusal",
        };
        let late = || fluxbee_ai_sdk::AiSdkError::Timeout("semantic tagger".to_string());
        let no_json = || {
            fluxbee_ai_sdk::AiSdkError::Protocol(
                "semantic tagger response missing JSON object".to_string(),
            )
        };
        let cut = || fluxbee_ai_sdk::AiSdkError::Truncated {
            provider: fluxbee_ai_sdk::AiProvider::OpenAi,
            limit: "max_output_tokens",
        };
        let empty = || fluxbee_ai_sdk::AiSdkError::NoAnswer {
            provider: fluxbee_ai_sdk::AiProvider::Anthropic,
            reason: "stop_reason=end_turn".to_string(),
        };
        for (make, kind) in [
            (
                provider as fn() -> fluxbee_ai_sdk::AiSdkError,
                ModelFailure::Provider,
            ),
            (refused, ModelFailure::Provider),
            (late, ModelFailure::Provider),
            (empty, ModelFailure::Provider),
            (no_json, ModelFailure::InvalidOutput),
            (cut, ModelFailure::InvalidOutput),
        ] {
            let err = make();
            assert_eq!(model_failure(&err), kind, "{err}");

            let mut thread = ThreadCognitionState::default();
            let tagger = SemanticTaggerOutput {
                tags: strings(&["billing"]),
                reason_signals_canonical: strings(&["resolve", "challenge"]),
                reason_signals_extra: strings(&["frustration"]),
                affect_signals: strings(&["frustration"]),
            };
            let juan = person("ilk:juan", "Juan");
            let outcome = update_thread_state(
                &key(TEST_THREAD),
                Some("ilk:juan"),
                Some(TEST_AGENT),
                Some(&juan),
                Some("Juan"),
                &tagger,
                &CognitionThresholds::default(),
                "test-key",
                &AiAccount::root(UsageRecorder::disabled(), "SY.cognition@motherbee"),
                &CognitionSemanticTaggerConfig::default(),
                "2026-01-01T00:01:00Z",
                &mut thread,
                &EpisodeAsk::default(),
                &FailingNarrative(make),
            )
            .await;
            assert!(outcome.called, "{err}");
            assert_eq!(
                (
                    outcome.provider_error,
                    outcome.invalid_output,
                    outcome.failed
                ),
                (
                    kind == ModelFailure::Provider,
                    kind == ModelFailure::InvalidOutput,
                    false
                ),
                "{err}"
            );
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
            &EpisodeAsk::default(),
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
        hold_recent_turns(
            threads,
            store,
            &mut Takes::default(),
            &mut Refusals::default(),
            recent.into_iter().map(Intake::Turn).collect(),
            &mut StoreLoads::default(),
            &mut IntakeTally::default(),
        )
    }

    /// No news, for the region's tests that are not about them.
    fn no_news(_: &CommunicationKey, _: &ThreadCognitionState) -> (Vec<NewsSummary>, u32) {
        (Vec::new(), 0)
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
            ai_settings_source: serde_json::json!({ "from": "hive_yaml" }),
            ai_settings_error: None,
        }
    }

    fn test_runtime_paths() -> RuntimePaths {
        RuntimePaths {
            state_dir: PathBuf::from("/var/lib/fluxbee/test"),
            store_path: PathBuf::from("/var/lib/fluxbee/test/cognition.db"),
        }
    }

    /// docs/hive-config-update-v1.md §4: cognition's provider and model are the installation
    /// file's once its hive holds one, whatever hive.yaml's `ai` says, and hive.yaml's until then;
    /// settings it cannot use are its degraded reason, never a fallback and never a stop.
    #[test]
    fn cognition_reads_the_installation_file_once_its_hive_holds_one() {
        let root = std::env::temp_dir().join(format!(
            "cognition-installation-{}",
            Uuid::new_v4().simple()
        ));
        let dist = root.join("dist");
        let file = fluxbee_ai_sdk::installation_file_path(&dist);
        fs::create_dir_all(file.parent().expect("its dir")).expect("dist");
        let hive = |ai: &str| -> HiveFile {
            serde_yaml::from_str(&format!(
                "hive_id: worker1\ndist:\n  path: \"{}\"\nai:\n{ai}",
                dist.display()
            ))
            .expect("an ai no reader uses still loads")
        };

        let (engine, source) = cognition_ai_settings(&hive(
            "  default_provider: openai\n  providers:\n    openai:\n      model: gpt-5.5\n",
        ))
        .expect("hive.yaml");
        assert_eq!(
            (engine.provider, engine.model.as_str()),
            (AiProvider::OpenAi, "gpt-5.5")
        );
        assert_eq!(source, json!({"from": "hive_yaml"}));

        let bytes = fluxbee_ai_sdk::InstallationFile {
            schema_version: fluxbee_ai_sdk::INSTALLATION_SCHEMA_VERSION,
            version: 1,
            ai: Some(
                serde_yaml::from_str(
                    "default_provider: anthropic\nproviders:\n  openai:\n    model: gpt-5.5\n  anthropic:\n    model: claude-sonnet-5-5\n",
                )
                .expect("ai"),
            ),
        }
        .render()
        .expect("renders");
        fs::write(&file, &bytes).expect("the file");
        let (engine, source) =
            cognition_ai_settings(&hive("  default_provider: gemini\n")).expect("the file");
        assert_eq!(
            (engine.provider, engine.model.as_str()),
            (AiProvider::Anthropic, "claude-sonnet-5-5")
        );
        assert_eq!(source["from"], json!("installation_file"));
        assert_eq!(source["version"], json!(1));

        fs::write(&file, b"{\"schema_version\":1,\"version\":0}").expect("broken");
        let reason = cognition_ai_settings(&hive(
            "  default_provider: openai\n  providers:\n    openai:\n      model: gpt-5.5\n",
        ))
        .expect_err("no fallback");
        assert!(reason.contains("version 0"), "{reason}");
        let mut control = test_control_state();
        control.ai_settings_error = Some(reason.clone());
        control.ai_secret_source = CognitionAiSecretSource::LocalFile;
        assert_eq!(
            cognition_state_label(&control),
            "degraded_ai_settings_invalid"
        );
        assert_eq!(
            cognition_degraded_reasons(&control),
            vec!["ai_settings_invalid"]
        );
        assert_eq!(cognition_ai_settings_json(&control)["error"], json!(reason));
        let _ = fs::remove_dir_all(&root);
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
        reference.turns = vec![taken(&person_says("third double charge"), TEST_TS)
            .turn
            .into()];

        let store = test_store();
        assert_eq!(
            store
                .save([(&key(TEST_THREAD), &reference)], &[])
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
        assert!(!back.feelings.is_empty());

        live_turn(&mut reference, 4, turn.0, turn.1, turn.2).await;
        live_turn(&mut back, 4, turn.0, turn.1, turn.2).await;
        assert_eq!(
            sorted_keys(&back.contexts),
            sorted_keys(&reference.contexts)
        );
        assert_eq!(
            sorted_keys(&back.feelings),
            sorted_keys(&reference.feelings)
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
            .save(
                [(&key(TEST_THREAD), &back), (&key("thread:older"), &older)],
                &[],
            )
            .expect("written");
        let recent = store.most_recent(usize::MAX, NO_AGENT_ILK).expect("read");
        let keys: Vec<&str> = recent
            .iter()
            .map(|(key, _)| key.thread_id.as_str())
            .collect();
        assert_eq!(keys, [TEST_THREAD, "thread:older"]);
        assert_eq!(recent[0].1.turn_count, 4);
        assert_eq!(
            store.most_recent(1, NO_AGENT_ILK).expect("read").len(),
            1,
            "a byte bound"
        );
        assert_eq!(store.stats().expect("stats").0, 2);
    }

    /// Whose agent served a thread last is the one with the newest turn, whenever its worker last
    /// saw it; a communication with no agent is not that, and is not loaded at start
    /// (docs/cognition-hands-v1.md §12.4).
    #[test]
    fn the_file_names_the_agent_with_the_newest_turn_and_loads_no_agentless_one() {
        let store = test_store();
        let turn_at = |text: &str, ts: &str| MemoryTurn {
            direction: MemoryTurnDirection::Out,
            src_ilk: None,
            via: None,
            writer: None,
            to: None,
            to_via: None,
            text: text.to_string(),
            attachments: 0,
            choice: None,
            ts: ts.to_string(),
        };
        // Sales' worker saw its communication last, late, but invoice's turn is the newer.
        let sales = ThreadCognitionState {
            agent_node: "AI.sales@motherbee".to_string(),
            last_seen_at: Some("2026-10-30T12:10:00Z".to_string()),
            turns: vec![turn_at("sales", "2026-10-30T12:00:00.000Z").into()],
            ..ThreadCognitionState::default()
        };
        let invoice = ThreadCognitionState {
            agent_node: "AI.invoice@motherbee".to_string(),
            turns: vec![turn_at("invoice", "2026-10-30T12:05:00.000Z").into()],
            ..ThreadCognitionState::default()
        };
        let people = ThreadCognitionState {
            turns: vec![turn_at("people", "2026-10-30T12:20:00.000Z").into()],
            ..ThreadCognitionState::default()
        };
        let key_of = |agent_ilk: &str| CommunicationKey {
            thread_id: TEST_THREAD.to_string(),
            agent_ilk: agent_ilk.to_string(),
        };
        store
            .save(
                [
                    (&key_of("ilk:sales"), &sales),
                    (&key_of("ilk:invoice"), &invoice),
                    (&key_of(NO_AGENT_ILK), &people),
                ],
                &[],
            )
            .expect("written");
        let (_, node) = store
            .last_agent_on_thread(TEST_THREAD, NO_AGENT_ILK)
            .expect("read")
            .expect("an agent");
        assert_eq!(node, "AI.invoice@motherbee");
        let mut loaded: Vec<String> = store
            .most_recent(usize::MAX, NO_AGENT_ILK)
            .expect("read")
            .into_iter()
            .map(|(key, _)| key.agent_ilk)
            .collect();
        loaded.sort();
        assert_eq!(loaded, ["ilk:invoice", "ilk:sales"]);
    }

    /// Only this node may read its file: the folder is 0700, and the file and its WAL sidecars
    /// 0600 (docs/cognition-hands-v1.md §11.2).
    #[test]
    fn the_file_is_cognitions_alone() {
        use std::os::unix::fs::PermissionsExt;
        let store = test_store();
        store
            .save([(&key(TEST_THREAD), &ThreadCognitionState::default())], &[])
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
            .save(
                [(&key("thread:good"), &ThreadCognitionState::default())],
                &[],
            )
            .expect("written");
        let side = rusqlite::Connection::open(store.path()).expect("a side connection");
        for (thread, version, state) in [
            ("thread:broken", 2, "{not json"),
            ("thread:older", 1, "{}"),
            ("thread:newer", 3, "{\"turn_count\": 9}"),
            ("thread:partial", 2, "{\"turn_count\": 3}"),
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
            .most_recent(usize::MAX, NO_AGENT_ILK)
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
        let hot_set = retain_memory_hot_set(&mut threads, &kept, &no_news).expect("hot set");
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
        take_recent_turns(
            &app_state,
            vec![taken(&person_says("first"), TEST_TS).into()],
        )
        .await;
        // The file fails: the communication is held, so its next turn needs no read.
        let side = rusqlite::Connection::open(app_state.store.path()).expect("a side connection");
        side.execute_batch("DROP TABLE communications")
            .expect("the table goes");
        take_recent_turns(
            &app_state,
            vec![taken(&person_says("second"), TEST_TS + 1_000).into()],
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
        take_recent_turns(&app_state, vec![taken(&other, TEST_TS + 2_000).into()]).await;
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
        stored.turns = vec![taken(&person_says("hello"), TEST_TS).turn.into()];
        store
            .save([(&key(TEST_THREAD), &stored)], &[])
            .expect("written");

        let mut threads = HashMap::new();
        let mut loads = StoreLoads::default();
        let mut other = person_says("anyone?");
        other.meta.thread_id = Some("thread:new".to_string());
        let touched = hold_recent_turns(
            &mut threads,
            &store,
            &mut Takes::default(),
            &mut Refusals::default(),
            vec![
                taken(&person_says("and the price?"), TEST_TS + 1_000).into(),
                taken(&other, TEST_TS + 2_000).into(),
            ],
            &mut loads,
            &mut IntakeTally::default(),
        );
        assert_eq!(touched, [key(TEST_THREAD), key("thread:new")]);
        assert_eq!((loads.loaded, loads.failed), (1, 0));
        let held = &threads[&key(TEST_THREAD)];
        assert_eq!(texts(&held.turns), ["hello", "and the price?"]);
        assert_eq!(held.turn_count, 1);
        let package = build_memory_package_for_thread(TEST_THREAD, held, (Vec::new(), 0));
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

        let hot_set = retain_memory_hot_set(&mut threads, &[], &no_news).expect("hot set");
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
        let full = retain_memory_hot_set(&mut threads, &[], &no_news).expect("hot set");
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
        let hot_set = retain_memory_hot_set(&mut threads, &[], &no_news).expect("hot set");
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
        let hot_set =
            retain_memory_hot_set(&mut threads, &[key("thread:turn")], &no_news).expect("hot set");
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
        assert_eq!(thread.feelings.len(), 1);
        let feeling = thread.feelings.values().next().expect("a feeling");
        assert_eq!(feeling.affect_id, "anger");
        assert_eq!(feeling.title, "Friction around billing");
        assert_eq!(feeling.summary, "AI feeling summary (anger).");
        assert_eq!(feeling.reason, "AI feeling reason (anger).");
        assert_eq!(feeling.base_intensity, 8.0);
        assert_eq!(feeling.evidence_strength, 9.0);
        assert!(feeling
            .evidence_signals
            .contains(&"frustration".to_string()));
    }

    #[test]
    fn the_feeling_gate_needs_its_signal_combination() {
        let tagger = |canonical: &[&str], affect: &[&str]| SemanticTaggerOutput {
            tags: strings(&["billing"]),
            reason_signals_canonical: strings(canonical),
            affect_signals: strings(affect),
            ..SemanticTaggerOutput::default()
        };
        let affects = |canonical: &[&str], affect: &[&str]| {
            build_feeling_candidates(&tagger(canonical, affect), "billing")
                .into_iter()
                .map(|candidate| candidate.affect_id)
                .collect::<Vec<_>>()
        };
        assert!(affects(&["inform"], &["frustration"]).is_empty());
        assert!(affects(&["resolve", "challenge"], &[]).is_empty());
        let urgency =
            build_feeling_candidates(&tagger(&["resolve", "request"], &["urgency"]), "billing");
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
        assert_eq!(thread.feelings.len(), 3);
        for affect in ["anger", "escalation"] {
            let shown = feeling(&thread, "ilk:ana", affect);
            assert_eq!(shown.weight, 2.0, "{affect}");
            assert_eq!(shown.summary, "AI feeling summary (anger, escalation).");
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
    ) -> &'a FeelingState {
        &thread.feelings[&feeling_key(owner, affect, "billing")]
    }

    /// A feeling is its owner's (docs/cognition-hands-v1.md §10.3): Ana's anger stays while Pedro
    /// talks, fades only with Ana's own calm turns, by the root of anger's inertia, and closes on the
    /// 64th of them, leaving the state (§11.2). It names her, the context and the step.
    #[tokio::test]
    async fn a_feeling_is_its_owners_and_fades_with_their_own_turns() {
        let (ana, pedro) = (person("ilk:ana", "Ana"), person("ilk:pedro", "Pedro"));
        let mut thread = ThreadCognitionState::default();
        says(&mut thread, Some(&ana), 1, ANGRY).await;
        assert_eq!(thread.feelings.len(), 1);
        let shown = feeling(&thread, "ilk:ana", "anger");
        assert_eq!(shown.owner, ana);
        assert_eq!(shown.context_label, "billing");
        assert_eq!(shown.weight, 2.0);
        assert_eq!(shown.summary, "AI feeling summary (anger).");

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
            let Some(anger) = thread.feelings.get(&anger_key) else {
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
        assert!(thread.feelings.is_empty());
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
        while thread.feelings.contains_key(&anger_key) {
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
        assert_eq!(thread.feelings.len(), 1);
        assert_eq!(feeling(&thread, "ilk:juan", "anger").weight, 2.0);
    }

    fn feeling_state(
        owner: &str,
        affect: &str,
        context: &str,
        weight: f64,
        felt: &str,
    ) -> FeelingState {
        FeelingState {
            feeling_id: format!("feeling:{owner}:{affect}:{context}"),
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
            thread.feelings.insert(feeling.feeling_id.clone(), feeling);
        }
        let older = feeling_state("ilk:tie", "urgency", "access", 13.5, "2026-01-01T00:00:00Z");
        let newer = feeling_state("ilk:tie", "anger", "access", 13.5, "2026-01-02T00:00:00Z");
        for feeling in [older, newer] {
            thread.feelings.insert(feeling.feeling_id.clone(), feeling);
        }

        let package = build_memory_package_for_thread(TEST_THREAD, &thread, (Vec::new(), 0));
        assert_eq!(package.package_version, MEMORY_PACKAGE_VERSION);
        assert_eq!(package.feelings.len(), COGNITION_PACKAGE_MAX_FEELINGS);
        let first = &package.feelings[0];
        assert_eq!(first.owner.ilk.as_deref(), Some("ilk:p13"));
        assert_eq!(first.weight, 14.0);
        assert_eq!(first.level, "strong");
        assert_eq!(
            first.summary.chars().count(),
            COGNITION_FEELING_TEXT_MAX_CHARS
        );
        assert_eq!(
            (
                package.feelings[1].affect.as_str(),
                package.feelings[2].affect.as_str()
            ),
            ("anger", "urgency"),
            "on a tie, the most recently felt first"
        );
        assert_eq!(
            package
                .truncated
                .as_ref()
                .map(|truncated| truncated.dropped_feelings),
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
        let mut answer = agent_says("we will sort it out");
        answer.meta.dst_ilk = Some("ilk:person".to_string());
        assert_eq!(
            narrator_speaker(&answer, TEST_AGENT_NODE, facts).as_deref(),
            Some("the agent, answering person Ana Pérez (ilk:person)")
        );
        assert_eq!(feeling_owner(&answer, TEST_AGENT_NODE, facts), None);
        let mut wf_text = person_says("Ana has not answered for 2 days");
        wf_text.meta.context = Some(json!({"writer": {"ilk": "ilk:wf"}}));
        assert_eq!(feeling_owner(&wf_text, TEST_AGENT_NODE, facts), None);
        let unknown = turn_copy("IO.api@motherbee", Some("ilk:new"), text("hello"));
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
            takes: std::sync::Mutex::new(Takes::default()),
            refusals: std::sync::Mutex::new(Refusals::default()),
            answers: std::sync::Mutex::new(Answers::default()),
            agents_answered: Notify::new(),
            vault: VaultClient::for_primary(
                Arc::clone(&dispatcher),
                VaultCallerOwned::new("ilk:cognition".to_string(), node_name),
            ),
            dispatcher,
            agent_ilks: Mutex::new(HashMap::from([(TEST_AGENT_NODE.to_string(), agent)])),
            usage: UsageRecorder::disabled(),
            stream_seen: Mutex::new(HashMap::new()),
            episodes: std::sync::Mutex::new(episodes::EpisodeBook::default()),
            clears: AtomicU64::new(0),
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
                    to_via: None,
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
                    to_via: None,
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

    /// A `STREAM_BATCH` as `turns_batch`, its records copied for `producer`.
    fn batch_of(hive_id: &str, producer: &str, records: &[(u64, u64, &Message)]) -> Message {
        let mut batch = turns_batch(hive_id, records);
        let mut payload: StreamBatch =
            serde_json::from_value(batch.payload.clone()).expect("batch");
        for record in &mut payload.records {
            record.producer = producer.to_string();
        }
        batch.payload = serde_json::to_value(payload).expect("batch");
        batch
    }

    /// What a WF relays from one person to another on `TEST_THREAD`: a forward to the other's IO
    /// node, which keeps its writer.
    fn relayed(writer_ilk: &str, content: &str) -> Message {
        let mut message = turn_copy(RELAY_WF, Some(writer_ilk), text(content));
        message.routing.dst = Destination::Unicast("IO.slack.team@motherbee".to_string());
        message.meta.context = Some(json!({"writer": {"ilk": writer_ilk}}));
        message
    }

    const RELAY_WF: &str = "WF.soporte@motherbee";

    /// The people's part of a handover (docs/cognition-hands-v1.md §12.4): what a WF relays between
    /// people on the thread joins the communication of the agent that served it here, each line
    /// naming its writer; and a message copied for the agent and relayed to a person too is one
    /// turn, analyzed once.
    #[tokio::test]
    async fn a_relay_between_people_joins_the_agents_communication_once() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogrelay");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let answer = agent_says("A person of the team takes your case.");
        let batch = turns_batch(&hive_id, &[(1, TEST_TS, &answer)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let mut forwarded = answer.clone();
        forwarded.routing.src_l2_name = Some(RELAY_WF.to_string());
        forwarded.routing.dst = Destination::Unicast("IO.api@motherbee".to_string());
        let juan = relayed("ilk:juan", "Hi Ana, I am issuing the credit note now.");
        let batch = batch_of(
            &hive_id,
            RELAY_WF,
            &[
                (2, TEST_TS + 1_000, &forwarded),
                (3, TEST_TS + 2_000, &juan),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        let first = turn_rx.try_recv().expect("the agent's answer");
        assert!(!first.relay && !first.duplicate);
        let again = turn_rx
            .try_recv()
            .expect("the answer, relayed to the person");
        assert!(again.relay && again.duplicate, "one message is one turn");
        let juans = turn_rx.try_recv().expect("Juan's message");
        assert!(juans.relay && !juans.duplicate);
        assert_eq!(
            juans.holder.as_ref().map(Holder::agent_node),
            Some(TEST_AGENT_NODE)
        );

        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(
            texts(&package.turns),
            [
                "A person of the team takes your case.",
                "Hi Ana, I am issuing the credit note now."
            ]
        );
        assert_eq!(package.turns[1].direction, MemoryTurnDirection::In);
        assert_eq!(
            package.turns[1]
                .writer
                .as_ref()
                .and_then(|writer| writer.ilk.as_deref()),
            Some("ilk:juan")
        );
        assert_eq!(
            package.turns[1].to_via.as_deref(),
            Some("IO.slack.team@motherbee")
        );
        assert_eq!(package.turns[0].to_via, None, "the agent's own answer");
        assert_eq!(
            app_state
                .runtime_state
                .lock()
                .await
                .duplicate_turns_skipped_total,
            1
        );
        unlink();
    }

    /// A WF that sends one message to the agent and to a person in one cycle puts both copies in one
    /// batch, in either order: the relay is the agent's turn once, analyzed once, and makes no
    /// communication with no agent (docs/cognition-hands-v1.md §12.4).
    #[tokio::test]
    async fn one_message_a_wf_sends_an_agent_and_a_person_is_one_turn() {
        for relay_first in [true, false] {
            let dir = TestDir::new();
            let (hive_id, region, unlink) = test_region("cogboth");
            let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
            let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

            let mut to_agent = person_says("Hi, my invoice 123 was charged twice.");
            to_agent.routing.src_l2_name = Some(RELAY_WF.to_string());
            let mut to_person = to_agent.clone();
            to_person.routing.dst = Destination::Unicast("IO.slack.team@motherbee".to_string());
            let (first, second) = if relay_first {
                (&to_person, &to_agent)
            } else {
                (&to_agent, &to_person)
            };
            let mut batch = turns_batch(&hive_id, &[(1, TEST_TS, first), (2, TEST_TS, second)]);
            let mut payload: StreamBatch =
                serde_json::from_value(batch.payload.clone()).expect("batch");
            payload.records[usize::from(!relay_first)].producer = RELAY_WF.to_string();
            batch.payload = serde_json::to_value(payload).expect("batch");
            take_stream_batch(&app_state, &turn_tx, &batch).await;
            next_outgoing(&mut router).await;

            let mut queued = [
                turn_rx.try_recv().expect("a copy"),
                turn_rx.try_recv().expect("the other copy"),
            ];
            queued.sort_by_key(|turn| turn.relay);
            let [agents, relay] = queued;
            assert!(
                !agents.relay && !agents.duplicate,
                "relay first: {relay_first}"
            );
            assert!(relay.relay && relay.duplicate, "relay first: {relay_first}");
            assert_eq!(
                relay.holder.as_ref().map(Holder::agent_node),
                Some(TEST_AGENT_NODE)
            );
            let no_agent = CommunicationKey {
                thread_id: TEST_THREAD.to_string(),
                agent_ilk: NO_AGENT_ILK.to_string(),
            };
            assert!(app_state.store.load(&no_agent).expect("read").is_none());
            let package = region_package(&app_state)
                .await
                .expect("the agent's package");
            assert_eq!(
                texts(&package.turns),
                ["Hi, my invoice 123 was charged twice."]
            );
            assert_eq!(package.turns[0].to_via, None, "relay first: {relay_first}");
            unlink();
        }
    }

    fn no_agent_key() -> CommunicationKey {
        CommunicationKey {
            thread_id: TEST_THREAD.to_string(),
            agent_ilk: NO_AGENT_ILK.to_string(),
        }
    }

    /// The first agent on a thread takes what people said there before it (docs/cognition-hands-v1.md
    /// §12.6, the operator's call of 2026-11-07): the communication with no agent becomes the
    /// agent's, each line naming its writer, and leaves the file in the write of the agent's, whether
    /// it was held or only in the file.
    #[tokio::test]
    async fn the_first_agent_on_a_thread_takes_what_people_said_there() {
        for from_the_file in [false, true] {
            let dir = TestDir::new();
            let (hive_id, region, unlink) = test_region("cogtake");
            let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
            let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

            let ana = relayed("ilk:ana", "Juan, the credit note never came.");
            let juan = relayed("ilk:juan", "Ana, I am passing it to billing.");
            let batch = batch_of(
                &hive_id,
                RELAY_WF,
                &[(1, TEST_TS, &ana), (2, TEST_TS + 1_000, &juan)],
            );
            take_stream_batch(&app_state, &turn_tx, &batch).await;
            next_outgoing(&mut router).await;
            assert!(app_state
                .store
                .load(&no_agent_key())
                .expect("read")
                .is_some());
            if from_the_file {
                app_state.thread_states.lock().await.remove(&no_agent_key());
            }

            let to_agent = person_says("Is my credit note coming?");
            let batch = turns_batch(&hive_id, &[(3, TEST_TS + 2_000, &to_agent)]);
            take_stream_batch(&app_state, &turn_tx, &batch).await;
            next_outgoing(&mut router).await;

            let lines = [
                "Juan, the credit note never came.",
                "Ana, I am passing it to billing.",
                "Is my credit note coming?",
            ];
            {
                let threads = app_state.thread_states.lock().await;
                assert!(
                    !threads.contains_key(&no_agent_key()),
                    "from the file: {from_the_file}"
                );
                assert_eq!(texts(&threads[&key(TEST_THREAD)].turns), lines);
            }
            assert!(app_state
                .store
                .load(&no_agent_key())
                .expect("read")
                .is_none());
            let stored = app_state
                .store
                .load(&key(TEST_THREAD))
                .expect("read")
                .expect("the agent's, in the file");
            assert_eq!(texts(&stored.turns), lines);
            {
                let takes = app_state.takes.lock().expect("takes");
                assert!(takes.removals.is_empty(), "deleted with the agent's write");
                assert_eq!(takes.taker_of(&no_agent_key()), Some(&key(TEST_THREAD)));
            }
            assert_eq!(
                app_state
                    .runtime_state
                    .lock()
                    .await
                    .conversations_taken_total,
                1
            );
            let package = region_package(&app_state)
                .await
                .expect("the agent's package");
            assert_eq!(texts(&package.turns), lines);
            assert_eq!(
                package.turns[0]
                    .writer
                    .as_ref()
                    .and_then(|writer| writer.ilk.as_deref()),
                Some("ilk:ana")
            );
            unlink();
        }
    }

    /// A WF's one message to a person and to the agent, in two batches on a new thread, the relay
    /// first (§12.6's third question): the agent takes the relay's communication with it, so its copy
    /// is the same turn, analyzed once, and no communication with no agent is left.
    #[tokio::test]
    async fn a_relay_and_its_copy_in_two_batches_on_a_new_thread_are_one_turn() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogtwo");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let mut to_agent = person_says("Hi, my invoice 123 was charged twice.");
        to_agent.routing.src_l2_name = Some(RELAY_WF.to_string());
        let mut to_person = to_agent.clone();
        to_person.routing.dst = Destination::Unicast("IO.slack.team@motherbee".to_string());
        let batch = batch_of(&hive_id, RELAY_WF, &[(1, TEST_TS, &to_person)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let relay = turn_rx.try_recv().expect("the relay");
        assert!(relay.relay && !relay.duplicate);
        assert!(matches!(relay.holder, Some(Holder::NoAgent)));

        let batch = turns_batch(&hive_id, &[(2, TEST_TS, &to_agent)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let copy = turn_rx.try_recv().expect("the agent's copy");
        assert!(!copy.relay && copy.duplicate, "one message is one turn");

        assert!(!app_state
            .thread_states
            .lock()
            .await
            .contains_key(&no_agent_key()));
        assert!(app_state
            .store
            .load(&no_agent_key())
            .expect("read")
            .is_none());
        // The communication that took it is in the file, so a restart keeps it.
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("the agent's, in the file");
        assert_eq!(stored.agent_node, TEST_AGENT_NODE);
        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(
            texts(&package.turns),
            ["Hi, my invoice 123 was charged twice."]
        );
        assert_eq!(package.turns[0].to_via, None, "said to the agent too");
        // A later relay on the thread finds the agent.
        assert_eq!(
            holder_of(&app_state, TEST_THREAD, RELAY_WF)
                .await
                .as_ref()
                .map(Holder::agent_node),
            Some(TEST_AGENT_NODE)
        );
        unlink();
    }

    /// The worker's turn of a communication an agent took before the worker got to it is the agent's:
    /// its hold follows the recorded take, so the communication with no agent is never made again,
    /// and its result is written to the agent's.
    #[tokio::test]
    async fn the_worker_follows_a_take_to_the_agent_that_took_its_turn() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogfollow");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let ana = relayed("ilk:ana", "Juan, the credit note never came.");
        let batch = batch_of(&hive_id, RELAY_WF, &[(1, TEST_TS, &ana)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        turn_rx.try_recv().expect("the relay");

        let to_agent = person_says("Is my credit note coming?");
        let batch = turns_batch(&hive_id, &[(2, TEST_TS + 1_000, &to_agent)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        let mut loads = StoreLoads::default();
        let target = hold_for_worker(&app_state, &no_agent_key(), "", 0, &mut loads)
            .await
            .expect("held");
        assert_eq!(target, Some(key(TEST_THREAD)));
        assert!(!app_state
            .thread_states
            .lock()
            .await
            .contains_key(&no_agent_key()));
        assert_eq!(
            taker_holder(&app_state, &key(TEST_THREAD))
                .await
                .as_ref()
                .map(Holder::agent_ilk),
            Some(TEST_AGENT),
            "the agent's turn now, paid by its tenant"
        );

        // Every turn of the taken communication is the agent's, whatever the take carried.
        assert_eq!(
            app_state
                .takes
                .lock()
                .expect("takes")
                .follow(&no_agent_key()),
            key(TEST_THREAD)
        );

        let mut analyzed = app_state.thread_states.lock().await[&key(TEST_THREAD)].clone();
        analyzed.turn_count += 1;
        let (written_back, synced) =
            write_back_worker_turn(&app_state, &no_agent_key(), analyzed, 0, "").await;
        assert_eq!(written_back, WriteBack::Written);
        synced.expect("the region written");
        let threads = app_state.thread_states.lock().await;
        assert!(!threads.contains_key(&no_agent_key()), "never made again");
        assert_eq!(threads[&key(TEST_THREAD)].turn_count, 1);
        drop(threads);
        assert!(app_state
            .store
            .load(&no_agent_key())
            .expect("read")
            .is_none());
        unlink();
    }

    /// A take that comes while the models answer a turn of the communication with no agent (the usual
    /// timing: a WF's relay, then the agent's copy in the next batch): the worker's result is written
    /// to the agent's communication, with the agent's own turn and node kept, and the one with no
    /// agent is not made again.
    #[tokio::test]
    async fn a_take_while_the_models_answer_sends_the_result_to_the_agent_that_took_it() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogmidtake");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let ana = relayed("ilk:ana", "Juan, the credit note never came.");
        let batch = batch_of(&hive_id, RELAY_WF, &[(1, TEST_TS, &ana)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        turn_rx.try_recv().expect("the relay");

        // The worker holds the relay's communication and copies it, as `handle_turn` does.
        *app_state.worker_turn.lock().await = Some(no_agent_key());
        let mut loads = StoreLoads::default();
        let target = hold_for_worker(&app_state, &no_agent_key(), "", 0, &mut loads)
            .await
            .expect("held");
        assert_eq!(target, Some(no_agent_key()));
        let mut analyzed = app_state.thread_states.lock().await[&no_agent_key()].clone();

        // The agent's turn comes meanwhile, and takes it.
        let to_agent = person_says("Is my credit note coming?");
        let batch = turns_batch(&hive_id, &[(2, TEST_TS + 1_000, &to_agent)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        analyzed.turn_count += 1;
        let (written_back, synced) =
            write_back_worker_turn(&app_state, &no_agent_key(), analyzed, 0, "").await;
        assert_eq!(written_back, WriteBack::Written);
        synced.expect("the region written");
        {
            let threads = app_state.thread_states.lock().await;
            assert!(!threads.contains_key(&no_agent_key()), "never made again");
            let agents = &threads[&key(TEST_THREAD)];
            assert_eq!(agents.turn_count, 1, "the worker's result");
            assert_eq!(
                texts(&agents.turns),
                [
                    "Juan, the credit note never came.",
                    "Is my credit note coming?"
                ]
            );
            assert_eq!(agents.agent_node, TEST_AGENT_NODE);
        }
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("the agent's, in the file");
        assert_eq!(stored.turn_count, 1);
        assert!(region_package(&app_state).await.is_some());
        unlink();
    }

    /// A take the worker makes, for a turn the intake left out, is written at once with the taken
    /// row's deletion: it never lives only in memory, so a restart loses nothing.
    #[tokio::test]
    async fn a_take_the_worker_makes_is_written_with_the_deletion() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogwrktk");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let ana = relayed("ilk:ana", "Juan, the credit note never came.");
        let batch = batch_of(&hive_id, RELAY_WF, &[(1, TEST_TS, &ana)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        app_state.thread_states.lock().await.remove(&no_agent_key());

        let mut loads = StoreLoads::default();
        let target = hold_for_worker(
            &app_state,
            &key(TEST_THREAD),
            TEST_AGENT_NODE,
            0,
            &mut loads,
        )
        .await
        .expect("held");
        assert_eq!(target, Some(key(TEST_THREAD)));
        assert_eq!(loads.taken, 1);
        {
            let threads = app_state.thread_states.lock().await;
            let agents = &threads[&key(TEST_THREAD)];
            assert_eq!(texts(&agents.turns), ["Juan, the credit note never came."]);
            assert_eq!(agents.agent_node, TEST_AGENT_NODE);
        }
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("the agent's, written at once");
        assert_eq!(texts(&stored.turns), ["Juan, the credit note never came."]);
        assert!(app_state
            .store
            .load(&no_agent_key())
            .expect("read")
            .is_none());
        assert!(app_state.takes.lock().expect("takes").removals.is_empty());
        unlink();
    }

    /// A communication with no agent whose row cannot be read takes nothing and blocks nothing: the
    /// agent's communication starts without it and keeps its turn, and the row stays.
    #[tokio::test]
    async fn an_unreadable_conversation_with_no_agent_takes_nothing_and_blocks_nothing() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogunread");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        app_state
            .store
            .put_row(&no_agent_key(), 99, "{}")
            .expect("a row of a newer build");

        let to_agent = person_says("Is my credit note coming?");
        let batch = turns_batch(&hive_id, &[(1, TEST_TS, &to_agent)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(texts(&package.turns), ["Is my credit note coming?"]);
        assert!(
            app_state.store.load(&no_agent_key()).is_err(),
            "the row stays"
        );
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.conversations_taken_total, 0);
        assert_eq!(state.store_load_failures_total, 1);
        drop(state);
        unlink();
    }

    /// Only an agent's new communication takes the thread's people, once: an agent's communication
    /// already held takes nothing, nor does one with no agent. The taken row is not read again, and
    /// leaves the file with the next write.
    #[test]
    fn only_an_agents_new_communication_takes_the_threads_people() {
        let store = test_store();
        let people = ThreadCognitionState {
            turns: vec![taken(&person_says("hello"), TEST_TS).turn.into()],
            turn_keys: vec!["k1".to_string()],
            ..ThreadCognitionState::default()
        };
        store
            .save([(&no_agent_key(), &people)], &[])
            .expect("written");
        let mut threads = HashMap::new();
        let mut takes = Takes::default();
        let mut loads = StoreLoads::default();
        let held = held_or_loaded(
            &mut threads,
            &store,
            &mut takes,
            &key(TEST_THREAD),
            &mut loads,
        )
        .expect("held");
        assert_eq!(texts(&held.turns), ["hello"]);
        assert_eq!(loads.taken, 1);
        assert!(takes.removal_pending(&no_agent_key()));
        assert_eq!(takes.follow(&no_agent_key()), key(TEST_THREAD));
        // Its row is not read again while it waits to be deleted.
        let mut fresh = HashMap::new();
        let again = held_or_loaded(&mut fresh, &store, &mut takes, &no_agent_key(), &mut loads)
            .expect("held");
        assert!(again.turns.is_empty());
        store
            .save(
                threads.get_key_value(&key(TEST_THREAD)),
                &takes.removals.clone(),
            )
            .expect("written");
        assert!(store.load(&no_agent_key()).expect("read").is_none());

        // An agent's held communication takes nothing, nor does one with no agent.
        takes.removals.clear();
        store
            .save([(&no_agent_key(), &people)], &[])
            .expect("written");
        held_or_loaded(
            &mut threads,
            &store,
            &mut takes,
            &key(TEST_THREAD),
            &mut loads,
        )
        .expect("held");
        let mut fresh = HashMap::new();
        held_or_loaded(&mut fresh, &store, &mut takes, &no_agent_key(), &mut loads).expect("held");
        assert_eq!(loads.taken, 1);
        assert!(store.load(&no_agent_key()).expect("read").is_some());
    }

    /// A turn of a communication the first agent took is the taker's, whatever the intake found its
    /// holder to be (a relay whose agent the identity memory lost, say): the communication with no
    /// agent is never made again.
    #[test]
    fn a_turn_of_a_taken_communication_is_the_takers() {
        let store = test_store();
        let mut threads = HashMap::new();
        threads.insert(
            key(TEST_THREAD),
            ThreadCognitionState {
                agent_node: TEST_AGENT_NODE.to_string(),
                ..ThreadCognitionState::default()
            },
        );
        let mut takes = Takes::default();
        takes.record(Take {
            taken: no_agent_key(),
            taker: key(TEST_THREAD),
        });
        let mut relay = taken(&relayed("ilk:ana", "one more thing"), TEST_TS);
        relay.key = no_agent_key();
        relay.agent_node = String::new();
        let touched = hold_recent_turns(
            &mut threads,
            &store,
            &mut takes,
            &mut Refusals::default(),
            vec![relay.into()],
            &mut StoreLoads::default(),
            &mut IntakeTally::default(),
        );
        assert_eq!(touched, vec![key(TEST_THREAD)]);
        assert!(!threads.contains_key(&no_agent_key()));
        assert_eq!(texts(&threads[&key(TEST_THREAD)].turns), ["one more thing"]);
        assert_eq!(threads[&key(TEST_THREAD)].agent_node, TEST_AGENT_NODE);
    }

    /// Only the first agent on a thread takes what people said there: beside another agent's
    /// communication, held or in the file, they stay with no agent (§12.6's first question).
    #[test]
    fn only_the_first_agent_on_a_thread_takes_its_people() {
        for in_the_file in [false, true] {
            let store = test_store();
            let mut threads = HashMap::new();
            threads.insert(
                no_agent_key(),
                ThreadCognitionState {
                    turns: vec![taken(&person_says("hello"), TEST_TS).turn.into()],
                    ..ThreadCognitionState::default()
                },
            );
            let invoice = CommunicationKey {
                thread_id: TEST_THREAD.to_string(),
                agent_ilk: "ilk:invoice".to_string(),
            };
            if in_the_file {
                store
                    .save([(&invoice, &ThreadCognitionState::default())], &[])
                    .expect("written");
            } else {
                threads.insert(invoice, ThreadCognitionState::default());
            }
            let mut takes = Takes::default();
            let mut loads = StoreLoads::default();
            let held = held_or_loaded(
                &mut threads,
                &store,
                &mut takes,
                &key(TEST_THREAD),
                &mut loads,
            )
            .expect("held");
            assert!(held.turns.is_empty(), "in the file: {in_the_file}");
            assert_eq!(loads.taken, 0);
            assert!(threads.contains_key(&no_agent_key()));
            assert!(takes.recent.is_empty() && takes.removals.is_empty());
        }
    }

    /// A row written again in the transaction that deletes it stays: the deletions go first.
    #[test]
    fn a_row_written_again_in_the_transaction_that_deletes_it_stays() {
        let store = test_store();
        store
            .save([(&no_agent_key(), &ThreadCognitionState::default())], &[])
            .expect("written");
        let again = ThreadCognitionState {
            turn_count: 3,
            ..ThreadCognitionState::default()
        };
        store
            .save([(&no_agent_key(), &again)], &[no_agent_key()])
            .expect("written");
        assert_eq!(
            store
                .load(&no_agent_key())
                .expect("read")
                .expect("kept")
                .turn_count,
            3
        );
    }

    /// A write-back to a communication let go from memory loads it from the file first: its turns
    /// are merged, never written over.
    #[tokio::test]
    async fn a_write_back_to_a_communication_let_go_from_memory_keeps_its_file_row() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogwbload");
        let (app_state, _router) = test_app_state(&hive_id, Some(region), &dir.0);
        let stored = ThreadCognitionState {
            agent_node: TEST_AGENT_NODE.to_string(),
            turns: vec![taken(&person_says("kept in the file"), TEST_TS).turn.into()],
            ..ThreadCognitionState::default()
        };
        app_state
            .store
            .save([(&key(TEST_THREAD), &stored)], &[])
            .expect("written");
        let analyzed = ThreadCognitionState {
            turn_count: 1,
            ..ThreadCognitionState::default()
        };
        let (written_back, synced) =
            write_back_worker_turn(&app_state, &key(TEST_THREAD), analyzed, 0, "").await;
        assert_eq!(written_back, WriteBack::Written);
        synced.expect("the region written");
        let back = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("its row");
        assert_eq!(texts(&back.turns), ["kept in the file"]);
        assert_eq!(back.turn_count, 1);
        assert_eq!(back.agent_node, TEST_AGENT_NODE);
        unlink();
    }

    /// A clear drops the takes with everything else: no row is left to delete, nor a communication a
    /// turn could follow to.
    #[tokio::test]
    async fn a_clear_drops_the_takes_too() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogclrtk");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        app_state.takes.lock().expect("takes").record(Take {
            taken: no_agent_key(),
            taker: key(TEST_THREAD),
        });
        take_clear(&app_state, &clear_from(Some("SY.admin@motherbee"), false)).await;
        next_outgoing(&mut router).await;
        let takes = app_state.takes.lock().expect("takes");
        assert!(takes.recent.is_empty() && takes.removals.is_empty());
        drop(takes);
        unlink();
    }

    /// The two copies of one message a WF forwards to an IO node and to the agent can come in two
    /// batches, the relay first: once the agent's copy comes, the turn is the agent's and names no
    /// other place, and the worker's copy taken before does not bring the relay back
    /// (docs/cognition-hands-v1.md §12.9).
    #[tokio::test]
    async fn the_agents_copy_after_its_relay_makes_the_turn_the_agents() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogcc");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let key = CommunicationKey {
            thread_id: TEST_THREAD.to_string(),
            agent_ilk: TEST_AGENT.to_string(),
        };

        let answer = agent_says("Hello, how can I help?");
        let batch = turns_batch(&hive_id, &[(1, TEST_TS, &answer)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let mut to_agent = person_says("My invoice 123 was charged twice.");
        to_agent.routing.src_l2_name = Some(RELAY_WF.to_string());
        let mut to_team = to_agent.clone();
        to_team.routing.dst = Destination::Unicast("IO.slack.team@motherbee".to_string());
        let batch = batch_of(&hive_id, RELAY_WF, &[(2, TEST_TS + 1_000, &to_team)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let worker_copy = app_state
            .thread_states
            .lock()
            .await
            .get(&key)
            .cloned()
            .expect("held");
        assert_eq!(
            worker_copy.turns[1].turn.to_via.as_deref(),
            Some("IO.slack.team@motherbee")
        );

        let batch = turns_batch(&hive_id, &[(3, TEST_TS + 1_001, &to_agent)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let queued: Vec<CognitionTurn> = std::iter::from_fn(|| turn_rx.try_recv().ok()).collect();
        assert_eq!(
            queued
                .iter()
                .map(|turn| (turn.relay, turn.duplicate))
                .collect::<Vec<_>>(),
            [(false, false), (true, false), (false, true)]
        );
        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(
            texts(&package.turns),
            [
                "Hello, how can I help?",
                "My invoice 123 was charged twice."
            ]
        );
        assert_eq!(
            (&package.turns[1].to, &package.turns[1].to_via),
            (&None, &None)
        );

        write_back_turn(
            &mut *app_state.thread_states.lock().await,
            &key,
            worker_copy,
            &Refusals::default(),
        );
        let held = app_state
            .thread_states
            .lock()
            .await
            .get(&key)
            .cloned()
            .expect("held");
        assert_eq!(
            texts(&held.turns),
            [
                "Hello, how can I help?",
                "My invoice 123 was charged twice."
            ]
        );
        assert_eq!(held.turns[1].turn.to_via, None);
        unlink();
    }

    /// The agent's own answer that a WF relays in a later cycle, under that cycle's trace, is the
    /// answer its communication holds, not a second turn (docs/cognition-hands-v1.md §12.4).
    #[tokio::test]
    async fn the_agents_answer_relayed_in_a_later_cycle_is_not_a_second_turn() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogecho");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let answer = agent_says("Your credit note is approved.");
        let batch = turns_batch(&hive_id, &[(1, TEST_TS, &answer)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        // A supervisor approved it, and the WF sends it on in another cycle: another trace.
        let mut later = answer.clone();
        later.routing.trace_id = Uuid::new_v4().to_string();
        later.routing.src_l2_name = Some(RELAY_WF.to_string());
        later.routing.dst = Destination::Unicast("IO.api@motherbee".to_string());
        later.meta.context = Some(json!({"writer": {"ilk": TEST_AGENT}}));
        let batch = batch_of(&hive_id, RELAY_WF, &[(2, TEST_TS + 60_000, &later)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        assert!(!turn_rx.try_recv().expect("the answer").duplicate);
        assert!(turn_rx.try_recv().expect("its relay").duplicate);
        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(texts(&package.turns), ["Your credit note is approved."]);
        unlink();
    }

    /// A conversation's people are all those its turns name, however many: the news of each reaches
    /// its package (docs/cognition-hands-v1.md §12.4). An episode is about four at most.
    #[test]
    fn a_conversations_people_are_all_it_names() {
        let mut state = ThreadCognitionState::default();
        for n in 1..=6 {
            let ilk = format!("ilk:p{n}");
            state.turns.push(
                MemoryTurn {
                    direction: MemoryTurnDirection::In,
                    src_ilk: Some(ilk.clone()),
                    via: None,
                    writer: Some(MemoryTurnParty {
                        ilk: Some(ilk),
                        name: None,
                        kind: Some("person".to_string()),
                    }),
                    to: None,
                    to_via: None,
                    text: format!("turn {n}"),
                    attachments: 0,
                    choice: None,
                    ts: "2026-10-30T12:00:00.000Z".to_string(),
                }
                .into(),
            );
        }
        assert_eq!(people_of_conversation(&state, TEST_AGENT).len(), 6);
        assert_eq!(
            episodes::people_of(["ilk:a", "ilk:b", "ilk:c", "ilk:d", "ilk:e"]).len(),
            4
        );
    }

    /// A relay on a thread no agent served on this hive (docs/cognition-hands-v1.md §12.4): its
    /// turns make the communication with no agent, which the file keeps and the region leaves out,
    /// and each turn's writer pays.
    #[tokio::test]
    async fn a_relay_on_a_thread_with_no_agent_is_its_writers_to_pay() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogpeers");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let mut juan = relayed("ilk:juan", "Pedro, the note is out.");
        juan.meta.thread_id = Some("thread:people".to_string());
        let batch = batch_of(&hive_id, RELAY_WF, &[(1, TEST_TS, &juan)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let queued = turn_rx.try_recv().expect("the turn");
        assert!(matches!(queued.holder, Some(Holder::NoAgent)));

        let no_agent = CommunicationKey {
            thread_id: "thread:people".to_string(),
            agent_ilk: NO_AGENT_ILK.to_string(),
        };
        let stored = app_state
            .store
            .load(&no_agent)
            .expect("read")
            .expect("in the file");
        assert_eq!(texts(&stored.turns), ["Pedro, the note is out."]);
        let snapshot = app_state
            .memory_region
            .lock()
            .await
            .as_ref()
            .expect("a region")
            .read_snapshot()
            .expect("snapshot");
        assert!(snapshot
            .threads
            .iter()
            .all(|entry| entry.thread_id != "thread:people"));
        assert_eq!(
            app_state
                .runtime_state
                .lock()
                .await
                .shm_pruned_threads_total,
            0,
            "a communication with no agent is not pruned from the region"
        );

        let payer = payer_of(&hive_id, &Holder::NoAgent, &juan).expect("a payer");
        assert_eq!(payer.source, TenantSource::Writer);
        assert_eq!(payer.ilk, "ilk:juan");
        unlink();
    }

    fn credit_note_episode() -> episodes::EpisodeProposal {
        episodes::EpisodeProposal {
            lens: "friction".to_string(),
            title: "Argument over the credit note".to_string(),
            summary: "The person and Juan argued over the credit note's delay. It is pending."
                .to_string(),
            state: "pending".to_string(),
            base_intensity: 8.0,
            novelty: 0.0,
            evidence_signals: strings(&["escalation"]),
            evidence_strength: 9.0,
            evidence_contexts: strings(&["credit note"]),
        }
    }

    /// An episode the narrator proposes (docs/cognition-hands-v1.md §12.4): through the gate, it is
    /// kept in memory and in the file, sent to every hive's cognition, and in the package of each
    /// conversation of its people. One the gate refuses goes nowhere.
    #[tokio::test]
    async fn an_episode_through_the_gate_is_kept_sent_and_in_the_package() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogepisode");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let question = person_says("Where is my credit note?");
        let batch = turns_batch(&hive_id, &[(1, TEST_TS, &question)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        let people = strings(&["ilk:person", "ilk:juan"]);
        publish_episode(
            &app_state,
            &credit_note_episode(),
            &strings(&["refunds"]),
            people.clone(),
            TEST_THREAD,
            0,
        )
        .await;
        assert_eq!(
            app_state
                .runtime_state
                .lock()
                .await
                .episodes_gated_out_total,
            1
        );
        assert!(
            router
                .next_outgoing_within(Duration::from_millis(200))
                .await
                .is_none(),
            "a refused episode goes nowhere"
        );

        let leading = strings(&["credit note", "billing"]);
        publish_episode(
            &app_state,
            &credit_note_episode(),
            &leading,
            people,
            TEST_THREAD,
            0,
        )
        .await;
        let sent = next_outgoing(&mut router).await;
        assert!(matches!(sent.routing.dst, Destination::Broadcast));
        assert_eq!(
            sent.meta.msg.as_deref(),
            Some(episodes::MSG_COGNITION_EPISODE)
        );
        assert_eq!(sent.meta.target.as_deref(), Some("SY.cognition@*"));
        let episode: episodes::Episode = serde_json::from_value(sent.payload).expect("an episode");
        assert_eq!(episode.intensity, 8.0);
        assert_eq!(episode.people, ["ilk:person", "ilk:juan"]);
        assert_eq!(
            app_state.store.live_episodes(0).expect("read"),
            std::slice::from_ref(&episode)
        );

        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!(package.news.len(), 1);
        assert_eq!(package.news[0].episode_id, episode.episode_id);
        assert_eq!(package.news[0].state, "pending");
        let state = app_state.runtime_state.lock().await.clone();
        assert_eq!(
            (state.episodes_made_total, state.episodes_live_total),
            (1, 1)
        );
        unlink();
    }

    /// An episode another hive's cognition sent (docs/cognition-hands-v1.md §12.4) is taken once,
    /// and only from a cognition; after a restart it comes back from the file.
    #[tokio::test]
    async fn an_episode_is_taken_once_and_only_from_a_cognition() {
        let dir = TestDir::new();
        let episode = episodes::episode_of(
            "episode:from-worker2".to_string(),
            &credit_note_episode(),
            8.0,
            now_epoch_ms_i64(),
            strings(&["ilk:person", "ilk:juan"]),
            episodes::EpisodeOrigin {
                thread_id: "thread:elsewhere".to_string(),
                node: "SY.cognition@worker2".to_string(),
            },
        );
        let from = |name: &str| Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some(name.to_string()),
                dst: Destination::Broadcast,
                ttl: 15,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(episodes::MSG_COGNITION_EPISODE.to_string()),
                target: Some(episodes::EPISODE_BROADCAST_TARGET.to_string()),
                ..Meta::default()
            },
            payload: serde_json::to_value(&episode).expect("episode"),
        };
        {
            let (app_state, _router) = test_app_state("cogtake", None, &dir.0);
            take_episode(&app_state, &from("SY.cognition@worker2")).await;
            take_episode(&app_state, &from("SY.cognition@worker2")).await;
            take_episode(&app_state, &from("AI.mallory@worker1")).await;
            let state = app_state.runtime_state.lock().await.clone();
            assert_eq!(
                (
                    state.episodes_received_total,
                    state.episodes_refused_total,
                    state.episodes_live_total
                ),
                (1, 1, 1)
            );
        }
        let (again, _router) = test_app_state("cogtake", None, &dir.0);
        load_live_episodes(&again).await;
        assert_eq!(again.runtime_state.lock().await.episodes_live_total, 1);
    }

    // --- The clear (FINDINGS A-160) ---------------------------------------------------------------

    /// A `COGNITION_CLEAR` as its router delivers it: from `from`, as the router names the sender,
    /// and a tap's copy when `via_tap`.
    fn clear_from(from: Option<&str>, via_tap: bool) -> Message {
        Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: from.map(str::to_string),
                dst: Destination::Unicast("SY.cognition@worker1".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_COGNITION_CLEAR.to_string()),
                via_tap,
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    /// A test cognition that holds one communication, `TEST_THREAD`'s, with the person's turn the
    /// intake took (its seq 1), and one episode about the person: in memory, in its file and in the
    /// region. The worker's queue holds the turn.
    async fn cognition_with_a_conversation(
        hive_id: &str,
        region: MemoryRegionWriter,
        dir: &Path,
    ) -> (
        Arc<CognitionAppState>,
        fluxbee_sdk::RouterDispatcherTestHarness,
        mpsc::Sender<CognitionTurn>,
        mpsc::Receiver<CognitionTurn>,
    ) {
        let (app_state, mut router) = test_app_state(hive_id, Some(region), dir);
        let (turn_tx, turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let question = person_says("Where is my credit note?");
        let batch = turns_batch(hive_id, &[(1, TEST_TS, &question)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        assert_eq!(
            next_outgoing(&mut router).await.meta.msg.as_deref(),
            Some(MSG_STREAM_ACK)
        );
        publish_episode(
            &app_state,
            &credit_note_episode(),
            &strings(&["credit note", "billing"]),
            strings(&["ilk:person", "ilk:juan"]),
            TEST_THREAD,
            0,
        )
        .await;
        assert_eq!(
            next_outgoing(&mut router).await.meta.msg.as_deref(),
            Some(episodes::MSG_COGNITION_EPISODE)
        );
        assert_eq!(app_state.store.stats().expect("stats").0, 1);
        assert_eq!(app_state.store.live_episodes(0).expect("episodes").len(), 1);
        let package = region_package(&app_state)
            .await
            .expect("the agent's package");
        assert_eq!((package.turns.len(), package.news.len()), (1, 1));
        (app_state, router, turn_tx, turn_rx)
    }

    /// The bytes of its file and of its WAL, where a write lands before a checkpoint.
    fn file_bytes(app_state: &CognitionAppState) -> Vec<u8> {
        let path = app_state.store.path().to_path_buf();
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        [path, PathBuf::from(wal)]
            .iter()
            .filter_map(|path| fs::read(path).ok())
            .flatten()
            .collect()
    }

    fn holds(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    }

    /// A clear from SY.admin on the motherbee drops every communication and every episode: from
    /// memory, from the file, which keeps none of their text, and from the region, written empty.
    /// It answers what it removed, and the cognition keeps running, empty: the turn it had queued is
    /// not analyzed, and the next one starts its communication again.
    #[tokio::test]
    async fn a_clear_from_the_motherbee_admin_drops_everything_and_keeps_running() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogclear");
        let (app_state, mut router, turn_tx, mut turn_rx) =
            cognition_with_a_conversation(&hive_id, region, &dir.0).await;
        let before = file_bytes(&app_state);
        assert!(holds(&before, "Where is my credit note?"));
        assert!(holds(&before, "Argument over the credit note"));

        take_clear(&app_state, &clear_from(Some("SY.admin@motherbee"), false)).await;
        let answer = next_outgoing(&mut router).await;
        assert_eq!(
            answer.meta.msg.as_deref(),
            Some(MSG_COGNITION_CLEAR_RESPONSE)
        );
        assert_eq!(
            answer.payload,
            json!({
                "ok": true,
                "node_name": format!("SY.cognition@{hive_id}"),
                "removed": {
                    "file": { "communications": 1, "episodes": 1 },
                    "memory": { "communications": 1, "episodes": 1 },
                },
                "region": "written_empty",
            })
        );

        assert!(app_state.thread_states.lock().await.is_empty());
        assert_eq!(app_state.episodes.lock().expect("book").len(), 0);
        assert_eq!(app_state.store.stats().expect("stats").0, 0);
        assert!(app_state
            .store
            .live_episodes(0)
            .expect("episodes")
            .is_empty());
        let after = file_bytes(&app_state);
        assert!(!holds(&after, "Where is my credit note?"));
        assert!(!holds(&after, "Argument over the credit note"));
        let snapshot = app_state
            .memory_region
            .lock()
            .await
            .as_ref()
            .expect("the region")
            .read_snapshot()
            .expect("a snapshot");
        assert!(snapshot.threads.is_empty());
        let state = app_state.runtime_state.lock().await.clone();
        assert_eq!(
            (
                state.active_threads_total,
                state.episodes_live_total,
                state.shm_hot_threads_total,
                state.clears_total
            ),
            (0, 0, 0, 1)
        );
        assert!(state.last_thread_id.is_none() && state.last_src_ilk.is_none());
        assert!(state.last_clear_at.is_some());
        let status = build_status_payload(
            &app_state.node_name,
            &test_control_state(),
            &state,
            &app_state.runtime_paths,
            app_state.store.stats().ok(),
        );
        assert_eq!(status["clears"]["total"], json!(1));
        assert_eq!(status["store"]["communications_total"], json!(0));

        // The turn it had queued was taken before the clear.
        let queued = turn_rx.try_recv().expect("the queued turn");
        assert_eq!(queued.clears, 0);
        let mut loads = StoreLoads::default();
        assert!(hold_for_worker(
            &app_state,
            &key(TEST_THREAD),
            TEST_AGENT_NODE,
            queued.clears,
            &mut loads
        )
        .await
        .expect("no file read")
        .is_none());
        assert!(app_state.thread_states.lock().await.is_empty());

        // The next turn starts its communication again.
        let again = person_says("Hello again");
        let batch = turns_batch(&hive_id, &[(2, TEST_TS + 1_000, &again)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let package = region_package(&app_state).await.expect("the new package");
        assert_eq!(package.turns.len(), 1);
        assert!(package.news.is_empty());
        assert_eq!(app_state.store.stats().expect("stats").0, 1);
        let next = turn_rx.try_recv().expect("the next turn");
        assert_eq!(next.clears, 1);
        unlink();
    }

    /// A clear from anyone but SY.admin on the motherbee, as the router names the sender, is
    /// refused and touches nothing; so is a tap's copy of the admin's own.
    #[tokio::test]
    async fn a_clear_from_anyone_else_is_refused_and_touches_nothing() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogclearno");
        let (app_state, mut router, _turn_tx, _turn_rx) =
            cognition_with_a_conversation(&hive_id, region, &dir.0).await;
        for (from, via_tap) in [
            (Some("SY.admin@worker1"), false),
            (Some("SY.cognition@worker2"), false),
            (Some("SY.orchestrator@motherbee"), false),
            (Some("AI.sales@motherbee"), false),
            (Some(""), false),
            (None, false),
            (Some("SY.admin@motherbee"), true),
        ] {
            take_clear(&app_state, &clear_from(from, via_tap)).await;
            let answer = next_outgoing(&mut router).await;
            assert_eq!(
                answer.meta.msg.as_deref(),
                Some(MSG_COGNITION_CLEAR_RESPONSE)
            );
            assert_eq!(answer.payload["ok"], json!(false), "{from:?}");
            assert_eq!(answer.payload["error"], json!("UNAUTHORIZED"), "{from:?}");
        }
        assert_eq!(app_state.clears.load(Ordering::SeqCst), 0);
        assert_eq!(app_state.store.stats().expect("stats").0, 1);
        assert_eq!(app_state.store.live_episodes(0).expect("episodes").len(), 1);
        assert_eq!(app_state.thread_states.lock().await.len(), 1);
        assert!(region_package(&app_state).await.is_some());
        assert_eq!(app_state.runtime_state.lock().await.clears_total, 0);
        unlink();
    }

    /// The worker's turn a clear came in the middle of brings nothing back: not its communication,
    /// which it had copied before, nor its episode. A turn of after the clear writes back.
    #[tokio::test]
    async fn a_turn_the_worker_was_on_when_a_clear_came_brings_nothing_back() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogclrmid");
        let (app_state, mut router, _turn_tx, _turn_rx) =
            cognition_with_a_conversation(&hive_id, region, &dir.0).await;
        let mut loads = StoreLoads::default();
        assert_eq!(
            hold_for_worker(
                &app_state,
                &key(TEST_THREAD),
                TEST_AGENT_NODE,
                0,
                &mut loads
            )
            .await
            .expect("held"),
            Some(key(TEST_THREAD))
        );
        let copy = app_state
            .thread_states
            .lock()
            .await
            .get(&key(TEST_THREAD))
            .cloned()
            .expect("the worker's copy");

        take_clear(&app_state, &clear_from(Some("SY.admin@motherbee"), false)).await;
        next_outgoing(&mut router).await;

        let (written_back, synced) =
            write_back_worker_turn(&app_state, &key(TEST_THREAD), copy.clone(), 0, "").await;
        assert_eq!(written_back, WriteBack::AfterClear);
        synced.expect("the region written");
        assert!(app_state.thread_states.lock().await.is_empty());
        assert_eq!(app_state.store.stats().expect("stats").0, 0);
        assert!(region_package(&app_state).await.is_none());

        publish_episode(
            &app_state,
            &credit_note_episode(),
            &strings(&["credit note", "billing"]),
            strings(&["ilk:person", "ilk:juan"]),
            TEST_THREAD,
            0,
        )
        .await;
        assert!(
            router
                .next_outgoing_within(Duration::from_millis(200))
                .await
                .is_none(),
            "an episode of before the clear goes nowhere"
        );
        assert!(app_state
            .store
            .live_episodes(0)
            .expect("episodes")
            .is_empty());
        assert_eq!(app_state.episodes.lock().expect("book").len(), 0);

        let (written_back, _) =
            write_back_worker_turn(&app_state, &key(TEST_THREAD), copy, 1, "").await;
        assert_eq!(written_back, WriteBack::Written);
        assert_eq!(app_state.store.stats().expect("stats").0, 1);
        unlink();
    }

    /// The authority is the admin that commands the edges, by the name its router stamps.
    #[test]
    fn a_clear_is_the_motherbee_admins_alone() {
        assert_eq!(COGNITION_CLEAR_AUTHORITY, "SY.admin@motherbee");
        assert!(authorize_clear(&clear_from(Some("SY.admin@motherbee"), false)).is_ok());
        assert!(authorize_clear(&clear_from(Some(" SY.admin@motherbee "), false)).is_ok());
        for refused in [
            clear_from(Some("SY.admin@worker1"), false),
            clear_from(Some("SY.architect@motherbee"), false),
            clear_from(None, false),
            clear_from(Some("SY.admin@motherbee"), true),
        ] {
            assert!(authorize_clear(&refused).is_err());
        }
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
            recent_turn_of(&message, TEST_AGENT_NODE, TEST_AGENT, 0, TEST_TS, known)
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
            0,
            TEST_TS,
            known,
        )
        .expect("a turn")
        .turn;
        assert_eq!(unnamed.writer, party(Some("ilk:person"), None, None));

        let mut answer = agent_says("Your credit note is on its way");
        answer.meta.dst_ilk = Some("ilk:ana".to_string());
        let answer = recent_turn_of(&answer, TEST_AGENT_NODE, TEST_AGENT, 0, TEST_TS, known)
            .expect("a turn")
            .turn;
        assert_eq!(answer.writer, None);
        assert_eq!(
            answer.to,
            party(Some("ilk:ana"), Some("Ana Pérez"), Some("person"))
        );
    }

    /// A relay names where it went, the IO node, and the person when the message names them: it was
    /// said to them, not to the agent. A turn the agent got names neither, and the agent's answer to
    /// an IO node only whom it answered. The tagger reads whom a relay was written to, and an
    /// episode of a relay is between its writer and the person it names, before any other person
    /// of the thread (docs/cognition-hands-v1.md §12.9, FINDINGS A-161).
    #[test]
    fn a_relay_names_where_it_went_and_whom_when_the_message_does() {
        let known = |ilk: &str| {
            (ilk == "ilk:ana").then(|| IlkFacts {
                ilk_type: "human".to_string(),
                name: Some("Ana Pérez".to_string()),
            })
        };
        let turn_of = |message: &Message| {
            recent_turn_of(message, TEST_AGENT_NODE, TEST_AGENT, 0, TEST_TS, known)
                .expect("a turn")
                .turn
        };
        let addressee =
            |message: &Message| relayed_of(message, known).as_ref().map(relay_addressee);
        let mut to_ana = relayed("ilk:juan", "Te pido disculpas");
        to_ana.routing.dst = Destination::Unicast("IO.web@worker1".to_string());
        to_ana.meta.dst_ilk = Some("ilk:ana".to_string());
        let turn = turn_of(&to_ana);
        assert_eq!(turn.direction, MemoryTurnDirection::In);
        assert_eq!(turn.to_via.as_deref(), Some("IO.web@worker1"));
        assert_eq!(
            turn.to,
            Some(MemoryTurnParty {
                ilk: Some("ilk:ana".to_string()),
                name: Some("Ana Pérez".to_string()),
                kind: Some("person".to_string()),
            })
        );
        assert_eq!(
            relayed_of(&to_ana, known),
            Some(Relayed {
                to: turn.to.clone(),
                to_via: "IO.web@worker1".to_string()
            }),
            "as its turn keeps it"
        );
        assert_eq!(
            addressee(&to_ana).as_deref(),
            Some("person Ana Pérez (ilk:ana) via IO.web@worker1")
        );

        let to_team = relayed("ilk:ana", "You again?");
        let turn = turn_of(&to_team);
        assert_eq!(turn.to_via.as_deref(), Some("IO.slack.team@motherbee"));
        assert_eq!(turn.to, None);
        assert_eq!(
            addressee(&to_team).as_deref(),
            Some("IO.slack.team@motherbee")
        );

        let got = turn_of(&person_says("hello"));
        assert_eq!((got.to, got.to_via), (None, None));
        assert_eq!(addressee(&person_says("hello")), None);
        let mut answer = agent_says("I'm passing it to invoicing now");
        answer.routing.dst = Destination::Unicast("IO.web@worker1".to_string());
        answer.meta.dst_ilk = Some("ilk:ana".to_string());
        let answer = turn_of(&answer);
        assert_eq!(answer.to_via, None);
        assert_eq!(answer.to.and_then(|to| to.ilk).as_deref(), Some("ilk:ana"));

        let pedro = HeldTurn::from(MemoryTurn {
            direction: MemoryTurnDirection::In,
            src_ilk: Some("ilk:pedro".to_string()),
            via: Some("IO.slack.team@motherbee".to_string()),
            writer: Some(MemoryTurnParty {
                ilk: Some("ilk:pedro".to_string()),
                name: None,
                kind: None,
            }),
            to: None,
            to_via: None,
            text: "I'll see to it".to_string(),
            attachments: 0,
            choice: None,
            ts: turn_ts(TEST_TS),
        });
        let (counterpart, people) = episode_people(
            &to_ana,
            TEST_AGENT_NODE,
            TEST_AGENT,
            relayed_of(&to_ana, known).as_ref(),
            std::slice::from_ref(&pedro),
            known,
        );
        assert_eq!(
            counterpart.and_then(|party| party.ilk).as_deref(),
            Some("ilk:ana")
        );
        assert_eq!(people, ["ilk:juan", "ilk:ana"]);
        let (counterpart, _) = episode_people(
            &to_team,
            TEST_AGENT_NODE,
            TEST_AGENT,
            relayed_of(&to_team, known).as_ref(),
            std::slice::from_ref(&pedro),
            known,
        );
        assert_eq!(
            counterpart.and_then(|party| party.ilk).as_deref(),
            Some("ilk:pedro")
        );
        let mut to_web = relayed("ilk:juan", "I'll look at it today");
        to_web.routing.dst = Destination::Unicast("IO.web@worker1".to_string());
        let (counterpart, people) = episode_people(
            &to_web,
            TEST_AGENT_NODE,
            TEST_AGENT,
            relayed_of(&to_web, known).as_ref(),
            std::slice::from_ref(&pedro),
            known,
        );
        assert_eq!(
            counterpart,
            Some(MemoryTurnParty {
                ilk: None,
                name: Some("IO.web@worker1".to_string()),
                kind: None,
            })
        );
        assert_eq!(people, ["ilk:juan"]);
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
        let who = |held: &HeldTurn| {
            (
                held.turn.direction,
                held.turn.src_ilk.clone(),
                held.turn.via.clone(),
                held.turn.ts.clone(),
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
        assert!(
            recent_turn_of(&no_text, TEST_AGENT_NODE, TEST_AGENT, 0, TEST_TS, no_facts).is_none()
        );
    }

    /// Text of more bytes a character keeps fewer turns: they stay under the bytes a package may
    /// take, and the newest always stays. Twenty turns of 2,000 characters of Latin
    /// text all fit.
    #[test]
    fn the_last_turns_stay_under_their_byte_bound() {
        let size = |turns: &[HeldTurn]| {
            turns
                .iter()
                .map(|held| serde_json::to_vec(&held.turn).expect("turn").len() + 1)
                .sum::<usize>()
        };
        let mut wide = Vec::new();
        for n in 0..20u64 {
            let said = person_says(&format!("{n} {}", "字".repeat(1_990)));
            push_recent_turn(&mut wide, taken(&said, TEST_TS + n * 1_000).turn.into());
        }
        assert!(wide.len() < COGNITION_RECENT_TURNS);
        assert!(size(&wide) <= COGNITION_RECENT_TURNS_MAX_BYTES);
        assert!(wide.last().expect("a turn").turn.text.starts_with("19 "));

        let mut latin = Vec::new();
        for n in 0..20u64 {
            let said = person_says(&"x".repeat(COGNITION_TURN_TEXT_MAX_CHARS));
            push_recent_turn(&mut latin, taken(&said, TEST_TS + n * 1_000).turn.into());
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
                recent_turn_of(
                    &to_sales,
                    TEST_AGENT_NODE,
                    "ilk:sales",
                    0,
                    TEST_TS,
                    no_facts,
                )
                .expect("a turn"),
                recent_turn_of(
                    &to_invoice,
                    "AI.invoice@motherbee",
                    "ilk:invoice",
                    0,
                    TEST_TS,
                    no_facts,
                )
                .expect("a turn"),
            ],
        );
        let hot_set = build_memory_hot_set_snapshot(&threads, &[], &no_news).expect("hot set");
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
        store
            .save([(&key(TEST_THREAD), &held)], &[])
            .expect("written");

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
        write_back_turn(
            &mut restarted,
            &key(TEST_THREAD),
            updated,
            &Refusals::default(),
        );
        let written = &restarted[&key(TEST_THREAD)];
        assert_eq!(written.turn_count, loaded.turn_count + 1);
        assert_eq!(written.turns.len(), 5);
        assert_eq!(
            written.turns.last().map(|held| held.turn.text.as_str()),
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
        take_recent_turns(&app_state, vec![taken(&newest, TEST_TS).into()]).await;
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
        updated.turns.push(first.turn.into());
        hold(
            &mut held,
            &store,
            vec![taken(&person_says("later"), TEST_TS + 1)],
        );
        write_back_turn(&mut held, &key(TEST_THREAD), updated, &Refusals::default());
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
            0,
            TEST_TS,
            no_facts
        )
        .is_none());
    }

    // --- A turn its agent refused without running it (FINDINGS A-292) ---

    /// What the agent's AI runtime answers a turn it refuses without running it, since 0.1.136
    /// (`busy_reply` and `send_reply`, crates/fluxbee_ai_sdk/src/runtime.rs): the reply to the
    /// request with the runtime's error, and the agent's entry in the trace, `busy` (`reply_to`).
    fn busy_answer_to(request: &Message) -> Message {
        reply_to(
            request,
            json!({
                "type": "error",
                "code": "ai_runtime_busy",
                "message": "The AI runtime is busy: its 4 model workers are all taken and 128 \
                            more messages wait for them. This message was not run; send it again \
                            later.",
                "retryable": true
            }),
            &["busy"],
        )
    }

    /// What the agent's AI runtime answers a turn it ran: its text, and the agent's entry in the
    /// trace with no act (`reply_to`).
    fn answer_to(request: &Message, content: &str) -> Message {
        reply_to(request, text(content), &[])
    }

    /// The agent's AI runtime's reply to `request` (`send_reply`,
    /// crates/fluxbee_ai_sdk/src/runtime.rs): `payload`, stamped as the agent's, and the agent's
    /// entry in the trace with `act`, as its SDK writes it; with the agent's name, as its router
    /// copies it.
    fn reply_to(request: &Message, payload: Value, act: &[&str]) -> Message {
        let mut answer = fluxbee_ai_sdk::build_reply_message_runtime_src(request, payload);
        fluxbee_ai_sdk::set_reply_act(&mut answer, act.iter().copied());
        answer.routing.src = Uuid::new_v4().to_string();
        answer.meta.src_ilk = Some(TEST_AGENT.to_string());
        message_writer::set_writer(
            &mut answer.meta,
            message_writer::Writer::new(Some(TEST_AGENT), Some(TEST_AGENT_NODE)),
        );
        let act = fluxbee_ai_sdk::take_reply_act(&mut answer);
        message_trace::write_entry(
            &mut answer,
            TEST_AGENT_NODE,
            message_trace::TraceNote {
                to: request.routing.src_l2_name.clone(),
                act,
            },
        );
        answer.routing.src_l2_name = Some(TEST_AGENT_NODE.to_string());
        answer
    }

    /// The trace of `message`, to change in a test.
    fn trace_of(message: &mut Message) -> &mut Vec<Value> {
        message
            .meta
            .context
            .as_mut()
            .and_then(|context| context.get_mut("trace"))
            .and_then(Value::as_array_mut)
            .expect("a trace")
    }

    /// The bug (FINDINGS A-292): the router copies a person's message for cognition once the
    /// agent's link takes it, before the agent refuses it; the refusal had no text, so the message
    /// stayed in the communication as said to the agent and unanswered, and was there twice once
    /// the person sent it again. The refusal now drops it, from the package and the file, and the
    /// message sent again is one turn, answered.
    #[tokio::test]
    async fn a_refused_turn_leaves_its_communication_and_sent_again_is_one_turn() {
        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogrefuse");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);

        let hello = person_says("Hello");
        let greeting = agent_says("Hello, how can I help?");
        let question = person_says("Is my invoice ready?");
        let batch = turns_batch(
            &hive_id,
            &[
                (1, TEST_TS, &hello),
                (2, TEST_TS + 1_000, &greeting),
                (3, TEST_TS + 2_000, &question),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let package = region_package(&app_state).await.expect("the package");
        assert_eq!(
            texts(&package.turns),
            ["Hello", "Hello, how can I help?", "Is my invoice ready?"],
            "the router copies the message before the agent can refuse it"
        );

        let batch = turns_batch(
            &hive_id,
            &[(4, TEST_TS + 2_040, &busy_answer_to(&question))],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        let ack = next_outgoing(&mut router).await;
        assert_eq!(ack.payload["up_to_seq"], json!(4));
        let package = region_package(&app_state).await.expect("the package");
        assert_eq!(texts(&package.turns), ["Hello", "Hello, how can I help?"]);
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("in the file");
        assert_eq!(texts(&stored.turns), ["Hello", "Hello, how can I help?"]);
        let queued: Vec<u64> = std::iter::from_fn(|| turn_rx.try_recv().ok())
            .map(|turn| turn.seq)
            .collect();
        assert_eq!(queued, [1, 2, 3], "the refusal is no turn of the worker");

        let again = person_says("Is my invoice ready?");
        let answer = agent_says("Yes: it went out this morning.");
        let batch = turns_batch(
            &hive_id,
            &[
                (5, TEST_TS + 60_000, &again),
                (6, TEST_TS + 63_000, &answer),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let package = region_package(&app_state).await.expect("the package");
        assert_eq!(
            texts(&package.turns),
            [
                "Hello",
                "Hello, how can I help?",
                "Is my invoice ready?",
                "Yes: it went out this morning."
            ]
        );
        assert_eq!(package.turns[2].ts, turn_ts(TEST_TS + 60_000));

        let state = app_state.runtime_state.lock().await;
        let status = build_status_payload(
            &app_state.node_name,
            &test_control_state(),
            &state,
            &test_runtime_paths(),
            None,
        );
        assert_eq!(status["turns_consumer"]["refusals_total"], json!(1));
        assert_eq!(
            status["turns_consumer"]["refused_turns_dropped_total"],
            json!(1)
        );
        assert_eq!(state.intake_turns_total, 5, "the refusal is no turn");
        drop(state);
        unlink();
    }

    /// A refusal in the batch of the turn it refused drops it before the worker takes the turn; the
    /// worker does not analyze it, and never gets the refusal: neither calls a model.
    #[tokio::test]
    async fn the_worker_does_not_analyze_a_turn_its_agent_refused() {
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cogrefwork", None, &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let question = person_says("Is my invoice ready?");
        let batch = turns_batch(
            "cogrefwork",
            &[
                (1, TEST_TS, &question),
                (2, TEST_TS + 40, &busy_answer_to(&question)),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let queued = turn_rx
            .try_recv()
            .expect("the refused turn waits for the worker");
        assert_eq!(queued.seq, 1);
        assert!(turn_rx.try_recv().is_err(), "the refusal is no turn");
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("in the file");
        assert!(stored.turns.is_empty() && stored.turn_keys.is_empty());

        handle_turn(queued, Arc::clone(&app_state)).await;
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.refused_turns_not_analyzed_total, 1);
        assert_eq!(state.semantic_tagger_calls_total, 0);
        assert_eq!(state.turns_not_analyzed_total, 0);
        assert_eq!(state.processed_turns_total, 0);
        drop(state);
        let stored = app_state
            .store
            .load(&key(TEST_THREAD))
            .expect("read")
            .expect("in the file");
        assert_eq!(stored.turn_count, 0);
    }

    /// A refusal is the AI runtime's answer `ai_runtime_busy` by its exact shape: the agent's, the
    /// runtime's error and nothing else, its entry in the trace the last with the one act `busy`.
    /// Anything looser is a turn, as before; and it names the message it refused.
    #[test]
    fn only_the_exact_busy_answer_is_a_refusal() {
        let question = person_says("Is my invoice ready?");
        let busy = busy_answer_to(&question);
        assert!(is_busy_answer(&busy, TEST_AGENT_NODE));
        assert_eq!(refused_request(&busy), Some(request_of(&question)));
        assert!(!is_busy_answer(&busy, "AI.other@motherbee"));

        let mut looser: Vec<(&str, Message)> = Vec::new();
        let mut changed = |why: &'static str, change: &dyn Fn(&mut Message)| {
            let mut message = busy.clone();
            change(&mut message);
            looser.push((why, message));
        };
        changed("not retryable", &|m| m.payload["retryable"] = json!(false));
        changed("retryable as text", &|m| {
            m.payload["retryable"] = json!("true")
        });
        changed("another error", &|m| {
            m.payload["code"] = json!("ai_runtime_error")
        });
        changed("another case", &|m| {
            m.payload["code"] = json!("AI_RUNTIME_BUSY")
        });
        changed("not an error", &|m| m.payload["type"] = json!("text"));
        changed("one more field", &|m| m.payload["reply"] = json!("Busy"));
        changed("a message that is no text", &|m| {
            m.payload["message"] = json!(7)
        });
        changed("no message", &|m| {
            m.payload
                .as_object_mut()
                .expect("payload")
                .remove("message");
        });
        changed("not the agent's", &|m| {
            m.routing.src_l2_name = Some("IO.api@motherbee".to_string());
        });
        changed("no act", &|m| {
            trace_of(m).last_mut().expect("entry")["act"] = json!([]);
        });
        changed("one more act", &|m| {
            trace_of(m).last_mut().expect("entry")["act"] = json!(["busy", "retry"]);
        });
        changed("another act", &|m| {
            trace_of(m).last_mut().expect("entry")["act"] = json!(["forward"]);
        });
        changed("another node's entry", &|m| {
            trace_of(m).last_mut().expect("entry")["from"] = json!("AI.other@motherbee");
        });
        changed("no entry of the agent", &|m| {
            trace_of(m).pop();
        });
        changed("forwarded on", &|m| {
            trace_of(m).push(json!({"t": "2026-10-30T12:00:01.000Z",
                "from": "WF.sales@motherbee", "to": "IO.api@motherbee", "act": ["forward"]}));
        });
        changed("no trace", &|m| m.meta.context = None);
        for (why, message) in &looser {
            assert!(!is_busy_answer(message, TEST_AGENT_NODE), "{why}");
        }

        // What a person writes in that shape is their turn.
        let mut look_alike = busy.clone();
        look_alike.routing.src_l2_name = Some("IO.api@motherbee".to_string());
        assert!(!is_busy_answer(&look_alike, "IO.api@motherbee"));
    }

    /// One cycle can bring its agent several messages under one `trace_id`: a WF writes to the
    /// agent again on its answer, twice in one step. The refusal drops the message it refused, told
    /// apart by its trace, even when another of the cycle reached the agent after it; never the one
    /// the agent answered, nor the one it took.
    #[tokio::test]
    async fn a_refusal_drops_only_the_message_it_refused_in_its_cycle() {
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cogrefcyc", None, &dir.0);
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let first = supervisor_says("Approve 20% for Ana.");
        let mut answer = agent_says("Approved: 20% on her next invoice.");
        answer.routing.trace_id = first.routing.trace_id.clone();
        let again_on_the_answer = |content: &str, at: &str| {
            let mut message = supervisor_says(content);
            message.routing.trace_id = first.routing.trace_id.clone();
            trace_of(&mut message).extend([
                json!({"t": "2026-10-30T12:05:03.000Z", "from": TEST_AGENT_NODE,
                       "to": "WF.sales@motherbee"}),
                json!({"t": at, "from": "WF.sales@motherbee", "to": TEST_AGENT_NODE,
                       "act": ["send_message"]}),
            ]);
            message
        };
        let refused = again_on_the_answer("Tell Ana now.", "2026-10-30T12:05:03.050Z");
        let taken_after = again_on_the_answer("And copy billing.", "2026-10-30T12:05:03.051Z");
        let batch = turns_batch(
            "cogrefcyc",
            &[
                (1, TEST_TS, &first),
                (2, TEST_TS + 3_000, &answer),
                (3, TEST_TS + 3_100, &refused),
                (4, TEST_TS + 3_101, &taken_after),
                (5, TEST_TS + 3_140, &busy_answer_to(&refused)),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let threads = app_state.thread_states.lock().await;
        assert_eq!(
            texts(&threads[&key(TEST_THREAD)].turns),
            [
                "Approve 20% for Ana.",
                "Approved: 20% on her next invoice.",
                "And copy billing."
            ]
        );
    }

    /// A message a WF relays to a person and forwards to the agent too, which the agent refuses,
    /// stays as the relay it also was: said to the person, not to the agent; whichever of its
    /// copies came first (docs/cognition-hands-v1.md §12.9).
    #[tokio::test]
    async fn a_refused_message_a_wf_relayed_too_stays_as_the_relay() {
        for relay_first in [true, false] {
            let dir = TestDir::new();
            let (hive_id, region, unlink) = test_region("cogrefrel");
            let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
            let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
            let greeting = agent_says("A person of the team takes your case.");
            let batch = turns_batch(&hive_id, &[(1, TEST_TS, &greeting)]);
            take_stream_batch(&app_state, &turn_tx, &batch).await;
            next_outgoing(&mut router).await;
            let mut to_agent = person_says("My invoice 123 was charged twice.");
            to_agent.routing.src_l2_name = Some(RELAY_WF.to_string());
            let mut to_team = to_agent.clone();
            to_team.routing.dst = Destination::Unicast("IO.slack.team@motherbee".to_string());
            let (relay_seq, copy_seq) = if relay_first { (2, 3) } else { (3, 2) };
            let relay = batch_of(
                &hive_id,
                RELAY_WF,
                &[(relay_seq, TEST_TS + 1_000, &to_team)],
            );
            let copy = turns_batch(&hive_id, &[(copy_seq, TEST_TS + 1_001, &to_agent)]);
            let batches = if relay_first {
                [relay, copy]
            } else {
                [copy, relay]
            };
            for batch in &batches {
                take_stream_batch(&app_state, &turn_tx, batch).await;
                next_outgoing(&mut router).await;
            }
            let package = region_package(&app_state).await.expect("the package");
            assert_eq!(package.turns.len(), 2, "one turn: {relay_first}");
            assert_eq!(package.turns[1].to_via, None, "said to the agent too");

            let batch = turns_batch(
                &hive_id,
                &[(4, TEST_TS + 1_040, &busy_answer_to(&to_agent))],
            );
            take_stream_batch(&app_state, &turn_tx, &batch).await;
            next_outgoing(&mut router).await;
            let package = region_package(&app_state).await.expect("the package");
            assert_eq!(
                texts(&package.turns),
                [
                    "A person of the team takes your case.",
                    "My invoice 123 was charged twice."
                ],
                "relay first: {relay_first}"
            );
            assert_eq!(
                package.turns[1].to_via.as_deref(),
                Some("IO.slack.team@motherbee"),
                "relay first: {relay_first}"
            );
            let stored = app_state
                .store
                .load(&key(TEST_THREAD))
                .expect("read")
                .expect("in the file");
            assert_eq!(
                stored.turns[1].turn.to_via.as_deref(),
                Some("IO.slack.team@motherbee")
            );
            let state = app_state.runtime_state.lock().await;
            assert_eq!(
                (state.refusals_total, state.refused_turns_dropped_total),
                (1, 0)
            );
            drop(state);
            unlink();
        }
    }

    /// A WF that sends the refused message again in its cycle sends it under the same key: the
    /// refusal took the key with the turn, so the message sent again is a turn, and the worker's.
    #[tokio::test]
    async fn a_message_sent_again_under_its_key_is_a_turn_again() {
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cogrefkey", None, &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let first = supervisor_says("Approve 20% for Ana.");
        let mut again = supervisor_says("Approve 20% for Ana.");
        again.routing.trace_id = first.routing.trace_id.clone();
        trace_of(&mut again).push(json!({"t": "2026-10-30T12:05:05.000Z",
            "from": "WF.sales@motherbee", "to": TEST_AGENT_NODE, "act": ["send_message"]}));
        assert_eq!(turn_key(&first), turn_key(&again));
        let batch = turns_batch(
            "cogrefkey",
            &[
                (1, TEST_TS, &first),
                (2, TEST_TS + 40, &busy_answer_to(&first)),
                (3, TEST_TS + 5_000, &again),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let threads = app_state.thread_states.lock().await;
        let held = &threads[&key(TEST_THREAD)];
        assert_eq!(texts(&held.turns), ["Approve 20% for Ana."]);
        assert_eq!(held.turns[0].turn.ts, turn_ts(TEST_TS + 5_000));
        assert_eq!(held.turns[0].requests, [request_of(&again)]);
        assert_eq!(held.turn_keys, [turn_key(&again)]);
        drop(threads);
        let queued: Vec<(u64, bool)> = std::iter::from_fn(|| turn_rx.try_recv().ok())
            .map(|turn| (turn.seq, turn.duplicate))
            .collect();
        assert_eq!(queued, [(1, false), (3, false)]);
        let refusals = app_state.refusals.lock().expect("refusals");
        assert!(refusals.holds(&key(TEST_THREAD), &request_of(&first)));
        assert!(!refusals.holds(&key(TEST_THREAD), &request_of(&again)));
    }

    /// The worker's copy, taken before a refusal, does not bring the refused turn back when it is
    /// written back; and the worker's own turn, refused while the models answered, derives nothing.
    #[tokio::test]
    async fn the_workers_copy_does_not_bring_a_refused_turn_back() {
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cogrefcopy", None, &dir.0);
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let hello = person_says("Hello");
        let question = person_says("Is my invoice ready?");
        let batch = turns_batch(
            "cogrefcopy",
            &[(1, TEST_TS, &hello), (2, TEST_TS + 1_000, &question)],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        // The worker is on the first turn, and copies the communication: both turns are in it.
        let mut copy = app_state.thread_states.lock().await[&key(TEST_THREAD)].clone();
        copy.turn_count += 1;
        assert_eq!(copy.turns.len(), 2);
        let batch = turns_batch(
            "cogrefcopy",
            &[(3, TEST_TS + 1_040, &busy_answer_to(&question))],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        let (written_back, _) = write_back_worker_turn(
            &app_state,
            &key(TEST_THREAD),
            copy.clone(),
            0,
            &request_of(&hello),
        )
        .await;
        assert_eq!(written_back, WriteBack::Written);
        let held = app_state.thread_states.lock().await[&key(TEST_THREAD)].clone();
        assert_eq!(texts(&held.turns), ["Hello"]);
        assert_eq!(held.turn_count, 1);

        let (written_back, _) = write_back_worker_turn(
            &app_state,
            &key(TEST_THREAD),
            copy,
            0,
            &request_of(&question),
        )
        .await;
        assert_eq!(written_back, WriteBack::Refused);
        let held = app_state.thread_states.lock().await[&key(TEST_THREAD)].clone();
        assert_eq!(held.turn_count, 1, "nothing derived from the refused turn");
        assert_eq!(texts(&held.turns), ["Hello"]);
    }

    /// A refusal finds its turn in the file when its communication is not held, and starts none
    /// when neither has it.
    #[test]
    fn a_refusal_finds_its_turn_in_the_file_and_starts_no_communication() {
        let store = test_store();
        let question = person_says("Is my invoice ready?");
        let mut stored = ThreadCognitionState::default();
        let turn = taken(&question, TEST_TS);
        stored.turn_keys.push(turn.turn_key.clone());
        stored.turns.push(HeldTurn {
            turn: turn.turn,
            key: turn.turn_key,
            requests: vec![turn.request],
            refused: Vec::new(),
            relayed: None,
        });
        store
            .save([(&key(TEST_THREAD), &stored)], &[])
            .expect("written");
        let refusal = |thread: &str| Refusal {
            key: key(thread),
            request: request_of(&question),
            trace_id: question.routing.trace_id.clone(),
        };

        let mut threads = HashMap::new();
        let mut loads = StoreLoads::default();
        let mut tally = IntakeTally::default();
        let mut refusals = Refusals::default();
        let touched = hold_recent_turns(
            &mut threads,
            &store,
            &mut Takes::default(),
            &mut refusals,
            vec![
                Intake::Refusal(refusal(TEST_THREAD)),
                Intake::Refusal(refusal("thread:none")),
            ],
            &mut loads,
            &mut tally,
        );
        assert_eq!(touched, [key(TEST_THREAD)]);
        assert_eq!((loads.loaded, tally.refused_dropped), (1, 1));
        assert!(threads[&key(TEST_THREAD)].turns.is_empty());
        assert!(threads[&key(TEST_THREAD)].turn_keys.is_empty());
        assert!(!threads.contains_key(&key("thread:none")), "none started");
        assert!(refusals.holds(&key("thread:none"), &request_of(&question)));
    }

    /// A communication's row keeps what only cognition reads of each turn beside it, and a row
    /// written before 0.1.137 reads as turns without it; the package carries the turns alone, as
    /// they were.
    #[test]
    fn a_held_turn_keeps_its_own_fields_out_of_the_package() {
        let turn = taken(&person_says("Is my invoice ready?"), TEST_TS);
        let held = HeldTurn {
            turn: turn.turn.clone(),
            key: turn.turn_key.clone(),
            requests: vec![turn.request.clone()],
            refused: vec!["an earlier copy".to_string()],
            relayed: Some(Relayed {
                to: None,
                to_via: "IO.slack.team@motherbee".to_string(),
            }),
        };
        let row = serde_json::to_value(&held).expect("a row");
        assert_eq!(row["text"], json!("Is my invoice ready?"));
        assert_eq!(row["requests"], json!([turn.request]));
        assert_eq!(row["refused"], json!(["an earlier copy"]));
        assert_eq!(
            serde_json::from_value::<HeldTurn>(row.clone()).expect("read"),
            held
        );
        assert_eq!(
            serde_json::from_value::<MemoryTurn>(row).expect("a reader of 0.1.136"),
            turn.turn
        );
        let before = serde_json::to_value(&turn.turn).expect("a row of 0.1.136");
        assert_eq!(
            serde_json::from_value::<HeldTurn>(before).expect("read"),
            HeldTurn::from(turn.turn.clone())
        );

        let state = ThreadCognitionState {
            turns: vec![held],
            ..ThreadCognitionState::default()
        };
        let package = build_memory_package_for_thread(TEST_THREAD, &state, (Vec::new(), 0));
        assert_eq!(package.turns, [turn.turn]);
        let carried = serde_json::to_value(&package.turns[0]).expect("the package's turn");
        for field in ["key", "requests", "refused", "relayed"] {
            assert!(carried.get(field).is_none(), "{field}");
        }
    }

    // --- The review of A-292 ---

    /// The refusal of a turn that waits at the end of a full queue outlasts the refusals its drain
    /// brings, those of turns the full queue dropped among them: 256 turns at about 5 s each, at
    /// three refusals a second. Past `COGNITION_REFUSALS_KEPT` the oldest goes first.
    #[test]
    fn a_refusal_outlasts_a_full_queues_drain() {
        let mut refusals = Refusals::default();
        let waiting = key(TEST_THREAD);
        refusals.record(&waiting, "the waiting turn".to_string(), None);
        let drain = COGNITION_TURN_QUEUE * 5 * 3;
        let other = |n: usize| key(&format!("thread:{n}"));
        for n in 0..drain {
            refusals.record(&other(n), format!("turn {n}"), None);
        }
        assert!(refusals.dropped(&waiting, "the waiting turn"));
        for n in drain..COGNITION_REFUSALS_KEPT {
            refusals.record(&other(n), format!("turn {n}"), None);
        }
        assert!(
            !refusals.holds(&waiting, "the waiting turn"),
            "the oldest goes first"
        );
        assert_eq!(refusals.recent.len(), COGNITION_REFUSALS_KEPT);
    }

    /// A message a WF forwards to the agent and relays to a person too, which the agent refuses, is
    /// analyzed once, as the relay it is again (docs/cognition-hands-v1.md §12.9): the relay, when
    /// it came first or the refusal came before it; the agent's copy, as that relay, when the copy
    /// came first and the relay was its duplicate.
    #[tokio::test]
    async fn a_refused_message_a_wf_relayed_too_is_analyzed_once_as_the_relay() {
        for order in ["relay first", "copy first", "one batch"] {
            let dir = TestDir::new();
            let (app_state, mut router) = test_app_state("cogrefan", None, &dir.0);
            // The analysis stops at its first model call, which counts it.
            app_state.control_state.lock().await.ai_settings_error =
                Some("no model in the tests".to_string());
            let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
            let greeting = agent_says("A person of the team takes your case.");
            let batch = turns_batch("cogrefan", &[(1, TEST_TS, &greeting)]);
            take_stream_batch(&app_state, &turn_tx, &batch).await;
            next_outgoing(&mut router).await;
            turn_rx.try_recv().expect("the greeting");

            let mut to_agent = person_says("My invoice 123 was charged twice.");
            to_agent.routing.src_l2_name = Some(RELAY_WF.to_string());
            let mut to_team = to_agent.clone();
            to_team.routing.dst = Destination::Unicast("IO.slack.team@motherbee".to_string());
            let busy = busy_answer_to(&to_agent);
            let batches = match order {
                "relay first" => vec![
                    batch_of("cogrefan", RELAY_WF, &[(2, TEST_TS + 1_000, &to_team)]),
                    turns_batch("cogrefan", &[(3, TEST_TS + 1_001, &to_agent)]),
                    turns_batch("cogrefan", &[(4, TEST_TS + 1_040, &busy)]),
                ],
                "copy first" => vec![
                    turns_batch("cogrefan", &[(2, TEST_TS + 1_000, &to_agent)]),
                    batch_of("cogrefan", RELAY_WF, &[(3, TEST_TS + 1_001, &to_team)]),
                    turns_batch("cogrefan", &[(4, TEST_TS + 1_040, &busy)]),
                ],
                _ => {
                    let mut batch = turns_batch(
                        "cogrefan",
                        &[
                            (2, TEST_TS + 1_000, &to_agent),
                            (3, TEST_TS + 1_001, &to_team),
                            (4, TEST_TS + 1_040, &busy),
                        ],
                    );
                    let mut payload: StreamBatch =
                        serde_json::from_value(batch.payload.clone()).expect("batch");
                    payload.records[1].producer = RELAY_WF.to_string();
                    batch.payload = serde_json::to_value(payload).expect("batch");
                    vec![batch]
                }
            };
            for batch in &batches {
                take_stream_batch(&app_state, &turn_tx, batch).await;
                next_outgoing(&mut router).await;
            }
            if order == "copy first" {
                let refusals = app_state.refusals.lock().expect("refusals");
                let refused = refusals
                    .of(&key(TEST_THREAD), &request_of(&to_agent))
                    .expect("the refusal");
                assert_eq!(
                    refused
                        .relayed
                        .as_ref()
                        .map(|relayed| relayed.to_via.as_str()),
                    Some("IO.slack.team@motherbee")
                );
            }
            let held = app_state.thread_states.lock().await[&key(TEST_THREAD)].clone();
            assert_eq!(
                held.turns[1].turn.to_via.as_deref(),
                Some("IO.slack.team@motherbee"),
                "{order}"
            );

            let queued: Vec<CognitionTurn> =
                std::iter::from_fn(|| turn_rx.try_recv().ok()).collect();
            assert_eq!(queued.len(), 2, "{order}");
            for turn in queued {
                handle_turn(turn, Arc::clone(&app_state)).await;
            }
            let state = app_state.runtime_state.lock().await;
            assert_eq!(
                state.semantic_tagger_calls_total, 1,
                "analyzed once: {order}"
            );
            assert_eq!(
                state.refused_turns_not_analyzed_total,
                u64::from(order == "one batch"),
                "{order}"
            );
        }
    }

    /// A WF can send its agent one text twice in one cycle, under one key: the turn stands for both
    /// copies. The agent's refusal of one keeps it for the other, which the agent took and
    /// answers, and the worker analyzes it; the refusal of both drops it, and the worker analyzes
    /// neither.
    #[tokio::test]
    async fn a_turn_two_copies_stand_for_leaves_only_when_both_are_refused() {
        let first = supervisor_says("Approve 20% for Ana.");
        let mut second = supervisor_says("Approve 20% for Ana.");
        second.routing.trace_id = first.routing.trace_id.clone();
        trace_of(&mut second).last_mut().expect("entry")["t"] = json!("2026-10-30T12:05:00.051Z");
        assert_eq!(turn_key(&first), turn_key(&second));
        assert_ne!(request_of(&first), request_of(&second));
        let refused_first = [
            (1, TEST_TS, &first),
            (2, TEST_TS + 1, &second),
            (3, TEST_TS + 40, &busy_answer_to(&first)),
        ];

        let dir = TestDir::new();
        let (hive_id, region, unlink) = test_region("cogreftwo");
        let (app_state, mut router) = test_app_state(&hive_id, Some(region), &dir.0);
        app_state.control_state.lock().await.ai_settings_error =
            Some("no model in the tests".to_string());
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        take_stream_batch(&app_state, &turn_tx, &turns_batch(&hive_id, &refused_first)).await;
        next_outgoing(&mut router).await;
        let answer = agent_says("Approved: 20% on her next invoice.");
        let batch = turns_batch(&hive_id, &[(4, TEST_TS + 3_000, &answer)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let package = region_package(&app_state).await.expect("the package");
        assert_eq!(
            texts(&package.turns),
            ["Approve 20% for Ana.", "Approved: 20% on her next invoice."],
            "kept for the copy the agent took"
        );
        {
            let threads = app_state.thread_states.lock().await;
            let held = &threads[&key(TEST_THREAD)];
            assert_eq!(held.turns[0].requests, [request_of(&second)]);
            assert_eq!(held.turns[0].refused, [request_of(&first)]);
            assert!(held.turn_keys.contains(&turn_key(&first)));
        }
        let carrier = turn_rx.try_recv().expect("the first copy");
        assert!(!carrier.duplicate);
        assert!(turn_rx.try_recv().expect("the second copy").duplicate);
        handle_turn(carrier, Arc::clone(&app_state)).await;
        {
            let state = app_state.runtime_state.lock().await;
            assert_eq!(state.semantic_tagger_calls_total, 1, "the turn is analyzed");
            assert_eq!(state.refused_turns_not_analyzed_total, 0);
            assert_eq!(
                (state.refusals_total, state.refused_turns_dropped_total),
                (1, 0)
            );
        }
        unlink();

        // The agent refuses the second copy too: the turn leaves, and its first copy, the one the
        // worker takes, is not analyzed.
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cogreftwo", None, &dir.0);
        let (turn_tx, mut turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        take_stream_batch(
            &app_state,
            &turn_tx,
            &turns_batch("cogreftwo", &refused_first),
        )
        .await;
        next_outgoing(&mut router).await;
        let batch = turns_batch("cogreftwo", &[(4, TEST_TS + 41, &busy_answer_to(&second))]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        {
            let threads = app_state.thread_states.lock().await;
            let held = &threads[&key(TEST_THREAD)];
            assert!(held.turns.is_empty() && held.turn_keys.is_empty());
        }
        {
            let refusals = app_state.refusals.lock().expect("refusals");
            assert!(refusals.dropped(&key(TEST_THREAD), &request_of(&first)));
            assert!(refusals.dropped(&key(TEST_THREAD), &request_of(&second)));
        }
        handle_turn(
            turn_rx.try_recv().expect("the first copy"),
            Arc::clone(&app_state),
        )
        .await;
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.refused_turns_not_analyzed_total, 1);
        assert_eq!(state.semantic_tagger_calls_total, 0);
        assert_eq!(
            (state.refusals_total, state.refused_turns_dropped_total),
            (2, 1)
        );
    }

    /// The agent's copy of a relayed message finds that relay by its key: a person's same words a
    /// WF relayed in another cycle stay that cycle's relay.
    #[tokio::test]
    async fn the_agents_copy_finds_its_own_relay_by_its_key() {
        let dir = TestDir::new();
        let (app_state, mut router) = test_app_state("cogrelkey", None, &dir.0);
        let (turn_tx, _turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        let greeting = agent_says("Shall I issue the credit note?");
        let batch = turns_batch("cogrelkey", &[(1, TEST_TS, &greeting)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        let yes_once = relayed("ilk:ana", "yes");
        let yes_again = relayed("ilk:ana", "yes");
        assert_ne!(turn_key(&yes_once), turn_key(&yes_again), "two cycles");
        let batch = batch_of(
            "cogrelkey",
            RELAY_WF,
            &[
                (2, TEST_TS + 1_000, &yes_once),
                (3, TEST_TS + 60_000, &yes_again),
            ],
        );
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;
        // The first cycle's copy for the agent comes last.
        let mut copy = yes_once.clone();
        copy.routing.dst = Destination::Unicast(TEST_AGENT_NODE.to_string());
        let batch = turns_batch("cogrelkey", &[(4, TEST_TS + 60_001, &copy)]);
        take_stream_batch(&app_state, &turn_tx, &batch).await;
        next_outgoing(&mut router).await;

        let threads = app_state.thread_states.lock().await;
        let held = &threads[&key(TEST_THREAD)];
        let lines: Vec<(Option<&str>, &[String])> = held
            .turns
            .iter()
            .map(|held| (held.turn.to_via.as_deref(), held.requests.as_slice()))
            .collect();
        assert_eq!(
            lines,
            [
                (None, &[][..]),
                (None, &[request_of(&copy)][..]),
                (Some("IO.slack.team@motherbee"), &[][..]),
            ]
        );
    }

    /// The journal names the agents' refusals at once, then at most once an interval with how
    /// many came since its last line, and the end of a burst once that line is due.
    #[test]
    fn the_refusals_are_named_at_once_then_once_an_interval() {
        let mut log = RefusalLog::default();
        let start = std::time::Instant::now();
        assert_eq!(log.flush(start), None, "nothing to name");
        assert_eq!(log.note(start), Some(1));
        assert_eq!(log.note(start + Duration::from_secs(1)), None);
        assert_eq!(log.note(start + Duration::from_secs(9)), None);
        assert_eq!(log.flush(start + Duration::from_secs(9)), None, "too soon");
        assert_eq!(log.flush(start + COGNITION_REFUSAL_LOG_INTERVAL), Some(2));
        assert_eq!(log.flush(start + COGNITION_REFUSAL_LOG_INTERVAL * 3), None);
        // The next refusal after a quiet interval is named at once again.
        assert_eq!(
            log.note(start + COGNITION_REFUSAL_LOG_INTERVAL * 3),
            Some(1)
        );
    }

    // --- The worker waits for the agent's answer before a person's turn (FINDINGS A-332) ---

    /// A cognition that cannot call a model (its `ai` settings unusable), whose analysis stops at
    /// its first model call, which counts it; and the queue its intake hands the turns to.
    async fn waiting_cognition(
        hive_id: &str,
        dir: &Path,
    ) -> (
        Arc<CognitionAppState>,
        fluxbee_sdk::RouterDispatcherTestHarness,
        mpsc::Sender<CognitionTurn>,
        mpsc::Receiver<CognitionTurn>,
    ) {
        let (app_state, router) = test_app_state(hive_id, None, dir);
        app_state.control_state.lock().await.ai_settings_error =
            Some("no model in the tests".to_string());
        let (turn_tx, turn_rx) = mpsc::channel(COGNITION_TURN_QUEUE);
        (app_state, router, turn_tx, turn_rx)
    }

    /// The intake takes `records`, a batch of `hive_id`'s stream, and acks it.
    async fn take_batch(
        app_state: &CognitionAppState,
        router: &mut fluxbee_sdk::RouterDispatcherTestHarness,
        turn_tx: &mpsc::Sender<CognitionTurn>,
        hive_id: &str,
        records: &[(u64, u64, &Message)],
    ) {
        take_stream_batch(app_state, turn_tx, &turns_batch(hive_id, records)).await;
        let ack = next_outgoing(router).await;
        assert_eq!(ack.meta.msg.as_deref(), Some(MSG_STREAM_ACK));
    }

    /// The bug (FINDINGS A-332): with nothing else to do, the worker took a person's turn as soon as
    /// the intake did, and its tagger call was under way when the agent's refusal came, in the
    /// stream's next batch 40 ms later (0.037 s on 8.x). A-292 dropped the call's result; the call
    /// was made. The worker now waits for the agent's answer: the refusal ends the wait at once,
    /// and no model is called. A refusal the intake took before the worker got to its turn ends no
    /// wait: there is none. This cognition cannot call a model (`waiting_cognition`), and waits
    /// all the same: the refused turn counts as refused, not as a tagger failure.
    #[tokio::test(start_paused = true)]
    async fn a_refusal_after_the_worker_took_its_turn_costs_no_model_call() {
        let dir = TestDir::new();
        let (app_state, mut router, turn_tx, mut turn_rx) =
            waiting_cognition("cogwaitref", &dir.0).await;
        let question = person_says("Is my invoice ready?");
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitref",
            &[(1, TEST_TS, &question)],
        )
        .await;
        let start = time::Instant::now();
        let worker = tokio::spawn(handle_turn(
            turn_rx.try_recv().expect("the person's turn"),
            Arc::clone(&app_state),
        ));
        time::sleep(Duration::from_millis(40)).await;
        let busy = busy_answer_to(&question);
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitref",
            &[(2, TEST_TS + 40, &busy)],
        )
        .await;
        worker.await.expect("the worker");
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(40),
            "the refusal ends the wait"
        );
        {
            let state = app_state.runtime_state.lock().await;
            assert_eq!(state.semantic_tagger_calls_total, 0, "no model call");
            assert_eq!(state.semantic_tagger_failures_total, 0, "no tagger failure");
            assert_eq!(state.refused_turns_not_analyzed_total, 1);
            assert_eq!(
                (state.agent_waits_total, state.agent_waits_grace_over_total),
                (1, 0)
            );
        }

        let again = person_says("Is my invoice ready?");
        let busy = busy_answer_to(&again);
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitref",
            &[(3, TEST_TS + 60_000, &again), (4, TEST_TS + 60_040, &busy)],
        )
        .await;
        let start = time::Instant::now();
        handle_turn(
            turn_rx.try_recv().expect("the turn sent again"),
            Arc::clone(&app_state),
        )
        .await;
        assert_eq!(start.elapsed(), Duration::ZERO, "no wait");
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.semantic_tagger_calls_total, 0);
        assert_eq!(state.refused_turns_not_analyzed_total, 2);
        assert_eq!(state.agent_waits_total, 1);
        let status = build_status_payload(
            &app_state.node_name,
            &test_control_state(),
            &state,
            &test_runtime_paths(),
            None,
        );
        assert_eq!(status["turns_consumer"]["agent_waits_total"], json!(1));
        assert_eq!(
            status["turns_consumer"]["agent_waits_grace_over_total"],
            json!(0)
        );
        // Where SY.admin, and so Archi, reads them: CONFIG_GET (`node_control_config_get`).
        let config = build_cognition_config_get_payload(
            &app_state.node_name,
            &test_control_state(),
            &state,
            &test_runtime_paths(),
            None,
        );
        for (counter, value) in [
            ("refusals_total", 2),
            ("refused_turns_dropped_total", 2),
            ("refused_turns_not_analyzed_total", 2),
            ("agent_waits_total", 1),
            ("agent_waits_grace_over_total", 0),
        ] {
            assert_eq!(config["runtime"][counter], json!(value), "{counter}");
        }
    }

    /// A person's turn the agent took is analyzed all the same, as soon as the agent answers: here
    /// 300 ms after it, well within the grace. The agent's answer waits for nothing.
    #[tokio::test(start_paused = true)]
    async fn a_turn_the_agent_took_is_analyzed_as_soon_as_it_answers() {
        let dir = TestDir::new();
        let (app_state, mut router, turn_tx, mut turn_rx) =
            waiting_cognition("cogwaitans", &dir.0).await;
        let question = person_says("Is my invoice ready?");
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitans",
            &[(1, TEST_TS, &question)],
        )
        .await;
        let start = time::Instant::now();
        let worker = tokio::spawn(handle_turn(
            turn_rx.try_recv().expect("the person's turn"),
            Arc::clone(&app_state),
        ));
        time::sleep(Duration::from_millis(300)).await;
        let answer = answer_to(&question, "Yes: it went out this morning.");
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitans",
            &[(2, TEST_TS + 300, &answer)],
        )
        .await;
        worker.await.expect("the worker");
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(300),
            "analyzed once the agent answered, not at the grace's end"
        );
        {
            let state = app_state.runtime_state.lock().await;
            assert_eq!(state.semantic_tagger_calls_total, 1, "the turn is analyzed");
            assert_eq!(state.refused_turns_not_analyzed_total, 0);
            assert_eq!(
                (state.agent_waits_total, state.agent_waits_grace_over_total),
                (1, 0)
            );
        }

        let start = time::Instant::now();
        handle_turn(
            turn_rx.try_recv().expect("the agent's answer"),
            Arc::clone(&app_state),
        )
        .await;
        assert_eq!(
            start.elapsed(),
            Duration::ZERO,
            "the answer waits for nothing"
        );
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.semantic_tagger_calls_total, 2);
        assert_eq!(state.agent_waits_total, 1);
    }

    /// A person's turn the agent neither answers nor refuses within the grace is analyzed once the
    /// grace is over; one that waited that long in the queue already does not wait again; and one
    /// no model reads, with no person at its other end, waits for nothing.
    #[tokio::test(start_paused = true)]
    async fn a_turn_with_no_answer_is_analyzed_once_its_grace_is_over() {
        let dir = TestDir::new();
        let (app_state, mut router, turn_tx, mut turn_rx) =
            waiting_cognition("cogwaitnone", &dir.0).await;
        let question = person_says("Is my invoice ready?");
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitnone",
            &[(1, TEST_TS, &question)],
        )
        .await;
        let start = time::Instant::now();
        handle_turn(
            turn_rx.try_recv().expect("the person's turn"),
            Arc::clone(&app_state),
        )
        .await;
        assert_eq!(start.elapsed(), COGNITION_REFUSAL_GRACE);
        {
            let state = app_state.runtime_state.lock().await;
            assert_eq!(state.semantic_tagger_calls_total, 1, "the turn is analyzed");
            assert_eq!(
                (state.agent_waits_total, state.agent_waits_grace_over_total),
                (1, 1)
            );
        }

        let follow_up = person_says("And the receipt?");
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitnone",
            &[(2, TEST_TS + 60_000, &follow_up)],
        )
        .await;
        // Behind another turn's analysis for the whole grace.
        time::sleep(COGNITION_REFUSAL_GRACE).await;
        let start = time::Instant::now();
        handle_turn(
            turn_rx.try_recv().expect("the follow-up"),
            Arc::clone(&app_state),
        )
        .await;
        assert_eq!(start.elapsed(), Duration::ZERO, "it waited in the queue");
        {
            let state = app_state.runtime_state.lock().await;
            assert_eq!(state.semantic_tagger_calls_total, 2);
            assert_eq!(state.agent_waits_total, 1);
        }

        let mut reminder = turn_copy(
            "WF.sales@motherbee",
            Some("ilk:person"),
            text("Ana has not answered in 2 days: send a reminder"),
        );
        reminder.meta.context = Some(json!({"writer": {"name": "WF.sales@motherbee"}}));
        take_batch(
            &app_state,
            &mut router,
            &turn_tx,
            "cogwaitnone",
            &[(3, TEST_TS + 120_000, &reminder)],
        )
        .await;
        let start = time::Instant::now();
        handle_turn(
            turn_rx.try_recv().expect("the WF's own text"),
            Arc::clone(&app_state),
        )
        .await;
        assert_eq!(start.elapsed(), Duration::ZERO, "no model reads it");
        let state = app_state.runtime_state.lock().await;
        assert_eq!(state.turns_not_analyzed_total, 1);
        assert_eq!(state.agent_waits_total, 1);
    }

    /// An agent's answer names the message it answers by the agent's entry, the trace's last, as
    /// its refusal does; the intake notes only what the agent sent, in its communication.
    #[test]
    fn an_answer_names_the_message_it_answers_by_the_agents_entry() {
        let question = person_says("Is my invoice ready?");
        let answer = answer_to(&question, "Yes.");
        assert_eq!(
            answered_request(&answer, TEST_AGENT_NODE),
            Some(request_of(&question))
        );
        assert_eq!(answered_request(&answer, "AI.other@motherbee"), None);
        let mut forwarded = answer.clone();
        trace_of(&mut forwarded).push(json!({"t": "2026-10-30T12:00:01.000Z",
            "from": "WF.sales@motherbee", "to": "IO.api@motherbee", "act": ["forward"]}));
        assert_eq!(answered_request(&forwarded, TEST_AGENT_NODE), None);
        let mut no_entry = answer.clone();
        trace_of(&mut no_entry).pop();
        assert_eq!(answered_request(&no_entry, TEST_AGENT_NODE), None);
        let mut no_cycle = answer.clone();
        no_cycle.routing.trace_id = String::new();
        assert_eq!(answered_request(&no_cycle, TEST_AGENT_NODE), None);

        let holder = Holder::Agent {
            node: TEST_AGENT_NODE.to_string(),
            identity: AgentIdentity {
                ilk: TEST_AGENT.to_string(),
                tenant: None,
                seq: 0,
                read_at: std::time::Instant::now(),
            },
        };
        assert_eq!(
            answer_of(&answer, &holder),
            Some((key(TEST_THREAD), request_of(&question)))
        );
        assert_eq!(answer_of(&question, &holder), None, "what the agent got");
        assert_eq!(answer_of(&answer, &Holder::NoAgent), None);
        let mut no_thread = answer;
        no_thread.meta.thread_id = None;
        assert_eq!(answer_of(&no_thread, &holder), None);
    }

    /// The answers leave once no wait can look for them: a grace after a later one was taken; and
    /// past `COGNITION_ANSWERS_KEPT`, the oldest first.
    #[test]
    fn the_answers_leave_once_no_wait_looks_for_them() {
        let start = time::Instant::now();
        let mut answers = Answers::default();
        answers.record(start, key(TEST_THREAD), "first".to_string());
        answers.record(
            start + COGNITION_REFUSAL_GRACE,
            key(TEST_THREAD),
            "second".to_string(),
        );
        assert!(answers.holds(&key(TEST_THREAD), "first"), "a wait may look");
        assert!(!answers.holds(&key("thread:other"), "first"));
        assert!(!answers.holds(&key(TEST_THREAD), ""));
        answers.record(
            start + COGNITION_REFUSAL_GRACE + Duration::from_millis(1),
            key(TEST_THREAD),
            "third".to_string(),
        );
        assert!(!answers.holds(&key(TEST_THREAD), "first"), "no wait looks");
        assert!(answers.holds(&key(TEST_THREAD), "second"));

        let mut answers = Answers::default();
        for n in 0..=COGNITION_ANSWERS_KEPT {
            answers.record(start, key(TEST_THREAD), format!("answer {n}"));
        }
        assert_eq!(answers.recent.len(), COGNITION_ANSWERS_KEPT);
        assert!(!answers.holds(&key(TEST_THREAD), "answer 0"));
        assert!(answers.holds(&key(TEST_THREAD), "answer 1"));
    }
}
