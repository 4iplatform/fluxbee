use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::nats::{publish_local, resolve_local_nats_endpoint};
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    build_node_config_response_message, build_node_secret_record, connect, load_node_secret_record,
    managed_node_config_path, managed_node_instance_dir, managed_node_name,
    save_node_secret_record, try_handle_default_node_status, NodeConfig, NodeReceiver,
    NodeSecretDescriptor, NodeSecretWriteOptions, NodeSender, NodeUuidMode,
    NODE_CONFIG_APPLY_MODE_REPLACE, NODE_SECRET_REDACTION_TOKEN,
};
use fluxbee_sdk::{
    CognitionContextData, CognitionCooccurrenceData, CognitionDurableEntity,
    CognitionDurableEnvelope, CognitionDurableOp, CognitionEpisodeData, CognitionIlkProfile,
    CognitionMemoryData, CognitionReasonData, CognitionScopeData, CognitionScopeInstanceData,
    CognitionThreadData, SUBJECT_STORAGE_COGNITION_CONTEXTS,
    SUBJECT_STORAGE_COGNITION_COOCCURRENCES, SUBJECT_STORAGE_COGNITION_EPISODES,
    SUBJECT_STORAGE_COGNITION_MEMORIES, SUBJECT_STORAGE_COGNITION_REASONS,
    SUBJECT_STORAGE_COGNITION_SCOPES, SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES,
    SUBJECT_STORAGE_COGNITION_THREADS,
};
use json_router::nats::{NatsSubscriber as RouterNatsSubscriber, SUBJECT_STORAGE_TURNS};

type CognitionError = Box<dyn std::error::Error + Send + Sync>;

const COGNITION_NODE_BASE_NAME: &str = "SY.cognition";
const COGNITION_NODE_VERSION: &str = "2.0";
const COGNITION_CONFIG_SCHEMA_VERSION: u32 = 1;
const COGNITION_LOCAL_SECRET_KEY_OPENAI: &str = "openai_api_key";
const COGNITION_TURNS_SID: u32 = 27;
const DURABLE_QUEUE_TURNS: &str = "durable.sy-cognition.turns";
const COGNITION_DEFAULT_CONTEXT_OPEN_THRESHOLD: f64 = 0.5;
const COGNITION_DEFAULT_REASON_OPEN_THRESHOLD: f64 = 0.5;
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
const NATS_ERROR_LOG_EVERY: u64 = 20;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CognitionAiSecretSource {
    LocalFile,
    EnvCompat,
    Missing,
}

impl CognitionAiSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::EnvCompat => "env_compat",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CognitionThresholds {
    context_open: f64,
    reason_open: f64,
}

impl Default for CognitionThresholds {
    fn default() -> Self {
        Self {
            context_open: COGNITION_DEFAULT_CONTEXT_OPEN_THRESHOLD,
            reason_open: COGNITION_DEFAULT_REASON_OPEN_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone)]
struct CognitionControlState {
    schema_version: u32,
    config_version: u64,
    ai_secret_source: CognitionAiSecretSource,
    thresholds: CognitionThresholds,
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
    published_entities_total: u64,
    publish_errors_total: u64,
    last_trace_id: Option<String>,
    last_thread_id: Option<String>,
    last_thread_seq: Option<u64>,
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
}

#[derive(Debug)]
struct CognitionAppState {
    config_dir: PathBuf,
    hive_id: String,
    node_name: String,
    use_durable_consumer: bool,
    runtime_paths: RuntimePaths,
    control_state: Arc<Mutex<CognitionControlState>>,
    runtime_state: Arc<Mutex<CognitionRuntimeState>>,
    nats_subscribe_errors: Arc<AtomicU64>,
    thread_states: Arc<Mutex<HashMap<String, ThreadCognitionState>>>,
}

#[derive(Debug, Clone, Default)]
struct ThreadCognitionState {
    first_seen_at: Option<String>,
    last_seen_at: Option<String>,
    latest_thread_seq: Option<u64>,
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
    start_thread_seq: Option<u64>,
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

#[derive(Debug, Clone)]
struct EpisodeCandidate {
    affect_id: String,
    title: String,
    summary: String,
    base_intensity: f64,
    evidence_strength: f64,
    evidence_signals: Vec<String>,
    reason: String,
}

#[derive(Debug, Clone, Default)]
struct DeterministicTaggerOutput {
    tags: Vec<String>,
    reason_signals_canonical: Vec<String>,
    reason_signals_extra: Vec<String>,
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
    nats: Option<NatsSection>,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    #[serde(default)]
    mode: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), CognitionError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_cognition supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let hive = load_hive(&config_dir)?;
    let endpoint = resolve_local_nats_endpoint(&config_dir)?;
    let use_durable_consumer = hive
        .nats
        .as_ref()
        .and_then(|n| n.mode.as_deref())
        .map(|mode| mode.trim().eq_ignore_ascii_case("embedded"))
        .unwrap_or(true);
    let node_base_name = managed_node_name(COGNITION_NODE_BASE_NAME, &["SY_COGNITION_NODE_NAME"]);
    let node_name = ensure_l2_name(&node_base_name, &hive.hive_id);
    let runtime_paths = ensure_runtime_paths(&node_name)?;
    let ai_secret_source = resolve_openai_api_key_source(&node_name);
    let control_state = Arc::new(Mutex::new(bootstrap_cognition_control_state(
        &node_name,
        ai_secret_source,
    )?));
    let runtime_state = Arc::new(Mutex::new(CognitionRuntimeState::default()));
    let nats_subscribe_errors = Arc::new(AtomicU64::new(0));
    let app_state = Arc::new(CognitionAppState {
        config_dir: config_dir.clone(),
        hive_id: hive.hive_id.clone(),
        node_name: node_name.clone(),
        use_durable_consumer,
        runtime_paths: runtime_paths.clone(),
        control_state: Arc::clone(&control_state),
        runtime_state: Arc::clone(&runtime_state),
        nats_subscribe_errors: Arc::clone(&nats_subscribe_errors),
        thread_states: Arc::new(Mutex::new(HashMap::new())),
    });

    let node_config = NodeConfig {
        name: node_base_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: COGNITION_NODE_VERSION.to_string(),
    };
    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!(node_name = %sender.full_name(), "sy.cognition connected to router");

    std::mem::drop(tokio::spawn(run_turns_loop(
        endpoint.clone(),
        Arc::clone(&app_state),
    )));

    let startup_ai_secret_source = control_state.lock().await.ai_secret_source;

    tracing::info!(
        hive = %hive.hive_id,
        node_name = %node_name,
        endpoint = %endpoint,
        durable_turns = use_durable_consumer,
        ai_secret_source = %startup_ai_secret_source.as_str(),
        state_dir = %runtime_paths.state_dir.display(),
        "sy.cognition started"
    );

    let mut heartbeat = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let snapshot = control_state.lock().await.clone();
                tracing::debug!(
                    node_name = %sender.full_name(),
                    config_version = snapshot.config_version,
                    ai_secret_source = %snapshot.ai_secret_source.as_str(),
                    "sy.cognition heartbeat"
                );
            }
            received = receiver.recv() => {
                let msg = match received {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!(error = %err, "sy.cognition recv error; reconnecting");
                        let (new_sender, new_receiver) =
                            connect_with_retry(&node_config, Duration::from_secs(1)).await?;
                        sender = new_sender;
                        receiver = new_receiver;
                        tracing::info!(node_name = %sender.full_name(), "sy.cognition reconnected to router");
                        continue;
                    }
                };
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

async fn run_turns_loop(endpoint: String, app_state: Arc<CognitionAppState>) {
    loop {
        let subscriber = if app_state.use_durable_consumer {
            RouterNatsSubscriber::new(
                endpoint.clone(),
                SUBJECT_STORAGE_TURNS.to_string(),
                COGNITION_TURNS_SID,
            )
            .with_queue(DURABLE_QUEUE_TURNS)
        } else {
            RouterNatsSubscriber::new(
                endpoint.clone(),
                SUBJECT_STORAGE_TURNS.to_string(),
                COGNITION_TURNS_SID,
            )
        };
        let run_app_state = Arc::clone(&app_state);
        let run_result = subscriber
            .run(move |payload| {
                let app_state = Arc::clone(&run_app_state);
                async move { handle_turn_payload(payload, app_state).await }
            })
            .await;
        if let Err(err) = run_result {
            let count = app_state
                .nats_subscribe_errors
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                tracing::warn!(
                    subject = SUBJECT_STORAGE_TURNS,
                    error = %err,
                    failures = count,
                    "sy.cognition turns subscribe loop failed; retrying"
                );
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handle_turn_payload(
    payload: Vec<u8>,
    app_state: Arc<CognitionAppState>,
) -> Result<(), std::io::Error> {
    let msg: Message = match serde_json::from_slice(&payload) {
        Ok(msg) => msg,
        Err(err) => {
            let mut state = app_state.runtime_state.lock().await;
            state.invalid_turns_total = state.invalid_turns_total.saturating_add(1);
            tracing::warn!(
                error = %err,
                payload_bytes = payload.len(),
                "sy.cognition received invalid storage.turns payload"
            );
            return Ok(());
        }
    };

    let Some(thread_id) = msg
        .meta
        .thread_id
        .clone()
        .or_else(|| legacy_context_thread_id(&msg))
    else {
        let mut state = app_state.runtime_state.lock().await;
        state.invalid_turns_total = state.invalid_turns_total.saturating_add(1);
        state.last_trace_id = Some(msg.routing.trace_id.clone());
        tracing::warn!(
            trace_id = %msg.routing.trace_id,
            "sy.cognition skipping turn without thread_id"
        );
        return Ok(());
    };

    let text = extract_turn_text(&msg.payload).unwrap_or_default();
    let tagger = deterministic_tagger(&text);
    let thresholds = app_state.control_state.lock().await.thresholds.clone();
    let ts = chrono::Utc::now().to_rfc3339();
    let thread_seq = msg.meta.thread_seq;
    let src_ilk = msg.meta.src_ilk.clone();
    let dst_ilk = msg.meta.dst_ilk.clone();
    let ich = msg.meta.ich.clone();

    let envelopes = {
        let mut threads = app_state.thread_states.lock().await;
        let thread_state = threads
            .entry(thread_id.clone())
            .or_insert_with(ThreadCognitionState::default);
        update_thread_state_and_build_envelopes(
            &app_state.hive_id,
            &app_state.node_name,
            &thread_id,
            thread_seq,
            src_ilk.as_deref(),
            dst_ilk.as_deref(),
            ich.as_deref(),
            &tagger,
            &thresholds,
            &ts,
            thread_state,
        )
    };

    let mut published = 0u64;
    let mut publish_errors = 0u64;
    for (subject, body) in envelopes {
        if let Err(err) = publish_local(&app_state.config_dir, subject, &body).await {
            publish_errors = publish_errors.saturating_add(1);
            tracing::warn!(
                subject = subject,
                error = %err,
                trace_id = %msg.routing.trace_id,
                thread_id = %thread_id,
                "sy.cognition failed to publish derived cognition entity"
            );
        } else {
            published = published.saturating_add(1);
        }
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
    state.last_thread_seq = thread_seq;
    state.last_src_ilk = src_ilk;
    state.last_ich = ich;
    state.last_tags = tagger.tags;
    state.last_reason_signals_canonical = tagger.reason_signals_canonical;
    state.last_reason_signals_extra = tagger.reason_signals_extra;
    state.active_threads_total = active_threads_total;
    state.open_contexts_total = open_contexts_total;
    state.open_reasons_total = open_reasons_total;
    state.open_cooccurrences_total = open_cooccurrences_total;
    state.active_scopes_total = active_scopes_total;
    state.active_memories_total = active_memories_total;
    state.active_episodes_total = active_episodes_total;
    Ok(())
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
    if msg.meta.msg_type != SYSTEM_KIND {
        return Ok(());
    }
    let Some(command) = msg.meta.msg.as_deref() else {
        return Ok(());
    };
    match command {
        "CONFIG_GET" => {
            let snapshot = app_state.runtime_state.lock().await.clone();
            let control_state = app_state.control_state.lock().await.clone();
            let payload = build_cognition_config_get_payload(
                &app_state.node_name,
                &control_state,
                &snapshot,
                &app_state.runtime_paths,
                app_state.use_durable_consumer,
                app_state.nats_subscribe_errors.load(Ordering::Relaxed),
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
                app_state.use_durable_consumer,
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
                        app_state.use_durable_consumer,
                        app_state.nats_subscribe_errors.load(Ordering::Relaxed),
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
                        app_state.use_durable_consumer,
                        app_state.nats_subscribe_errors.load(Ordering::Relaxed),
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
    use_durable_consumer: bool,
    nats_subscribe_errors: u64,
) -> Value {
    let degraded_reasons = cognition_degraded_reasons(control_state);
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
        "ai_provider": {
            "provider": "openai",
            "configured": control_state.ai_secret_source != CognitionAiSecretSource::Missing,
            "source": control_state.ai_secret_source.as_str()
        },
        "turns_consumer": {
            "subject": SUBJECT_STORAGE_TURNS,
            "mode": if use_durable_consumer { "durable" } else { "volatile" },
            "durable_queue": if use_durable_consumer {
                Value::String(DURABLE_QUEUE_TURNS.to_string())
            } else {
                Value::Null
            },
            "processed_turns_total": runtime_state.processed_turns_total,
            "invalid_turns_total": runtime_state.invalid_turns_total,
            "subscribe_failures": nats_subscribe_errors,
            "published_entities_total": runtime_state.published_entities_total,
            "publish_errors_total": runtime_state.publish_errors_total,
            "last_trace_id": runtime_state.last_trace_id,
            "last_thread_id": runtime_state.last_thread_id,
            "last_thread_seq": runtime_state.last_thread_seq,
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
            "active_episodes_total": runtime_state.active_episodes_total
        },
        "paths": {
            "state_dir": runtime_paths.state_dir,
            "cache_dir": runtime_paths.cache_dir,
            "shm_dir": runtime_paths.shm_dir,
            "memory_lance": runtime_paths.memory_lance_path
        }
    })
}

fn build_cognition_config_get_payload(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_state: &CognitionRuntimeState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
    nats_subscribe_errors: u64,
    note: Option<&str>,
) -> Value {
    let configured = control_state.ai_secret_source != CognitionAiSecretSource::Missing;
    let mut secret_descriptor = NodeSecretDescriptor::new(
        "config.secrets.openai.api_key",
        COGNITION_LOCAL_SECRET_KEY_OPENAI,
    );
    secret_descriptor.required = false;
    secret_descriptor.configured = configured;
    secret_descriptor.persistence = control_state.ai_secret_source.as_str().to_string();

    let mut notes = vec![
        Value::String(
            "SY.cognition currently runs as a live skeleton: router control-plane plus storage.turns consumer."
                .to_string(),
        ),
        Value::String(
            "The OpenAI provider secret is optional at this stage; the node starts in degraded mode when missing."
                .to_string(),
        ),
        Value::String(
            "Deterministic v1 writer for threads, contexts, reasons, cooccurrences, scopes, scope_instances, memories, and episodes is active; SHM remains pending."
                .to_string(),
        ),
    ];
    if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
        notes.push(Value::String(note.to_string()));
    }

    json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": {
            "nats": {
                "input_subject": SUBJECT_STORAGE_TURNS,
                "consumer_mode": if use_durable_consumer { "durable" } else { "volatile" },
                "durable_queue": if use_durable_consumer { Value::String(DURABLE_QUEUE_TURNS.to_string()) } else { Value::Null }
            },
            "storage": {
                "write_subject_prefix": "storage.cognition",
                "enabled": true
            },
            "ai_providers": {
                "openai": {
                    "provider": "openai",
                    "api_key": if configured { Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()) } else { Value::Null }
                }
            },
            "thresholds": {
                "context_open": control_state.thresholds.context_open,
                "reason_open": control_state.thresholds.reason_open
            },
            "paths": {
                "state_dir": runtime_paths.state_dir,
                "cache_dir": runtime_paths.cache_dir,
                "shm_dir": runtime_paths.shm_dir,
                "memory_lance": runtime_paths.memory_lance_path
            }
        },
        "runtime": {
            "processed_turns_total": runtime_state.processed_turns_total,
            "invalid_turns_total": runtime_state.invalid_turns_total,
            "published_entities_total": runtime_state.published_entities_total,
            "publish_errors_total": runtime_state.publish_errors_total,
            "subscribe_failures": nats_subscribe_errors,
            "last_thread_id": runtime_state.last_thread_id,
            "last_thread_seq": runtime_state.last_thread_seq,
            "last_tags": runtime_state.last_tags,
            "last_reason_signals_canonical": runtime_state.last_reason_signals_canonical,
            "last_reason_signals_extra": runtime_state.last_reason_signals_extra,
            "open_cooccurrences_total": runtime_state.open_cooccurrences_total,
            "active_scopes_total": runtime_state.active_scopes_total,
            "active_memories_total": runtime_state.active_memories_total,
            "active_episodes_total": runtime_state.active_episodes_total
        },
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.cognition",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": [],
            "optional_fields": [
                "config.secrets.openai.api_key",
                "config.thresholds.context_open",
                "config.thresholds.reason_open"
            ],
            "secrets": [secret_descriptor],
            "notes": notes
        }
    })
}

fn apply_cognition_config_set(
    msg: &Message,
    node_name: &str,
    control_state: &mut CognitionControlState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
) -> Result<Value, CognitionError> {
    let Some(requested_node_name) = msg.payload.get("node_name").and_then(Value::as_str) else {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            use_durable_consumer,
            "invalid_config",
            "config-set requires node_name",
        ));
    };
    if requested_node_name != node_name {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            use_durable_consumer,
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
                use_durable_consumer,
                "unsupported_apply_mode",
                "SY.cognition currently supports only apply_mode=replace",
            ));
        }
    }

    let openai_api_key = extract_cognition_openai_api_key(&msg.payload);
    let stored_openai_secret = openai_api_key.is_some();
    let thresholds = match extract_cognition_thresholds(&msg.payload) {
        Ok(value) => value,
        Err(err) => {
            return Ok(config_error_response(
                node_name,
                control_state,
                runtime_paths,
                use_durable_consumer,
                "invalid_config",
                &err.to_string(),
            ));
        }
    };

    if let Some(api_key) = openai_api_key.as_deref() {
        persist_local_openai_api_key(
            node_name,
            api_key,
            &NodeSecretWriteOptions {
                updated_by_ilk: msg
                    .payload
                    .get("requested_by")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string),
                updated_by_label: Some("sy.cognition".to_string()),
                trace_id: Some(msg.routing.trace_id.clone()),
            },
        )?;
        control_state.ai_secret_source = CognitionAiSecretSource::LocalFile;
    }

    if let Some(thresholds) = thresholds {
        control_state.thresholds = thresholds;
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
            "thresholds": {
                "context_open": control_state.thresholds.context_open,
                "reason_open": control_state.thresholds.reason_open
            }
        },
        "stored_secrets": if stored_openai_secret {
            json!([{
                "field": "config.secrets.openai.api_key",
                "storage_key": COGNITION_LOCAL_SECRET_KEY_OPENAI,
                "value_redacted": true
            }])
        } else {
            Value::Array(Vec::new())
        },
        "message": "SY.cognition local config persisted. Provider secret affects future AI calls once the cognitive pipeline is enabled."
    }))
}

fn config_error_response(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
    code: &str,
    message: &str,
) -> Value {
    build_cognition_config_get_payload(
        node_name,
        control_state,
        &CognitionRuntimeState::default(),
        runtime_paths,
        use_durable_consumer,
        0,
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
        CognitionAiSecretSource::LocalFile | CognitionAiSecretSource::EnvCompat => "ready_scaffold",
    }
}

fn cognition_degraded_reasons(control_state: &CognitionControlState) -> Vec<&'static str> {
    let mut reasons = vec!["memory_shm_pending"];
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
    let thresholds = persisted
        .as_ref()
        .and_then(|value| {
            value
                .config
                .get("thresholds")
                .cloned()
                .and_then(|value| serde_json::from_value::<CognitionThresholds>(value).ok())
        })
        .unwrap_or_default();
    let state = CognitionControlState {
        schema_version,
        config_version,
        ai_secret_source,
        thresholds,
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
            "thresholds": {
                "context_open": state.thresholds.context_open,
                "reason_open": state.thresholds.reason_open
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

fn extract_cognition_openai_api_key(body: &Value) -> Option<String> {
    body.get("config")
        .and_then(|config| config.get("secrets"))
        .and_then(|value| value.get("openai"))
        .and_then(|value| value.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn extract_cognition_thresholds(
    body: &Value,
) -> Result<Option<CognitionThresholds>, CognitionError> {
    let Some(thresholds) = body.get("config").and_then(|value| value.get("thresholds")) else {
        return Ok(None);
    };
    let context_open = thresholds
        .get("context_open")
        .and_then(Value::as_f64)
        .unwrap_or(COGNITION_DEFAULT_CONTEXT_OPEN_THRESHOLD);
    let reason_open = thresholds
        .get("reason_open")
        .and_then(Value::as_f64)
        .unwrap_or(COGNITION_DEFAULT_REASON_OPEN_THRESHOLD);
    if !context_open.is_finite() || !(0.0..=1.0).contains(&context_open) {
        return Err(
            "config.thresholds.context_open must be a finite number between 0 and 1".into(),
        );
    }
    if !reason_open.is_finite() || !(0.0..=1.0).contains(&reason_open) {
        return Err("config.thresholds.reason_open must be a finite number between 0 and 1".into());
    }
    Ok(Some(CognitionThresholds {
        context_open,
        reason_open,
    }))
}

fn extract_turn_text(payload: &Value) -> Option<String> {
    TextV1Payload::from_value(payload)
        .ok()
        .and_then(|value| value.content)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn deterministic_tagger(text: &str) -> DeterministicTaggerOutput {
    let normalized = normalize_text(text);
    if normalized.is_empty() {
        return DeterministicTaggerOutput::default();
    }
    let tokens = tokenize(&normalized);
    let mut tags = Vec::new();
    let mut canonical = Vec::new();
    let mut extra = Vec::new();

    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "billing", "refund", "invoice", "payment", "charge", "charged",
        ],
        "billing",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["account", "login", "password", "access", "user", "profile"],
        "account",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "support", "issue", "problem", "broken", "error", "bug", "failed", "failing",
        ],
        "support",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["order", "shipping", "delivery", "package", "shipment"],
        "orders",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["identity", "tenant", "ilk", "channel", "provision"],
        "identity",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "policy",
            "permission",
            "role",
            "access",
            "authorize",
            "approval",
        ],
        "policy",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &["meeting", "schedule", "calendar", "call", "appointment"],
        "scheduling",
    );
    add_tag_if_matches(
        &mut tags,
        &tokens,
        &[
            "deploy",
            "release",
            "runtime",
            "node",
            "orchestrator",
            "service",
        ],
        "operations",
    );

    if matches_any_phrase(
        &normalized,
        &[
            "please",
            "can you",
            "could you",
            "need you",
            "i need",
            "i want",
        ],
    ) || tokens
        .iter()
        .any(|token| matches!(token.as_str(), "please" | "need" | "want"))
    {
        canonical.push("request".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "fix",
            "solve",
            "resolve",
            "refund",
            "help me",
            "not working",
            "working again",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "fix" | "solve" | "resolve" | "refund" | "help"
        )
    }) {
        canonical.push("resolve".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "why",
            "wrong",
            "broken",
            "still broken",
            "doesnt work",
            "unacceptable",
            "complaint",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "wrong" | "broken" | "complaint" | "bad" | "still"
        )
    }) {
        canonical.push("challenge".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "status",
            "update",
            "explain",
            "information",
            "details",
            "clarify",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "status" | "update" | "explain" | "details" | "clarify" | "info"
        )
    }) {
        canonical.push("inform".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "confirm",
            "verify",
            "is it correct",
            "is that correct",
            "okay?",
        ],
    ) || tokens
        .iter()
        .any(|token| matches!(token.as_str(), "confirm" | "verify" | "correct" | "ok"))
    {
        canonical.push("confirm".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &["security", "fraud", "risk", "protect", "block", "suspend"],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "security" | "fraud" | "risk" | "protect" | "block" | "suspend"
        )
    }) {
        canonical.push("protect".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "hello",
            "thanks",
            "thank you",
            "appreciate",
            "glad",
            "connect",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "hello" | "thanks" | "thank" | "appreciate" | "glad" | "connect"
        )
    }) {
        canonical.push("connect".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "cancel",
            "stop",
            "nevermind",
            "never mind",
            "close this",
            "no longer",
        ],
    ) || tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "cancel" | "stop" | "nevermind" | "close" | "abandon"
        )
    }) {
        canonical.push("abandon".to_string());
    }

    if matches_any_phrase(&normalized, &["urgent", "asap", "immediately", "right now"]) {
        extra.push("urgency".to_string());
    }
    if matches_any_phrase(
        &normalized,
        &[
            "frustrated",
            "angry",
            "broken",
            "still broken",
            "again",
            "unacceptable",
        ],
    ) {
        extra.push("frustration".to_string());
    }
    if matches_any_phrase(&normalized, &["thanks", "thank you", "appreciate"]) {
        extra.push("gratitude".to_string());
    }
    if matches_any_phrase(&normalized, &["why", "how", "confused", "dont understand"]) {
        extra.push("confusion".to_string());
    }
    if matches_any_phrase(&normalized, &["lawyer", "legal", "complaint", "escalate"]) {
        extra.push("escalation".to_string());
    }

    tags = dedup_limit(tags, COGNITION_MAX_TAGS);
    canonical = dedup_limit(
        canonical
            .into_iter()
            .filter(|value| {
                COGNITION_REASON_CANONICAL_SIGNALS
                    .iter()
                    .any(|allowed| allowed == value)
            })
            .collect(),
        COGNITION_MAX_REASON_SIGNALS,
    );
    extra = dedup_limit(extra, COGNITION_MAX_REASON_SIGNALS);

    if tags.is_empty() {
        tags = fallback_tags(&tokens, COGNITION_MAX_TAGS);
    }

    DeterministicTaggerOutput {
        tags,
        reason_signals_canonical: canonical,
        reason_signals_extra: extra,
    }
}

fn update_thread_state_and_build_envelopes(
    hive_id: &str,
    writer: &str,
    thread_id: &str,
    thread_seq: Option<u64>,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    ich: Option<&str>,
    tagger: &DeterministicTaggerOutput,
    thresholds: &CognitionThresholds,
    ts: &str,
    thread_state: &mut ThreadCognitionState,
) -> Vec<(&'static str, Vec<u8>)> {
    if thread_state.first_seen_at.is_none() {
        thread_state.first_seen_at = Some(ts.to_string());
    }
    thread_state.last_seen_at = Some(ts.to_string());
    thread_state.latest_thread_seq = thread_seq;
    thread_state.turn_count = thread_state.turn_count.saturating_add(1);

    let context_candidates = build_context_candidates(tagger);
    let reason_candidates = build_reason_candidates(tagger);

    let mut out = Vec::new();
    if let Some(body) = build_thread_envelope_body(
        hive_id,
        writer,
        thread_id,
        ts,
        thread_state,
        thread_seq,
        src_ilk,
        dst_ilk,
        ich,
    ) {
        out.push((SUBJECT_STORAGE_COGNITION_THREADS, body));
    }

    out.extend(update_contexts_for_thread(
        hive_id,
        writer,
        thread_id,
        ts,
        src_ilk,
        dst_ilk,
        thresholds.context_open,
        &context_candidates,
        &mut thread_state.contexts,
    ));
    out.extend(update_reasons_for_thread(
        hive_id,
        writer,
        thread_id,
        ts,
        src_ilk,
        dst_ilk,
        thresholds.reason_open,
        &reason_candidates,
        &mut thread_state.reasons,
    ));
    out.extend(update_cooccurrences_for_thread(
        hive_id,
        writer,
        thread_id,
        ts,
        (thresholds.context_open + thresholds.reason_open) * 0.5,
        &context_candidates,
        &reason_candidates,
        &thread_state.contexts,
        &thread_state.reasons,
        &mut thread_state.cooccurrences,
    ));
    out.extend(update_scope_binding_for_thread(
        hive_id,
        writer,
        thread_id,
        ts,
        thread_seq,
        src_ilk,
        dst_ilk,
        thread_state.turn_count,
        &thread_state.contexts,
        &thread_state.reasons,
        &mut thread_state.active_scope,
    ));
    out.extend(update_memories_and_episodes_for_thread(
        hive_id,
        writer,
        thread_id,
        ts,
        src_ilk,
        dst_ilk,
        tagger,
        &thread_state.contexts,
        &thread_state.reasons,
        &thread_state.active_scope,
        &mut thread_state.memories,
        &mut thread_state.episodes,
    ));
    out
}

fn build_context_candidates(tagger: &DeterministicTaggerOutput) -> Vec<ContextCandidate> {
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

fn build_reason_candidates(tagger: &DeterministicTaggerOutput) -> Vec<ReasonCandidate> {
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
    thread_id: &str,
    ts: &str,
    thread_state: &ThreadCognitionState,
    thread_seq: Option<u64>,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    ich: Option<&str>,
) -> Option<Vec<u8>> {
    let data = CognitionThreadData {
        latest_thread_seq: thread_seq,
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
        thread_id.to_string(),
        Some(thread_id.to_string()),
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
    thread_id: &str,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    open_threshold: f64,
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
                context_id: stable_entity_id("context", &[thread_id, &candidate.label]),
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
            Some(thread_id.to_string()),
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
        if context.weight < (open_threshold * 0.5) {
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
                Some(thread_id.to_string()),
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
    thread_id: &str,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    open_threshold: f64,
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
                reason_id: stable_entity_id("reason", &[thread_id, &candidate.label]),
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
            Some(thread_id.to_string()),
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
        if reason.weight < (open_threshold * 0.5) {
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
                Some(thread_id.to_string()),
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
    thread_id: &str,
    ts: &str,
    open_threshold: f64,
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
                        &[thread_id, &context.context_id, &reason.reason_id],
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
                Some(thread_id.to_string()),
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
        if cooccurrence.weight < (open_threshold * 0.5) {
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
                Some(thread_id.to_string()),
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
    thread_id: &str,
    ts: &str,
    thread_seq: Option<u64>,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    turn_count: u64,
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    active_scope: &mut Option<ScopeBindingState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let Some(context) = select_dominant_context(contexts) else {
        return out;
    };
    let Some(reason) = select_dominant_reason(reasons) else {
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
                start_thread_seq: thread_seq,
                unbind_streak: 0,
            };
            out.extend(build_scope_upsert_events(
                hive_id, writer, thread_id, ts, thread_seq, &opened,
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
                out.extend(build_scope_close_events(
                    hive_id, writer, thread_id, ts, thread_seq, scope,
                ));
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
                    start_thread_seq: thread_seq,
                    unbind_streak: 0,
                };
                out.extend(build_scope_upsert_events(
                    hive_id, writer, thread_id, ts, thread_seq, &opened,
                ));
                *scope = opened;
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
                out.extend(build_scope_upsert_events(
                    hive_id, writer, thread_id, ts, thread_seq, scope,
                ));
            }
        }
    }

    out
}

fn update_memories_and_episodes_for_thread(
    hive_id: &str,
    writer: &str,
    thread_id: &str,
    ts: &str,
    src_ilk: Option<&str>,
    dst_ilk: Option<&str>,
    tagger: &DeterministicTaggerOutput,
    contexts: &HashMap<String, ContextState>,
    reasons: &HashMap<String, ReasonState>,
    active_scope: &Option<ScopeBindingState>,
    memories: &mut HashMap<String, MemoryState>,
    episodes: &mut HashMap<String, EpisodeState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let Some(scope) = active_scope.as_ref() else {
        return out;
    };
    let Some(context) = contexts.get(&scope.dominant_context_label) else {
        return out;
    };
    let Some(reason) = reasons.get(&scope.dominant_reason_label) else {
        return out;
    };

    let current_ilk_weights = current_turn_ilk_weights(src_ilk, dst_ilk);
    out.extend(update_memories_for_thread(
        hive_id,
        writer,
        thread_id,
        ts,
        scope,
        context,
        reason,
        &current_ilk_weights,
        memories,
    ));
    out.extend(update_episodes_for_thread(
        hive_id, writer, thread_id, ts, tagger, scope, context, reason, episodes,
    ));
    out
}

fn update_memories_for_thread(
    hive_id: &str,
    writer: &str,
    thread_id: &str,
    ts: &str,
    scope: &ScopeBindingState,
    context: &ContextState,
    reason: &ReasonState,
    current_ilk_weights: &BTreeMap<String, f64>,
    memories: &mut HashMap<String, MemoryState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let memory_key = scope.scope_id.clone();
    let summary = summarize_scope_memory(context, reason);
    let memory = memories
        .entry(memory_key.clone())
        .or_insert_with(|| MemoryState {
            memory_id: stable_entity_id("memory", &[thread_id, &scope.scope_id]),
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
        Some(thread_id.to_string()),
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
            Some(thread_id.to_string()),
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
    thread_id: &str,
    ts: &str,
    tagger: &DeterministicTaggerOutput,
    scope: &ScopeBindingState,
    context: &ContextState,
    reason: &ReasonState,
    episodes: &mut HashMap<String, EpisodeState>,
) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    let Some(candidate) = build_episode_candidate(tagger, context, reason) else {
        return out;
    };

    let episode_key = format!("{}|{}", scope.scope_instance_id, candidate.affect_id);
    let episode = episodes.entry(episode_key).or_insert_with(|| EpisodeState {
        episode_id: stable_entity_id(
            "episode",
            &[thread_id, &scope.scope_instance_id, &candidate.affect_id],
        ),
        scope_id: scope.scope_id.clone(),
        scope_instance_id: scope.scope_instance_id.clone(),
        affect_id: candidate.affect_id.clone(),
        title: candidate.title.clone(),
        summary: candidate.summary.clone(),
        base_intensity: candidate.base_intensity,
        evidence_strength: candidate.evidence_strength,
        evidence_context_ids: vec![context.context_id.clone()],
        evidence_reason_ids: vec![reason.reason_id.clone()],
        evidence_signals: candidate.evidence_signals.clone(),
        intensity: candidate.base_intensity,
        reason: candidate.reason.clone(),
        created_at: ts.to_string(),
    });

    episode.scope_id = scope.scope_id.clone();
    episode.scope_instance_id = scope.scope_instance_id.clone();
    episode.title = candidate.title.clone();
    episode.summary = candidate.summary.clone();
    episode.base_intensity = episode.base_intensity.max(candidate.base_intensity);
    episode.evidence_strength = episode.evidence_strength.max(candidate.evidence_strength);
    episode.intensity = episode.intensity.max(candidate.base_intensity);
    episode.reason = candidate.reason.clone();
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
        Some(thread_id.to_string()),
        hive_id.to_string(),
        writer.to_string(),
        ts.to_string(),
        data,
    )) {
        out.push((SUBJECT_STORAGE_COGNITION_EPISODES, body));
    }

    out
}

fn summarize_scope_memory(context: &ContextState, reason: &ReasonState) -> String {
    format!(
        "The thread recurrently centers on {} with a dominant drive of {}.",
        context.label, reason.label
    )
}

fn build_episode_candidate(
    tagger: &DeterministicTaggerOutput,
    context: &ContextState,
    reason: &ReasonState,
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
    let mut summary = String::new();
    let mut reason_text = String::new();
    let mut evidence_signals = Vec::new();
    let mut base_intensity = 0.0;
    let mut evidence_strength = 0.0;

    if extra_signals.contains("frustration")
        && (canonical_signals.contains("challenge") || canonical_signals.contains("resolve"))
    {
        affect_id = Some("anger".to_string());
        title = format!("Friction around {}", context.label);
        summary = format!(
            "Strong friction emerged around {} with the drive of {}.",
            context.label, reason.label
        );
        reason_text =
            "Frustration aligned with a confrontational or urgent resolution drive".to_string();
        evidence_signals.extend(["frustration".to_string(), "challenge".to_string()]);
        base_intensity = 8.0;
        evidence_strength = 9.0;
    } else if extra_signals.contains("escalation")
        && (canonical_signals.contains("protect") || canonical_signals.contains("challenge"))
    {
        affect_id = Some("escalation".to_string());
        title = format!("Escalation around {}", context.label);
        summary = format!(
            "Escalation pressure appeared around {} with the drive of {}.",
            context.label, reason.label
        );
        reason_text = "Escalation signal with protective or confrontational framing".to_string();
        evidence_signals.extend(["escalation".to_string(), "protect".to_string()]);
        base_intensity = 8.0;
        evidence_strength = 9.0;
    } else if extra_signals.contains("urgency")
        && canonical_signals.contains("resolve")
        && canonical_signals.contains("request")
    {
        affect_id = Some("urgency".to_string());
        title = format!("Urgent push on {}", context.label);
        summary = format!(
            "Urgent pressure formed around {} while seeking {}.",
            context.label, reason.label
        );
        reason_text =
            "Urgency combined with explicit assistance and resolution seeking".to_string();
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
        summary,
        base_intensity,
        evidence_strength,
        evidence_signals,
        reason: reason_text,
    })
}

fn build_scope_upsert_events(
    hive_id: &str,
    writer: &str,
    thread_id: &str,
    ts: &str,
    thread_seq: Option<u64>,
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
        None,
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
        start_thread_seq: scope.start_thread_seq,
        end_thread_seq: thread_seq,
        opened_at: Some(scope.opened_at.clone()),
        closed_at: None,
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::ScopeInstance,
        CognitionDurableOp::Upsert,
        scope.scope_instance_id.clone(),
        Some(thread_id.to_string()),
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
    thread_id: &str,
    ts: &str,
    thread_seq: Option<u64>,
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
        None,
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
        start_thread_seq: scope.start_thread_seq,
        end_thread_seq: thread_seq,
        opened_at: Some(scope.opened_at.clone()),
        closed_at: Some(ts.to_string()),
    };
    if let Ok(body) = serde_json::to_vec(&CognitionDurableEnvelope::new(
        CognitionDurableEntity::ScopeInstance,
        CognitionDurableOp::Close,
        scope.scope_instance_id.clone(),
        Some(thread_id.to_string()),
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
) -> Option<&'a ContextState> {
    contexts
        .values()
        .filter(|context| context.status == "open")
        .max_by(|left, right| left.weight.total_cmp(&right.weight))
}

fn select_dominant_reason<'a>(
    reasons: &'a HashMap<String, ReasonState>,
) -> Option<&'a ReasonState> {
    reasons
        .values()
        .filter(|reason| reason.status == "open")
        .max_by(|left, right| left.weight.total_cmp(&right.weight))
}

fn current_turn_ilk_weights(src_ilk: Option<&str>, dst_ilk: Option<&str>) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    if let Some(src_ilk) = src_ilk.map(str::trim).filter(|value| !value.is_empty()) {
        *out.entry(src_ilk.to_string()).or_insert(0.0) += 1.0;
    }
    if let Some(dst_ilk) = dst_ilk.map(str::trim).filter(|value| !value.is_empty()) {
        *out.entry(dst_ilk.to_string()).or_insert(0.0) += 0.5;
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

fn normalize_text(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn matches_any_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| text.contains(phrase))
}

fn add_tag_if_matches(tags: &mut Vec<String>, tokens: &[String], lexicon: &[&str], label: &str) {
    if tokens
        .iter()
        .any(|token| lexicon.iter().any(|candidate| candidate == token))
    {
        tags.push(label.to_string());
    }
}

fn dedup_limit(values: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        out.push(value);
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn fallback_tags(tokens: &[String], limit: usize) -> Vec<String> {
    let stopwords: HashSet<&str> = [
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "for", "from", "how", "i",
        "if", "in", "is", "it", "me", "my", "of", "on", "or", "our", "please", "so", "that", "the",
        "this", "to", "we", "what", "why", "you", "your",
    ]
    .into_iter()
    .collect();
    dedup_limit(
        tokens
            .iter()
            .filter(|token| token.len() >= 4 && !stopwords.contains(token.as_str()))
            .cloned()
            .collect(),
        limit,
    )
}

fn persist_local_openai_api_key(
    node_name: &str,
    api_key: &str,
    options: &NodeSecretWriteOptions,
) -> Result<(), CognitionError> {
    let mut secrets = load_node_secret_record(node_name)
        .map(|record| record.secrets)
        .unwrap_or_else(|_| Map::new());
    secrets.insert(
        COGNITION_LOCAL_SECRET_KEY_OPENAI.to_string(),
        Value::String(api_key.to_string()),
    );
    let record = build_node_secret_record(secrets, options);
    save_node_secret_record(node_name, &record)?;
    Ok(())
}

fn load_local_openai_api_key(node_name: &str) -> Option<String> {
    load_node_secret_record(node_name)
        .ok()
        .and_then(|record| {
            record
                .secrets
                .get(COGNITION_LOCAL_SECRET_KEY_OPENAI)
                .cloned()
        })
        .and_then(|value| value.as_str().map(ToString::to_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_openai_api_key_source(node_name: &str) -> CognitionAiSecretSource {
    if load_local_openai_api_key(node_name).is_some() {
        return CognitionAiSecretSource::LocalFile;
    }
    if std::env::var("OPENAI_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return CognitionAiSecretSource::EnvCompat;
    }
    CognitionAiSecretSource::Missing
}

fn legacy_context_thread_id(msg: &Message) -> Option<String> {
    msg.meta
        .context
        .as_ref()
        .and_then(|value| value.get("thread_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, CognitionError> {
    let path = config_dir.join("hive.yaml");
    let raw = fs::read_to_string(&path)?;
    Ok(serde_yaml::from_str(&raw)?)
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!(error = %err, "sy.cognition router connect failed; retrying");
                time::sleep(delay).await;
            }
        }
    }
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
