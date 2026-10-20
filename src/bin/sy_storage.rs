use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time;
use tokio_postgres::{error::SqlState, Client, Config as PgConfig, GenericClient, NoTls, Row};
use tracing_subscriber::EnvFilter;

use fluxbee_sdk::protocol::{
    is_system_kind, Destination, Message, Meta, Routing, VaultSecretChangedPayload,
    VaultSecretInterest, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, managed_node_config_path, managed_node_name,
    try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, VaultCallerOwned, VaultClient,
    NODE_CONFIG_APPLY_MODE_REPLACE,
};
use fluxbee_sdk::{
    CognitionContextData, CognitionCooccurrenceData, CognitionDurableEntity,
    CognitionDurableEnvelope, CognitionDurableOp, CognitionEnvelopeBatch, CognitionEpisodeData,
    CognitionMemoryData, CognitionReasonData, CognitionScopeData, CognitionScopeInstanceData,
    CognitionStateGet, CognitionStateResponse, CognitionThreadData, StorageInboxMetrics,
    StorageMetricsResponse, COGNITION_DURABLE_SCHEMA_VERSION, MSG_COGNITION_STATE_GET,
    MSG_COGNITION_STATE_RESPONSE, MSG_STORAGE_METRICS_GET, MSG_STORAGE_METRICS_RESPONSE,
    SUBJECT_STORAGE_COGNITION_CONTEXTS, SUBJECT_STORAGE_COGNITION_COOCCURRENCES,
    SUBJECT_STORAGE_COGNITION_EPISODES, SUBJECT_STORAGE_COGNITION_MEMORIES,
    SUBJECT_STORAGE_COGNITION_REASONS, SUBJECT_STORAGE_COGNITION_SCOPES,
    SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES, SUBJECT_STORAGE_COGNITION_THREADS,
};
use fluxbee_sdk::{
    TenantSource, UsageGroup, UsageGroupBy, UsageQuery, UsageQueryResponse, UsageRecord,
    DEFAULT_ROOT_TENANT_ID, MSG_USAGE_QUERY, MSG_USAGE_QUERY_RESPONSE, USAGE_QUERY_DEFAULT_LIMIT,
    USAGE_QUERY_MAX_DAYS, USAGE_QUERY_MAX_LIMIT,
};

type StorageError = Box<dyn std::error::Error + Send + Sync>;
use fluxbee_sdk::stream::{
    is_stream_node_name, stream_hive, StreamBatch, MSG_STREAM_ACK, MSG_STREAM_BATCH,
    SUBJECT_STORAGE_COGNITION_BATCH,
    SUBJECT_STORAGE_COGNITION_PREFIX as STREAM_SUBJECT_COGNITION_PREFIX, SUBJECT_STORAGE_TURNS,
    SUBJECT_STORAGE_USAGE,
};
use json_router::router::system_policy::PRIMARY_HIVE_ID;

const INBOX_REPLAY_BATCH_SIZE: i64 = 200;
const INBOX_REPLAY_MAX_ROUNDS: u32 = 20;
const INBOX_ERROR_MAX_LEN: usize = 1024;
/// How often SY.storage retries the inbox rows that failed and purges the processed ones. Until
/// 0.1.74 failed rows were retried only at startup (A-70).
const INBOX_MAINTENANCE_INTERVAL_SECS: u64 = 60;
/// A failed row is retried until it has this many attempts; then it stays, with its last error,
/// visible in the inbox metrics.
const INBOX_MAX_ATTEMPTS: i32 = 10;
/// A row younger than this is left to its live handler, so a retry never races it.
const INBOX_RETRY_MIN_AGE_SECS: i32 = 60;
/// Processed rows are kept this long, then purged: `turns` holds the whole message, so the inbox
/// only absorbs redeliveries and restarts.
const STORAGE_INBOX_RETENTION_HOURS: i32 = 24;
/// The most processed rows one purge deletes, so a purge is never one huge transaction.
const INBOX_PURGE_BATCH: i64 = 10_000;
/// How many stream batches may wait for the ingest task. Past it a batch is not acked, and its
/// stream sends it again (docs/router-stream-v1.md §2.7).
const STREAM_INGEST_QUEUE: usize = 64;
/// How many acked batches may wait for their rows to be processed, off the ack's path. Past it the
/// rows wait in the inbox for the retry (`INBOX_RETRY_MIN_AGE_SECS`).
const STREAM_PROCESS_QUEUE: usize = 64;
/// The most bytes of envelopes one `COGNITION_STATE_RESPONSE` carries: well inside a frame.
const COGNITION_STATE_PAGE_BYTES: usize = 64 * 1024;
/// The order a communication's state is paged in, entity by entity (§6.2).
const COGNITION_STATE_ORDER: [CognitionDurableEntity; 8] = [
    CognitionDurableEntity::Thread,
    CognitionDurableEntity::Context,
    CognitionDurableEntity::Reason,
    CognitionDurableEntity::Cooccurrence,
    CognitionDurableEntity::Scope,
    CognitionDurableEntity::ScopeInstance,
    CognitionDurableEntity::Memory,
    CognitionDurableEntity::Episode,
];
/// `USAGE_QUERY`'s page when the question names none, and the largest it may name.
/// The most bytes of records or groups one `USAGE_QUERY_RESPONSE` carries: with its cursor (a time
/// and a field of at most `USAGE_FIELD_MAX_CHARS` characters) the answer stays under 64 KiB.
const USAGE_QUERY_PAGE_BYTES: usize = 62 * 1024;

/// The message log (A-70, Q7 of docs/router-stream-v1.md): one row per copied message, keyed by
/// the record's inbox key (`idx_turns_dedupe_key`, unique). A reply shares its request's
/// `trace_id`, and so its `ctx` and `seq`, and still gets a row of its own. Each row keeps the
/// whole message, its thread and its trace, and the time the router copied it (`ts`); `stored_at`
/// is when SY.storage wrote it, the order a live reader follows, since a stream's batch can arrive
/// late.
const TURNS_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS turns (
    dedupe_key  TEXT NOT NULL,
    ctx         TEXT NOT NULL,
    seq         BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now(),
    stored_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    from_ilk    TEXT NOT NULL,
    to_ilk      TEXT,
    ich         TEXT NOT NULL,
    msg_type    TEXT NOT NULL,
    content     JSONB NOT NULL,
    tags        TEXT[],
    thread_id   TEXT,
    trace_id    TEXT,
    message     JSONB NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_turns_dedupe_key ON turns (dedupe_key);
CREATE INDEX IF NOT EXISTS idx_turns_ctx_seq ON turns (ctx, seq);
CREATE INDEX IF NOT EXISTS idx_turns_ts_key ON turns (ts DESC, dedupe_key DESC);
CREATE INDEX IF NOT EXISTS idx_turns_stored_at ON turns (stored_at, dedupe_key);
CREATE INDEX IF NOT EXISTS idx_turns_thread ON turns (thread_id);
"#;

/// Every record lands in the inbox first, keyed by `dedupe_key` (`message_dedupe_key`), so a
/// resent batch lands on the same row. `source_ts` is when the router appended the record.
const INBOX_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS storage_inbox (
    dedupe_key   TEXT PRIMARY KEY,
    subject      TEXT NOT NULL,
    payload      BYTEA NOT NULL,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    source_ts    TIMESTAMPTZ,
    attempts     INT NOT NULL DEFAULT 0,
    processed_at TIMESTAMPTZ,
    last_error   TEXT
);
CREATE INDEX IF NOT EXISTS idx_storage_inbox_pending ON storage_inbox(processed_at, received_at);
"#;

/// Cognition's tables, v2 (A-70 stage 2a, docs/router-stream-v1.md §6.2): a communication is
/// `(thread_id, agent_ilk)`; every row of one carries both, and the turn it was written at
/// (`source_turn`), which orders it. A thread row is keyed by its communication.
const COGNITION_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS cognition_threads (
    communication_id TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    src_ilk          TEXT,
    dst_ilk          TEXT,
    ich              TEXT,
    status           TEXT,
    first_seen_at    TEXT,
    last_seen_at     TEXT,
    turn_count       BIGINT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_contexts (
    context_id       TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    scope_id         TEXT,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    label            TEXT NOT NULL,
    status           TEXT NOT NULL,
    score            DOUBLE PRECISION,
    weight           DOUBLE PRECISION,
    weight_avg_cumulative DOUBLE PRECISION,
    weight_avg_ema   DOUBLE PRECISION,
    weight_samples   BIGINT,
    tags             TEXT[],
    ilk_weights      JSONB,
    ilk_profile      JSONB,
    opened_at        TEXT,
    last_seen_at     TEXT,
    closed_at        TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_reasons (
    reason_id        TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    scope_id         TEXT,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    label            TEXT NOT NULL,
    status           TEXT NOT NULL,
    score            DOUBLE PRECISION,
    weight           DOUBLE PRECISION,
    weight_avg_cumulative DOUBLE PRECISION,
    weight_avg_ema   DOUBLE PRECISION,
    weight_samples   BIGINT,
    signals_canonical TEXT[],
    signals_extra    TEXT[],
    ilk_weights      JSONB,
    ilk_profile      JSONB,
    opened_at        TEXT,
    last_seen_at     TEXT,
    closed_at        TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_cooccurrences (
    cooccurrence_id  TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    context_id       TEXT NOT NULL,
    context_label    TEXT NOT NULL,
    reason_id        TEXT NOT NULL,
    reason_label     TEXT NOT NULL,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    status           TEXT NOT NULL,
    score            DOUBLE PRECISION,
    weight           DOUBLE PRECISION,
    weight_avg_cumulative DOUBLE PRECISION,
    weight_avg_ema   DOUBLE PRECISION,
    weight_samples   BIGINT,
    occurrences      BIGINT,
    opened_at        TEXT,
    last_seen_at     TEXT,
    closed_at        TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_scopes (
    scope_id         TEXT PRIMARY KEY,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    label            TEXT,
    status           TEXT NOT NULL,
    dominant_context_id TEXT,
    dominant_reason_id TEXT,
    binding_energy_ema DOUBLE PRECISION,
    opened_at        TEXT,
    last_seen_at     TEXT,
    closed_at        TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_scope_instances (
    scope_instance_id TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    scope_id         TEXT NOT NULL,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    dominant_context_id TEXT,
    dominant_reason_id TEXT,
    start_turn       BIGINT,
    end_turn         BIGINT,
    opened_at        TEXT,
    closed_at        TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_memories (
    memory_id        TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    scope_id         TEXT,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    summary          TEXT NOT NULL,
    weight           DOUBLE PRECISION,
    occurrences      BIGINT,
    dominant_context_id TEXT,
    dominant_reason_id TEXT,
    ilk_weights      JSONB,
    created_at       TEXT,
    last_seen_at     TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cognition_episodes (
    episode_id       TEXT PRIMARY KEY,
    thread_id        TEXT NOT NULL,
    agent_ilk        TEXT NOT NULL,
    source_turn      BIGINT NOT NULL DEFAULT 0,
    scope_id         TEXT,
    entity_version   INT NOT NULL,
    writer           TEXT NOT NULL,
    source_ts        TEXT NOT NULL,
    affect_id        TEXT NOT NULL,
    title            TEXT NOT NULL,
    summary          TEXT NOT NULL,
    base_intensity   DOUBLE PRECISION,
    evidence_strength DOUBLE PRECISION,
    evidence_context_ids TEXT[],
    evidence_reason_ids TEXT[],
    evidence_signals TEXT[],
    intensity        DOUBLE PRECISION,
    reason           TEXT,
    created_at       TEXT,
    payload          JSONB NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cognition_contexts_scope ON cognition_contexts(scope_id);
CREATE INDEX IF NOT EXISTS idx_cognition_reasons_scope ON cognition_reasons(scope_id);
CREATE INDEX IF NOT EXISTS idx_cognition_cooccurrences_context ON cognition_cooccurrences(context_id);
CREATE INDEX IF NOT EXISTS idx_cognition_cooccurrences_reason ON cognition_cooccurrences(reason_id);
CREATE INDEX IF NOT EXISTS idx_cognition_scope_instances_scope ON cognition_scope_instances(scope_id);
CREATE INDEX IF NOT EXISTS idx_cognition_memories_scope ON cognition_memories(scope_id);
CREATE INDEX IF NOT EXISTS idx_cognition_episodes_scope ON cognition_episodes(scope_id);
CREATE INDEX IF NOT EXISTS idx_cognition_threads_communication ON cognition_threads(thread_id, agent_ilk);
CREATE INDEX IF NOT EXISTS idx_cognition_contexts_communication ON cognition_contexts(thread_id, agent_ilk);
CREATE INDEX IF NOT EXISTS idx_cognition_reasons_communication ON cognition_reasons(thread_id, agent_ilk);
CREATE INDEX IF NOT EXISTS idx_cognition_cooccurrences_communication ON cognition_cooccurrences(thread_id, agent_ilk);
CREATE INDEX IF NOT EXISTS idx_cognition_scope_instances_communication ON cognition_scope_instances(thread_id, agent_ilk);
CREATE INDEX IF NOT EXISTS idx_cognition_memories_communication ON cognition_memories(thread_id, agent_ilk);
CREATE INDEX IF NOT EXISTS idx_cognition_episodes_communication ON cognition_episodes(thread_id, agent_ilk);
"#;

/// LLM usage records (A-70 stage 3, docs/router-stream-v1.md §7.5), one row per model call:
/// `claimed_tenant_id` is the tenant the record named, `tenant_id` the one SY.storage charged, and
/// `verified` is false when identity never answered. A response lands once (`record_id`, and the
/// provider's response id where there is one). No foreign key to the tenants, so a purge of one
/// stays free; nothing is purged from it. `stored_at` is the `now()` of an autocommit insert on the
/// one shared connection, so it follows commit order: a `since` cursor never passes a row that
/// commits later. Inserting from another connection or inside a transaction would break that.
const USAGE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS llm_usage (
    record_id            TEXT PRIMARY KEY,
    provider_response_id TEXT,
    ts                   TIMESTAMPTZ NOT NULL,
    stored_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    hive                 TEXT NOT NULL,
    node_name            TEXT NOT NULL,
    node_ilk             TEXT,
    tenant_id            TEXT NOT NULL,
    tenant_source        TEXT NOT NULL,
    claimed_tenant_id    TEXT NOT NULL,
    verified             BOOLEAN NOT NULL DEFAULT TRUE,
    counterpart_ilk      TEXT,
    provider             TEXT NOT NULL,
    model                TEXT NOT NULL,
    served_model         TEXT,
    service_tier         TEXT,
    kind                 TEXT NOT NULL,
    outcome              TEXT NOT NULL,
    uncached_input       BIGINT,
    cache_read           BIGINT,
    cache_write          BIGINT,
    output               BIGINT,
    reasoning            BIGINT,
    raw_usage            JSONB,
    trace_id             TEXT,
    thread_id            TEXT,
    work                 JSONB
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_llm_usage_provider_response ON llm_usage (provider_response_id) WHERE provider_response_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_llm_usage_tenant_ts ON llm_usage (tenant_id, ts);
CREATE INDEX IF NOT EXISTS idx_llm_usage_node_ts ON llm_usage (node_name, ts);
CREATE INDEX IF NOT EXISTS idx_llm_usage_ts ON llm_usage (ts);
CREATE INDEX IF NOT EXISTS idx_llm_usage_stored ON llm_usage (stored_at, record_id);
CREATE INDEX IF NOT EXISTS idx_llm_usage_tenant_stored ON llm_usage (tenant_id, stored_at, record_id);
"#;
const STORAGE_METRICS_LOG_INTERVAL_SECS: u64 = 30;
const STORAGE_METRICS_WARN_PENDING: i64 = 100;
const STORAGE_METRICS_WARN_AGE_SECS: i64 = 120;
const STORAGE_DB_NAME: &str = "fluxbee_storage";
const STORAGE_NODE_BASE_NAME: &str = "SY.storage";
const STORAGE_NODE_VERSION: &str = "2.0";
const STORAGE_CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageDbSecretSource {
    /// `postgres_url_ref` is persisted locally and was resolvable through
    /// SY.vault at the last `resolve_database_url` attempt. Note: the variant
    /// name stays "LocalFile" for backward compatibility with already-persisted
    /// state files, but the meaning is now "vault ref present locally".
    LocalFile,
    Missing,
}

impl StorageDbSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "vault",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone)]
struct StorageControlState {
    schema_version: u32,
    config_version: u64,
    secret_source: StorageDbSecretSource,
    /// Self ILK and tenant cached for matching VAULT_SECRET_CHANGED
    /// broadcasts. Filled at boot from
    /// `wait_for_self_system_ilk_id` and `DEFAULT_ROOT_TENANT_ID`.
    self_ilk_id: String,
    self_tenant_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StorageConfigStateFile {
    schema_version: u32,
    config_version: u64,
    node_name: String,
    config: Value,
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
}

struct Storage {
    client: Client,
    /// Its own connection for the stream's batches, one transaction each: the shared client
    /// cannot run a transaction, and a `BEGIN` on it would take in the other loops' statements.
    ingest: tokio::sync::Mutex<Client>,
    /// The database, for `USAGE_QUERY`'s connection.
    database_config: PgConfig,
    /// `USAGE_QUERY`'s own connection (§7.5), so a slow question never holds the others': opened at
    /// the first question and again after one failed. A question holds it until it is answered, so
    /// they run one at a time.
    usage_reader: tokio::sync::Mutex<Option<Client>>,
    /// What the usage records' checks found since the last report.
    usage_counts: std::sync::Mutex<UsageCounts>,
}

#[derive(Debug)]
struct TurnRecord {
    ctx: String,
    seq: i64,
    from_ilk: String,
    to_ilk: Option<String>,
    ich: String,
    msg_type: String,
    content: Value,
    tags: Vec<String>,
    thread_id: Option<String>,
    trace_id: String,
    /// The whole message, as the router copied it: `turns` is the message log (Archi reads it).
    message: Value,
}

#[tokio::main]
async fn main() -> Result<(), StorageError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_storage supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let hive = load_hive(&config_dir).await?;
    // Model D': self-ILK is deterministic from L2 name (no SHM wait).
    let self_ilk_id =
        fluxbee_sdk::deterministic_system_ilk_id(&format!("SY.storage@{}", hive.hive_id));
    tracing::info!(self_ilk_id = %self_ilk_id, "self system ILK computed deterministically");
    if hive.hive_id == PRIMARY_HIVE_ID && !is_mother_role(hive.role.as_deref()) {
        return Err(format!(
            "invalid hive.yaml: hive_id='{}' is reserved for role=motherbee",
            PRIMARY_HIVE_ID
        )
        .into());
    }
    if !is_mother_role(hive.role.as_deref()) {
        tracing::warn!(
            hive = %hive.hive_id,
            "SY.storage solo corre en motherbee; role != motherbee"
        );
        return Ok(());
    }
    if hive.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: role=motherbee requires hive_id='{}' (got '{}')",
            PRIMARY_HIVE_ID, hive.hive_id
        )
        .into());
    }

    let node_base_name = managed_node_name(STORAGE_NODE_BASE_NAME, &["SY_STORAGE_NODE_NAME"]);
    let node_name = ensure_l2_name(&node_base_name, &hive.hive_id);
    let node_config = NodeConfig {
        name: node_base_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: STORAGE_NODE_VERSION.to_string(),
    };
    // SY.storage is a system node; its tenant is the hive's root tenant.
    // Model D' pool match searches that tenant; there is no `sys` tenant
    // sentinel in the canonical model.
    let my_tenant = fluxbee_sdk::DEFAULT_ROOT_TENANT_ID;

    // Option-C fix (race ARCHI-BUG-12): connect to the router with our
    // PERSISTENT identity FIRST, so we are announced before any vault
    // lookup happens. The vault lookup then reuses this same connection.
    // If vault is not yet up (we may arrive before it), the lookup returns
    // a degraded state and we proceed — but because we are persistently
    // registered, the bootstrap VAULT_SECRET_CHANGED broadcast vault emits
    // at its own startup lands in our receive loop and triggers the
    // exit(0) rescue.
    let profile = build_storage_rpc_profile()
        .map_err(|err| format!("sy.storage rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    tracing::info!(node_name = %sender.full_name(), "sy.storage connected to router");

    let vault_client = VaultClient::new(
        dispatcher.clone(),
        hive.hive_id.clone(),
        VaultCallerOwned::new(self_ilk_id.clone(), node_name.clone()),
    );

    let (database_url, db_secret_source, vault_lookup_error) =
        resolve_database_url(&vault_client, &node_name, my_tenant).await;
    if let Some(err) = vault_lookup_error.as_deref() {
        tracing::warn!(
            node_name = %node_name,
            error = err,
            "SY.storage vault lookup for postgres_url_ref failed; starting degraded"
        );
    }
    let mut control_state = bootstrap_storage_control_state(
        &node_name,
        db_secret_source,
        self_ilk_id.clone(),
        my_tenant.to_string(),
    )?;

    let storage = initialize_storage_backend(database_url.as_deref(), &node_name).await;
    if let Some(storage) = storage.as_ref() {
        let replayed = storage
            .replay_pending_messages(INBOX_REPLAY_BATCH_SIZE, INBOX_REPLAY_MAX_ROUNDS, 0)
            .await?;
        if replayed > 0 {
            tracing::info!(count = replayed, "replayed pending storage inbox messages");
        }
    } else {
        tracing::warn!(
            node_name = %node_name,
            db_secret_source = %db_secret_source.as_str(),
            "sy.storage started without active DB backend; CONFIG_SET + restart required"
        );
    }

    tracing::info!(
        hive = %hive.hive_id,
        node_name = %node_name,
        db_secret_source = %db_secret_source.as_str(),
        storage_ready = storage.is_some(),
        "sy.storage started"
    );

    // Everything reaches SY.storage over the mesh (A-70): the routers' streams bring turns,
    // cognition's outputs and the usage records as STREAM_BATCH; SY.admin and SY.orchestrator ask
    // for the inbox metrics, and SY.admin for the usage records.
    if let Some(storage) = storage.as_ref() {
        std::mem::drop(tokio::spawn(run_storage_metrics_loop(Arc::clone(storage))));
        std::mem::drop(tokio::spawn(run_inbox_maintenance_loop(Arc::clone(
            storage,
        ))));
    }

    // Vault changes arrive event-driven via VAULT_SECRET_CHANGED
    // broadcasts from SY.vault — no polling loop. See
    // `handle_vault_secret_changed` in process_router_message.

    let mut heartbeat = time::interval(Duration::from_secs(5));
    let ingest_tx = storage.as_ref().map(|storage| {
        let (tx, rx) = tokio::sync::mpsc::channel(STREAM_INGEST_QUEUE);
        std::mem::drop(tokio::spawn(run_stream_ingest(
            Arc::clone(storage),
            Arc::clone(&dispatcher),
            rx,
        )));
        tx
    });
    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| format!("sy.storage system receiver: {err}"))?;
    // One inbox metrics scan at a time; a request that finds one running is told so.
    let metrics_gate = Arc::new(tokio::sync::Semaphore::new(1));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                tracing::debug!(
                    node_name = %sender.full_name(),
                    config_version = control_state.config_version,
                    db_secret_source = %control_state.secret_source.as_str(),
                    "sy.storage heartbeat"
                );
            }
            maybe_msg = system_rx.recv() => {
                let Some(msg) = maybe_msg else {
                    tracing::warn!("sy.storage system channel closed; exiting main loop");
                    return Ok(());
                };
                if msg.meta.msg.as_deref() == Some(MSG_COGNITION_STATE_GET) {
                    // Answered on a task of its own: a page is a few queries, and the main loop
                    // keeps taking the streams' batches meanwhile.
                    match storage.as_ref() {
                        Some(storage) => {
                            std::mem::drop(tokio::spawn(answer_cognition_state(
                                Arc::clone(storage),
                                Arc::clone(&dispatcher),
                                msg,
                            )));
                        }
                        None => {
                            let sender = dispatcher.sender_snapshot();
                            let reply = build_system_reply(
                                &sender,
                                &msg,
                                MSG_COGNITION_STATE_RESPONSE,
                                serde_json::to_value(CognitionStateResponse {
                                    error: Some("sy.storage has no database".to_string()),
                                    ..CognitionStateResponse::default()
                                })
                                .unwrap_or(Value::Null),
                            );
                            let _ = sender.send(reply).await;
                        }
                    }
                    continue;
                }
                if msg.meta.msg.as_deref() == Some(MSG_STORAGE_METRICS_GET) {
                    std::mem::drop(tokio::spawn(answer_storage_metrics(
                        storage.clone(),
                        Arc::clone(&metrics_gate),
                        Arc::clone(&dispatcher),
                        msg,
                    )));
                    continue;
                }
                if msg.meta.msg.as_deref() == Some(MSG_USAGE_QUERY) {
                    std::mem::drop(tokio::spawn(answer_usage_query(
                        storage.clone(),
                        Arc::clone(&dispatcher),
                        msg,
                    )));
                    continue;
                }
                if msg.meta.msg.as_deref() == Some(MSG_STREAM_BATCH) {
                    // To the ingest task. Without a database, or with its queue full, the batch is
                    // not acked and its stream sends it again.
                    match ingest_tx.as_ref() {
                        Some(tx) => {
                            if tx.try_send(msg).is_err() {
                                tracing::warn!("sy.storage stream ingest queue full; a batch waits for its stream to resend it");
                            }
                        }
                        None => tracing::debug!("sy.storage has no database; a stream batch waits"),
                    }
                    continue;
                }
                let sender = dispatcher.sender_snapshot();
                if let Err(err) = process_router_message(
                    &sender,
                    &msg,
                    &mut control_state,
                    &node_name,
                ).await {
                    tracing::warn!(error = %err, action = ?msg.meta.msg, "failed to process sy.storage system message");
                }
            }
        }
    }
}

/// A client whose connection, once gone, ends SY.storage. The client does not reconnect: once its
/// connection is gone every write fails, and until 0.1.74 SY.storage only logged it and kept
/// running without a database. It exits instead, and systemd starts it again (Restart=always) with
/// a new connection (A-70).
async fn connect_or_exit_on_close(database_config: &PgConfig) -> Result<Client, StorageError> {
    let (client, connection) = database_config.connect(NoTls).await?;
    tokio::spawn(async move {
        match connection.await {
            Ok(()) => tracing::error!(
                "postgres connection closed; exiting so that systemd restarts sy-storage"
            ),
            Err(err) => tracing::error!(
                error = %err,
                "postgres connection closed; exiting so that systemd restarts sy-storage"
            ),
        }
        std::process::exit(1);
    });
    Ok(client)
}

impl Storage {
    async fn connect(database_config: &PgConfig) -> Result<Self, StorageError> {
        let client = connect_or_exit_on_close(database_config).await?;
        let ingest = connect_or_exit_on_close(database_config).await?;
        Ok(Self {
            client,
            ingest: tokio::sync::Mutex::new(ingest),
            database_config: database_config.clone(),
            usage_reader: tokio::sync::Mutex::new(None),
            usage_counts: std::sync::Mutex::new(UsageCounts::default()),
        })
    }

    fn usage_counts(&self) -> std::sync::MutexGuard<'_, UsageCounts> {
        self.usage_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `USAGE_QUERY`'s connection, its statements cut at 15 s. Unlike the others, its end does not
    /// end SY.storage: the next question opens another.
    async fn connect_usage_reader(&self) -> Result<Client, StorageError> {
        let (client, connection) = self.database_config.connect(NoTls).await?;
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::warn!(error = %err, "sy.storage usage query connection closed");
            }
        });
        client
            .batch_execute("SET statement_timeout = '15s'")
            .await?;
        Ok(client)
    }

    /// Answers a checked `USAGE_QUERY` on its own connection (§7.5); a question that finds another
    /// running is told so.
    async fn query_usage(&self, plan: &UsageQueryPlan) -> Result<UsageQueryResponse, String> {
        let Ok(mut reader) = self.usage_reader.try_lock() else {
            return Err("sy.storage is answering another usage query".to_string());
        };
        let client = match reader.take() {
            Some(client) if !client.is_closed() => client,
            _ => self
                .connect_usage_reader()
                .await
                .map_err(|err| err.to_string())?,
        };
        let answer = usage_query_page(&client, plan).await;
        // A connection whose question failed is dropped: the next question opens another.
        if answer.is_ok() {
            *reader = Some(client);
        }
        answer.map_err(|err| err.to_string())
    }

    async fn ensure_schema(&self) -> Result<(), StorageError> {
        self.client.batch_execute(TURNS_SCHEMA_SQL).await?;
        self.client.batch_execute(INBOX_SCHEMA_SQL).await?;
        self.client.batch_execute(COGNITION_SCHEMA_SQL).await?;
        self.client.batch_execute(USAGE_SCHEMA_SQL).await?;
        Ok(())
    }

    /// Retries the inbox rows not processed yet, oldest first: at startup, and every
    /// `INBOX_MAINTENANCE_INTERVAL_SECS`. A row with `INBOX_MAX_ATTEMPTS` stays as it is, and one
    /// younger than `min_age_secs` is left to its live handler.
    async fn replay_pending_messages(
        &self,
        batch_size: i64,
        max_rounds: u32,
        min_age_secs: i32,
    ) -> Result<u64, StorageError> {
        let mut replayed = 0u64;
        // One pass walks the pending rows in (received_at, dedupe_key) order, so the rows that
        // fail again do not hold back the ones after them.
        let mut after: Option<(std::time::SystemTime, String)> = None;
        for _ in 0..max_rounds {
            let after_ts = after.as_ref().map(|(ts, _)| *ts);
            let after_key = after.as_ref().map(|(_, key)| key.clone());
            let rows = self
                .client
                .query(
                    r#"
SELECT dedupe_key, subject, payload, received_at, source_ts, attempts
FROM storage_inbox
WHERE processed_at IS NULL
  AND attempts < $2
  AND received_at < now() - make_interval(secs => $3)
  AND ($4::timestamptz IS NULL OR (received_at, dedupe_key) > ($4::timestamptz, $5::text))
ORDER BY received_at ASC, dedupe_key ASC
LIMIT $1
"#,
                    &[
                        &batch_size,
                        &INBOX_MAX_ATTEMPTS,
                        &f64::from(min_age_secs),
                        &after_ts,
                        &after_key,
                    ],
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            let rows_len = rows.len();
            for row in rows {
                let dedupe_key: String = row.get(0);
                let subject: String = row.get(1);
                let payload: Vec<u8> = row.get(2);
                after = Some((row.get(3), dedupe_key.clone()));
                let source_ts: Option<std::time::SystemTime> = row.get(4);
                // This retry is one more attempt: a failure counts it below.
                let attempt = row.get::<_, i32>(5) + 1;
                let Some(kind) = handler_kind_for_subject(&subject) else {
                    let err_msg = format!("unsupported subject in inbox replay: {subject}");
                    self.client
                        .execute(
                            r#"
UPDATE storage_inbox
SET attempts = attempts + 1, last_error = $2
WHERE dedupe_key = $1
"#,
                            &[&dedupe_key, &err_msg],
                        )
                        .await?;
                    continue;
                };

                if let Err(err) = self
                    .process_inbox_message(
                        &dedupe_key,
                        &subject,
                        kind,
                        &payload,
                        source_ts,
                        attempt,
                    )
                    .await
                {
                    // A retry counts as an attempt, so a row that keeps failing stops at
                    // INBOX_MAX_ATTEMPTS instead of being retried forever.
                    self.client
                        .execute(
                            "UPDATE storage_inbox SET attempts = attempts + 1 WHERE dedupe_key = $1",
                            &[&dedupe_key],
                        )
                        .await?;
                    tracing::warn!(
                        subject = %subject,
                        dedupe_key = %dedupe_key,
                        error = %err,
                        "failed processing pending inbox message"
                    );
                } else {
                    replayed += 1;
                }
            }
            if rows_len < batch_size as usize {
                break;
            }
        }
        Ok(replayed)
    }

    /// A stream batch's records into the inbox, in one transaction on the ingest connection.
    /// Returns, per row, its attempts with this delivery, or `None` when it was processed already:
    /// a repeat of a processed row is absorbed.
    async fn upsert_stream_rows(
        &self,
        rows: &[StreamRow],
    ) -> Result<Vec<Option<i32>>, StorageError> {
        let mut client = self.ingest.lock().await;
        let tx = client.transaction().await?;
        let stmt = tx
            .prepare(
                r#"
INSERT INTO storage_inbox (dedupe_key, subject, payload, attempts, last_error, source_ts)
VALUES ($1, $2, $3, 1, NULL, $4)
ON CONFLICT (dedupe_key) DO UPDATE SET
    attempts = storage_inbox.attempts + 1,
    last_error = NULL
RETURNING processed_at IS NOT NULL, attempts
"#,
            )
            .await?;
        let mut attempts = Vec::with_capacity(rows.len());
        for row in rows {
            let upserted = tx
                .query_one(
                    &stmt,
                    &[&row.dedupe_key, &row.subject, &row.payload, &row.source_ts],
                )
                .await?;
            attempts.push((!upserted.get::<_, bool>(0)).then(|| upserted.get::<_, i32>(1)));
        }
        tx.commit().await?;
        Ok(attempts)
    }

    /// One communication's state for its cognition's rebuild (§6.2): every row it wrote, as the
    /// envelopes it wrote them, and the scopes its scope instances name, in `COGNITION_STATE_ORDER`
    /// and then by id.
    async fn cognition_state(
        &self,
        thread_id: &str,
        agent_ilk: &str,
    ) -> Result<Vec<(CognitionDurableEntity, String, Value)>, StorageError> {
        let mut state = Vec::new();
        for (entity, table, id_column) in [
            (
                CognitionDurableEntity::Thread,
                "cognition_threads",
                "communication_id",
            ),
            (
                CognitionDurableEntity::Context,
                "cognition_contexts",
                "context_id",
            ),
            (
                CognitionDurableEntity::Reason,
                "cognition_reasons",
                "reason_id",
            ),
            (
                CognitionDurableEntity::Cooccurrence,
                "cognition_cooccurrences",
                "cooccurrence_id",
            ),
            (
                CognitionDurableEntity::ScopeInstance,
                "cognition_scope_instances",
                "scope_instance_id",
            ),
            (
                CognitionDurableEntity::Memory,
                "cognition_memories",
                "memory_id",
            ),
            (
                CognitionDurableEntity::Episode,
                "cognition_episodes",
                "episode_id",
            ),
        ] {
            let sql = format!(
                "SELECT {id_column} AS entity_id, entity_version, writer, source_ts, source_turn, payload \
                 FROM {table} WHERE thread_id = $1 AND agent_ilk = $2 ORDER BY {id_column}"
            );
            for row in self
                .client
                .query(sql.as_str(), &[&thread_id, &agent_ilk])
                .await?
            {
                let entity_id: String = row.get("entity_id");
                let envelope = serde_json::json!({
                    "schema_version": COGNITION_DURABLE_SCHEMA_VERSION,
                    "entity_version": row.get::<_, i32>("entity_version"),
                    "entity": entity,
                    "op": "upsert",
                    "entity_id": entity_id,
                    "thread_id": thread_id,
                    "agent_ilk": agent_ilk,
                    "turn": row.get::<_, i64>("source_turn").max(0),
                    "hive": "",
                    "writer": row.get::<_, String>("writer"),
                    "ts": row.get::<_, String>("source_ts"),
                    "data": row.get::<_, Value>("payload"),
                });
                state.push((entity, entity_id, envelope));
            }
        }
        for row in self
            .client
            .query(
                "SELECT scope_id, entity_version, writer, source_ts, payload FROM cognition_scopes \
                 WHERE scope_id IN (SELECT scope_id FROM cognition_scope_instances \
                                    WHERE thread_id = $1 AND agent_ilk = $2) \
                 ORDER BY scope_id",
                &[&thread_id, &agent_ilk],
            )
            .await?
        {
            let scope_id: String = row.get("scope_id");
            let envelope = serde_json::json!({
                "schema_version": COGNITION_DURABLE_SCHEMA_VERSION,
                "entity_version": row.get::<_, i32>("entity_version"),
                "entity": CognitionDurableEntity::Scope,
                "op": "upsert",
                "entity_id": scope_id,
                "hive": "",
                "writer": row.get::<_, String>("writer"),
                "ts": row.get::<_, String>("source_ts"),
                "data": row.get::<_, Value>("payload"),
            });
            state.push((CognitionDurableEntity::Scope, scope_id, envelope));
        }
        state.sort_by(|a, b| (entity_rank(a.0), &a.1).cmp(&(entity_rank(b.0), &b.1)));
        Ok(state)
    }

    /// Deletes processed rows older than `retention_hours`, at most `INBOX_PURGE_BATCH` per call.
    async fn purge_processed_inbox(&self, retention_hours: i32) -> Result<u64, StorageError> {
        let deleted = self
            .client
            .execute(
                r#"
DELETE FROM storage_inbox
WHERE dedupe_key IN (
    SELECT dedupe_key FROM storage_inbox
    WHERE processed_at IS NOT NULL
      AND processed_at < now() - make_interval(hours => $1)
    LIMIT $2
)
"#,
                &[&retention_hours, &INBOX_PURGE_BATCH],
            )
            .await?;
        Ok(deleted)
    }

    async fn inbox_metrics(&self) -> Result<InboxMetrics, StorageError> {
        let row = self
            .client
            .query_one(
                r#"
SELECT
    COUNT(*) AS pending,
    COUNT(*) FILTER (WHERE last_error IS NOT NULL) AS pending_with_error,
    COALESCE(EXTRACT(EPOCH FROM (now() - MIN(received_at)))::BIGINT, 0) AS oldest_pending_age_s
FROM storage_inbox
WHERE processed_at IS NULL
"#,
                &[],
            )
            .await?;
        Ok(InboxMetrics {
            pending: row.get::<_, i64>(0),
            pending_with_error: row.get::<_, i64>(1),
            oldest_pending_age_s: row.get::<_, i64>(2),
        })
    }

    async fn inbox_metrics_snapshot(&self) -> Result<StorageInboxMetrics, StorageError> {
        let row = self
            .client
            .query_one(
                r#"
SELECT
    COUNT(*) FILTER (WHERE processed_at IS NULL) AS pending,
    COUNT(*) FILTER (WHERE processed_at IS NULL AND last_error IS NOT NULL) AS pending_with_error,
    COALESCE(EXTRACT(EPOCH FROM (now() - MIN(received_at) FILTER (WHERE processed_at IS NULL)))::BIGINT, 0) AS oldest_pending_age_s,
    COUNT(*) FILTER (WHERE processed_at IS NOT NULL) AS processed_total,
    COALESCE(MAX(attempts), 0) AS max_attempts
FROM storage_inbox
"#,
                &[],
            )
            .await?;
        Ok(StorageInboxMetrics {
            pending: row.get::<_, i64>(0),
            pending_with_error: row.get::<_, i64>(1),
            oldest_pending_age_s: row.get::<_, i64>(2),
            processed_total: row.get::<_, i64>(3),
            max_attempts: row.get::<_, i32>(4),
        })
    }

    /// `attempt` counts this one. A row that fails its `INBOX_MAX_ATTEMPTS`th is never retried, so
    /// on that one a handler that waits on another service stops waiting: a usage record whose
    /// tenant identity cannot give is stored unverified (§7.5).
    async fn process_inbox_message(
        &self,
        dedupe_key: &str,
        subject: &str,
        kind: HandlerKind,
        payload: &[u8],
        source_ts: Option<std::time::SystemTime>,
        attempt: i32,
    ) -> Result<(), StorageError> {
        let last_attempt = attempt >= INBOX_MAX_ATTEMPTS;
        let result = self
            .process_subject_payload(kind, payload, dedupe_key, source_ts, last_attempt)
            .await;
        match result {
            Ok(()) => {
                self.client
                    .execute(
                        r#"
UPDATE storage_inbox
SET processed_at = now(), last_error = NULL
WHERE dedupe_key = $1
"#,
                        &[&dedupe_key],
                    )
                    .await?;
                Ok(())
            }
            Err(err) => {
                let err_text = truncate_error(&err.to_string(), INBOX_ERROR_MAX_LEN);
                self.client
                    .execute(
                        r#"
UPDATE storage_inbox
SET last_error = $2
WHERE dedupe_key = $1
"#,
                        &[&dedupe_key, &err_text],
                    )
                    .await?;
                tracing::warn!(
                    subject = %subject,
                    dedupe_key = %dedupe_key,
                    error = %err,
                    "storage message processing failed"
                );
                Err(err)
            }
        }
    }

    async fn process_subject_payload(
        &self,
        kind: HandlerKind,
        payload: &[u8],
        dedupe_key: &str,
        source_ts: Option<std::time::SystemTime>,
        last_attempt: bool,
    ) -> Result<(), StorageError> {
        match kind {
            HandlerKind::Turns => {
                let value: Value = serde_json::from_slice(payload)?;
                let turn = parse_turn(value)?;
                self.persist_turn(&self.client, dedupe_key, &turn, source_ts)
                    .await
            }
            HandlerKind::CognitionThreads => {
                let envelope: CognitionDurableEnvelope<CognitionThreadData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Thread)?;
                self.persist_cognition_thread(&self.client, &envelope).await
            }
            HandlerKind::CognitionContexts => {
                let envelope: CognitionDurableEnvelope<CognitionContextData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Context)?;
                self.persist_cognition_context(&self.client, &envelope)
                    .await
            }
            HandlerKind::CognitionReasons => {
                let envelope: CognitionDurableEnvelope<CognitionReasonData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Reason)?;
                self.persist_cognition_reason(&self.client, &envelope).await
            }
            HandlerKind::CognitionCooccurrences => {
                let envelope: CognitionDurableEnvelope<CognitionCooccurrenceData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Cooccurrence)?;
                self.persist_cognition_cooccurrence(&self.client, &envelope)
                    .await
            }
            HandlerKind::CognitionScopes => {
                let envelope: CognitionDurableEnvelope<CognitionScopeData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Scope)?;
                self.persist_cognition_scope(&self.client, &envelope).await
            }
            HandlerKind::CognitionScopeInstances => {
                let envelope: CognitionDurableEnvelope<CognitionScopeInstanceData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::ScopeInstance)?;
                self.persist_cognition_scope_instance(&self.client, &envelope)
                    .await
            }
            HandlerKind::CognitionMemories => {
                let envelope: CognitionDurableEnvelope<CognitionMemoryData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Memory)?;
                self.persist_cognition_memory(&self.client, &envelope).await
            }
            HandlerKind::CognitionEpisodes => {
                let envelope: CognitionDurableEnvelope<CognitionEpisodeData> =
                    parse_cognition_envelope(payload, CognitionDurableEntity::Episode)?;
                self.persist_cognition_episode(&self.client, &envelope)
                    .await
            }
            HandlerKind::Usage => {
                let record: UsageRecord = serde_json::from_slice(payload)?;
                self.persist_usage(&self.client, &record, last_attempt)
                    .await
            }
        }
    }

    /// One row per turn, keyed by its inbox key: a redelivered turn is the same payload and lands
    /// on the same key, while a reply, which shares its request's `trace_id`, is a row of its own.
    async fn persist_turn<C>(
        &self,
        db: &C,
        dedupe_key: &str,
        turn: &TurnRecord,
        source_ts: Option<std::time::SystemTime>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        // `ts` is when the router copied the message (the stream record's time), not when it got
        // here: a turn that waited in a stream keeps its time.
        db.execute(
            r#"
INSERT INTO turns (dedupe_key, ctx, seq, ts, from_ilk, to_ilk, ich, msg_type, content, tags,
                   thread_id, trace_id, message)
VALUES ($1, $2, $3, COALESCE($10, now()), $4, $5, $6, $7, $8, $9, $11, $12, $13)
ON CONFLICT (dedupe_key) DO NOTHING
"#,
            &[
                &dedupe_key,
                &turn.ctx,
                &turn.seq,
                &turn.from_ilk,
                &turn.to_ilk,
                &turn.ich,
                &turn.msg_type,
                &turn.content,
                &turn.tags,
                &source_ts,
                &turn.thread_id,
                &turn.trace_id,
                &turn.message,
            ],
        )
        .await?;
        Ok(())
    }

    /// A communication's thread row, keyed by the communication (`entity_id`).
    async fn persist_cognition_thread<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionThreadData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_threads WHERE communication_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        db.execute(
            r#"
INSERT INTO cognition_threads (
    communication_id, thread_id, agent_ilk, source_turn, entity_version, writer, source_ts,
    src_ilk, dst_ilk, ich, status, first_seen_at, last_seen_at, turn_count, payload
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
ON CONFLICT (communication_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    src_ilk = EXCLUDED.src_ilk,
    dst_ilk = EXCLUDED.dst_ilk,
    ich = EXCLUDED.ich,
    status = EXCLUDED.status,
    first_seen_at = EXCLUDED.first_seen_at,
    last_seen_at = EXCLUDED.last_seen_at,
    turn_count = EXCLUDED.turn_count,
    payload = EXCLUDED.payload,
    updated_at = now()
WHERE cognition_threads.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &agent_ilk,
                &source_turn,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.src_ilk,
                &envelope.data.dst_ilk,
                &envelope.data.ich,
                &envelope.data.status,
                &envelope.data.first_seen_at,
                &envelope.data.last_seen_at,
                &opt_i64(envelope.data.turn_count),
                &payload,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_context<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionContextData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_contexts WHERE context_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        let ilk_weights = json_value(&envelope.data.ilk_weights)?;
        let ilk_profile = json_value(&envelope.data.ilk_profile)?;
        db.execute(
            r#"
INSERT INTO cognition_contexts (
    context_id, thread_id, scope_id, entity_version, writer, source_ts,
    label, status, score, weight, weight_avg_cumulative, weight_avg_ema,
    weight_samples, tags, ilk_weights, ilk_profile, opened_at, last_seen_at, closed_at, payload, agent_ilk, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)
ON CONFLICT (context_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    scope_id = EXCLUDED.scope_id,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    label = EXCLUDED.label,
    status = EXCLUDED.status,
    score = EXCLUDED.score,
    weight = EXCLUDED.weight,
    weight_avg_cumulative = EXCLUDED.weight_avg_cumulative,
    weight_avg_ema = EXCLUDED.weight_avg_ema,
    weight_samples = EXCLUDED.weight_samples,
    tags = EXCLUDED.tags,
    ilk_weights = EXCLUDED.ilk_weights,
    ilk_profile = EXCLUDED.ilk_profile,
    opened_at = EXCLUDED.opened_at,
    last_seen_at = EXCLUDED.last_seen_at,
    closed_at = EXCLUDED.closed_at,
    payload = EXCLUDED.payload,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_contexts.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &envelope.data.scope_id,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.label,
                &normalized_status(&envelope.op, &envelope.data.status),
                &envelope.data.score,
                &envelope.data.weight,
                &envelope.data.weight_avg_cumulative,
                &envelope.data.weight_avg_ema,
                &opt_i64(envelope.data.weight_samples),
                &envelope.data.tags,
                &ilk_weights,
                &ilk_profile,
                &envelope.data.opened_at,
                &envelope.data.last_seen_at,
                &envelope.data.closed_at,
                &payload,
                &agent_ilk,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_reason<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionReasonData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_reasons WHERE reason_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        let ilk_weights = json_value(&envelope.data.ilk_weights)?;
        let ilk_profile = json_value(&envelope.data.ilk_profile)?;
        db.execute(
            r#"
INSERT INTO cognition_reasons (
    reason_id, thread_id, scope_id, entity_version, writer, source_ts,
    label, status, score, weight, weight_avg_cumulative, weight_avg_ema,
    weight_samples, signals_canonical, signals_extra, ilk_weights, ilk_profile,
    opened_at, last_seen_at, closed_at, payload, agent_ilk, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)
ON CONFLICT (reason_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    scope_id = EXCLUDED.scope_id,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    label = EXCLUDED.label,
    status = EXCLUDED.status,
    score = EXCLUDED.score,
    weight = EXCLUDED.weight,
    weight_avg_cumulative = EXCLUDED.weight_avg_cumulative,
    weight_avg_ema = EXCLUDED.weight_avg_ema,
    weight_samples = EXCLUDED.weight_samples,
    signals_canonical = EXCLUDED.signals_canonical,
    signals_extra = EXCLUDED.signals_extra,
    ilk_weights = EXCLUDED.ilk_weights,
    ilk_profile = EXCLUDED.ilk_profile,
    opened_at = EXCLUDED.opened_at,
    last_seen_at = EXCLUDED.last_seen_at,
    closed_at = EXCLUDED.closed_at,
    payload = EXCLUDED.payload,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_reasons.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &envelope.data.scope_id,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.label,
                &normalized_status(&envelope.op, &envelope.data.status),
                &envelope.data.score,
                &envelope.data.weight,
                &envelope.data.weight_avg_cumulative,
                &envelope.data.weight_avg_ema,
                &opt_i64(envelope.data.weight_samples),
                &envelope.data.signals_canonical,
                &envelope.data.signals_extra,
                &ilk_weights,
                &ilk_profile,
                &envelope.data.opened_at,
                &envelope.data.last_seen_at,
                &envelope.data.closed_at,
                &payload,
                &agent_ilk,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_cooccurrence<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionCooccurrenceData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_cooccurrences WHERE cooccurrence_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        db.execute(
            r#"
INSERT INTO cognition_cooccurrences (
    cooccurrence_id, thread_id, context_id, context_label, reason_id, reason_label,
    entity_version, writer, source_ts, status, score, weight,
    weight_avg_cumulative, weight_avg_ema, weight_samples, occurrences,
    opened_at, last_seen_at, closed_at, payload, agent_ilk, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22)
ON CONFLICT (cooccurrence_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    context_id = EXCLUDED.context_id,
    context_label = EXCLUDED.context_label,
    reason_id = EXCLUDED.reason_id,
    reason_label = EXCLUDED.reason_label,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    status = EXCLUDED.status,
    score = EXCLUDED.score,
    weight = EXCLUDED.weight,
    weight_avg_cumulative = EXCLUDED.weight_avg_cumulative,
    weight_avg_ema = EXCLUDED.weight_avg_ema,
    weight_samples = EXCLUDED.weight_samples,
    occurrences = EXCLUDED.occurrences,
    opened_at = EXCLUDED.opened_at,
    last_seen_at = EXCLUDED.last_seen_at,
    closed_at = EXCLUDED.closed_at,
    payload = EXCLUDED.payload,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_cooccurrences.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &envelope.data.context_id,
                &envelope.data.context_label,
                &envelope.data.reason_id,
                &envelope.data.reason_label,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &normalized_status(&envelope.op, &envelope.data.status),
                &envelope.data.score,
                &envelope.data.weight,
                &envelope.data.weight_avg_cumulative,
                &envelope.data.weight_avg_ema,
                &opt_i64(envelope.data.weight_samples),
                &opt_i64(envelope.data.occurrences),
                &envelope.data.opened_at,
                &envelope.data.last_seen_at,
                &envelope.data.closed_at,
                &payload,
                &agent_ilk,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_scope<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionScopeData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_scopes WHERE scope_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        // A scope has no communication of its own, but its envelopes carry the turn of the one
        // that wrote them: a late retry cannot reopen a closed scope.
        let source_turn = i64::try_from(envelope.turn.unwrap_or(0)).unwrap_or(i64::MAX);
        let payload = json_value(&envelope.data)?;
        db.execute(
            r#"
INSERT INTO cognition_scopes (
    scope_id, entity_version, writer, source_ts, label, status,
    dominant_context_id, dominant_reason_id, binding_energy_ema,
    opened_at, last_seen_at, closed_at, payload, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
ON CONFLICT (scope_id) DO UPDATE SET
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    label = EXCLUDED.label,
    status = EXCLUDED.status,
    dominant_context_id = EXCLUDED.dominant_context_id,
    dominant_reason_id = EXCLUDED.dominant_reason_id,
    binding_energy_ema = EXCLUDED.binding_energy_ema,
    opened_at = EXCLUDED.opened_at,
    last_seen_at = EXCLUDED.last_seen_at,
    closed_at = EXCLUDED.closed_at,
    payload = EXCLUDED.payload,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_scopes.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.label,
                &normalized_status(&envelope.op, &envelope.data.status),
                &envelope.data.dominant_context_id,
                &envelope.data.dominant_reason_id,
                &envelope.data.binding_energy_ema,
                &envelope.data.opened_at,
                &envelope.data.last_seen_at,
                &envelope.data.closed_at,
                &payload,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_scope_instance<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionScopeInstanceData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_scope_instances WHERE scope_instance_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        db.execute(
            r#"
INSERT INTO cognition_scope_instances (
    scope_instance_id, thread_id, scope_id, entity_version, writer, source_ts,
    dominant_context_id, dominant_reason_id, start_turn, end_turn,
    opened_at, closed_at, payload, agent_ilk, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
ON CONFLICT (scope_instance_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    scope_id = EXCLUDED.scope_id,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    dominant_context_id = EXCLUDED.dominant_context_id,
    dominant_reason_id = EXCLUDED.dominant_reason_id,
    start_turn = EXCLUDED.start_turn,
    end_turn = EXCLUDED.end_turn,
    opened_at = EXCLUDED.opened_at,
    closed_at = EXCLUDED.closed_at,
    payload = EXCLUDED.payload,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_scope_instances.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &envelope.data.scope_id,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.dominant_context_id,
                &envelope.data.dominant_reason_id,
                &opt_i64(envelope.data.start_turn),
                &opt_i64(envelope.data.end_turn),
                &envelope.data.opened_at,
                &envelope.data.closed_at,
                &payload,
                &agent_ilk,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_memory<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionMemoryData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_memories WHERE memory_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        let ilk_weights = json_value(&envelope.data.ilk_weights)?;
        db.execute(
            r#"
INSERT INTO cognition_memories (
    memory_id, thread_id, scope_id, entity_version, writer, source_ts,
    summary, weight, occurrences, dominant_context_id, dominant_reason_id,
    ilk_weights, created_at, last_seen_at, payload, agent_ilk, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
ON CONFLICT (memory_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    scope_id = EXCLUDED.scope_id,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    summary = EXCLUDED.summary,
    weight = EXCLUDED.weight,
    occurrences = EXCLUDED.occurrences,
    dominant_context_id = EXCLUDED.dominant_context_id,
    dominant_reason_id = EXCLUDED.dominant_reason_id,
    ilk_weights = EXCLUDED.ilk_weights,
    created_at = EXCLUDED.created_at,
    last_seen_at = EXCLUDED.last_seen_at,
    payload = EXCLUDED.payload,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_memories.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &envelope.data.scope_id,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.summary,
                &envelope.data.weight,
                &opt_i64(envelope.data.occurrences),
                &envelope.data.dominant_context_id,
                &envelope.data.dominant_reason_id,
                &ilk_weights,
                &envelope.data.created_at,
                &envelope.data.last_seen_at,
                &payload,
                &agent_ilk,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_cognition_episode<C>(
        &self,
        db: &C,
        envelope: &CognitionDurableEnvelope<CognitionEpisodeData>,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        if matches!(envelope.op, CognitionDurableOp::Delete) {
            db.execute(
                "DELETE FROM cognition_episodes WHERE episode_id = $1",
                &[&envelope.entity_id],
            )
            .await?;
            return Ok(());
        }
        let (agent_ilk, source_turn) = envelope_communication(envelope)?;
        let payload = json_value(&envelope.data)?;
        db.execute(
            r#"
INSERT INTO cognition_episodes (
    episode_id, thread_id, scope_id, entity_version, writer, source_ts,
    affect_id, title, summary, base_intensity, evidence_strength,
    evidence_context_ids, evidence_reason_ids, evidence_signals,
    intensity, reason, created_at, payload, agent_ilk, source_turn
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)
ON CONFLICT (episode_id) DO UPDATE SET
    thread_id = EXCLUDED.thread_id,
    scope_id = EXCLUDED.scope_id,
    entity_version = EXCLUDED.entity_version,
    writer = EXCLUDED.writer,
    source_ts = EXCLUDED.source_ts,
    affect_id = EXCLUDED.affect_id,
    title = EXCLUDED.title,
    summary = EXCLUDED.summary,
    base_intensity = EXCLUDED.base_intensity,
    evidence_strength = EXCLUDED.evidence_strength,
    evidence_context_ids = EXCLUDED.evidence_context_ids,
    evidence_reason_ids = EXCLUDED.evidence_reason_ids,
    evidence_signals = EXCLUDED.evidence_signals,
    intensity = EXCLUDED.intensity,
    reason = EXCLUDED.reason,
    created_at = EXCLUDED.created_at,
    payload = EXCLUDED.payload,
    agent_ilk = EXCLUDED.agent_ilk,
    source_turn = EXCLUDED.source_turn,
    updated_at = now()
WHERE cognition_episodes.source_turn <= EXCLUDED.source_turn
"#,
            &[
                &envelope.entity_id,
                envelope
                    .thread_id
                    .as_ref()
                    .ok_or_else(|| missing_field("thread_id"))?,
                &envelope.data.scope_id,
                &(envelope.entity_version as i32),
                &envelope.writer,
                &envelope.ts,
                &envelope.data.affect_id,
                &envelope.data.title,
                &envelope.data.summary,
                &envelope.data.base_intensity,
                &envelope.data.evidence_strength,
                &envelope.data.evidence_context_ids,
                &envelope.data.evidence_reason_ids,
                &envelope.data.evidence_signals,
                &envelope.data.intensity,
                &envelope.data.reason,
                &envelope.data.created_at,
                &payload,
                &agent_ilk,
                &source_turn,
            ],
        )
        .await?;
        Ok(())
    }

    /// A usage record into `llm_usage`, charged as `decide_usage_tenant` decides (§7.5). A record
    /// lands once: a repeat of its `record_id`, or of its provider's response id, is absorbed, and
    /// counted only the time it landed.
    async fn persist_usage<C>(
        &self,
        db: &C,
        record: &UsageRecord,
        last_attempt: bool,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        let charge = decide_usage_tenant(record, last_attempt, identity_tenant)?;
        let ts: std::time::SystemTime = chrono::DateTime::parse_from_rfc3339(&record.ts)?.into();
        let work = record
            .work
            .as_ref()
            .filter(|work| !work.is_empty())
            .map(json_value)
            .transpose()?;
        let inserted = db
            .execute(
                r#"
INSERT INTO llm_usage (
    record_id, provider_response_id, ts, hive, node_name, node_ilk, tenant_id, tenant_source,
    claimed_tenant_id, verified, counterpart_ilk, provider, model, served_model, service_tier,
    kind, outcome, uncached_input, cache_read, cache_write, output, reasoning, raw_usage,
    trace_id, thread_id, work
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26)
ON CONFLICT DO NOTHING
"#,
                &[
                    &record.record_id,
                    &record.provider_response_id,
                    &ts,
                    &record.hive,
                    &record.node_name,
                    &record.node_ilk,
                    &charge.tenant_id,
                    &record.tenant_source.as_str(),
                    &record.tenant_id,
                    &charge.verified,
                    &record.counterpart_ilk,
                    &record.provider,
                    &record.model,
                    &record.served_model,
                    &record.service_tier,
                    &record.kind.as_str(),
                    &record.outcome.as_str(),
                    &opt_i64(record.uncached_input),
                    &opt_i64(record.cache_read),
                    &opt_i64(record.cache_write),
                    &opt_i64(record.output),
                    &opt_i64(record.reasoning),
                    &record.raw_usage,
                    &record.trace_id,
                    &record.thread_id,
                    &work,
                ],
            )
            .await?;
        if inserted > 0 {
            let mut counts = self.usage_counts();
            if !charge.verified {
                counts.unverified += 1;
            } else if charge.tenant_id != record.tenant_id {
                counts.mismatched += 1;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum HandlerKind {
    Turns,
    CognitionThreads,
    CognitionContexts,
    CognitionReasons,
    CognitionCooccurrences,
    CognitionScopes,
    CognitionScopeInstances,
    CognitionMemories,
    CognitionEpisodes,
    Usage,
}

/// What the checks of the usage records found since the last report (§7.5): the records refused,
/// by reason; those charged to another tenant than they claimed; those stored unverified.
#[derive(Debug, Default)]
struct UsageCounts {
    refused: BTreeMap<&'static str, u64>,
    mismatched: u64,
    unverified: u64,
}

struct InboxMetrics {
    pending: i64,
    pending_with_error: i64,
    oldest_pending_age_s: i64,
}

fn handler_kind_for_subject(subject: &str) -> Option<HandlerKind> {
    match subject {
        SUBJECT_STORAGE_TURNS => Some(HandlerKind::Turns),
        SUBJECT_STORAGE_COGNITION_THREADS => Some(HandlerKind::CognitionThreads),
        SUBJECT_STORAGE_COGNITION_CONTEXTS => Some(HandlerKind::CognitionContexts),
        SUBJECT_STORAGE_COGNITION_REASONS => Some(HandlerKind::CognitionReasons),
        SUBJECT_STORAGE_COGNITION_COOCCURRENCES => Some(HandlerKind::CognitionCooccurrences),
        SUBJECT_STORAGE_COGNITION_SCOPES => Some(HandlerKind::CognitionScopes),
        SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES => Some(HandlerKind::CognitionScopeInstances),
        SUBJECT_STORAGE_COGNITION_MEMORIES => Some(HandlerKind::CognitionMemories),
        SUBJECT_STORAGE_COGNITION_EPISODES => Some(HandlerKind::CognitionEpisodes),
        SUBJECT_STORAGE_USAGE => Some(HandlerKind::Usage),
        _ => None,
    }
}

fn message_dedupe_key(subject: &str, payload: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in subject.as_bytes().iter().chain(payload.iter()) {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{subject}:{hash:016x}:{}", payload.len())
}

/// Every `INBOX_MAINTENANCE_INTERVAL_SECS`, retries the rows that failed and purges the processed
/// ones past `STORAGE_INBOX_RETENTION_HOURS` (A-70). The startup replay already ran, so the first
/// round waits one interval.
async fn run_inbox_maintenance_loop(storage: Arc<Storage>) {
    let mut ticker = time::interval(Duration::from_secs(INBOX_MAINTENANCE_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        match storage
            .replay_pending_messages(
                INBOX_REPLAY_BATCH_SIZE,
                INBOX_REPLAY_MAX_ROUNDS,
                INBOX_RETRY_MIN_AGE_SECS,
            )
            .await
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "retried failed storage inbox messages"),
            Err(err) => tracing::warn!(error = %err, "storage inbox retry failed"),
        }
        match storage
            .purge_processed_inbox(STORAGE_INBOX_RETENTION_HOURS)
            .await
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(count, "purged processed storage inbox messages"),
            Err(err) => tracing::warn!(error = %err, "storage inbox purge failed"),
        }
    }
}

fn entity_rank(entity: CognitionDurableEntity) -> usize {
    COGNITION_STATE_ORDER
        .iter()
        .position(|candidate| *candidate == entity)
        .unwrap_or(COGNITION_STATE_ORDER.len())
}

/// One page of a communication's state: the envelopes after `after`, up to
/// `COGNITION_STATE_PAGE_BYTES` (always at least one), and where the next page starts.
fn page_cognition_state(
    state: Vec<(CognitionDurableEntity, String, Value)>,
    after: Option<&(String, String)>,
) -> CognitionStateResponse {
    let after = after.and_then(|(entity, id)| {
        let entity: CognitionDurableEntity =
            serde_json::from_value(Value::String(entity.clone())).ok()?;
        Some((entity_rank(entity), id.clone()))
    });
    let mut response = CognitionStateResponse {
        ok: true,
        ..CognitionStateResponse::default()
    };
    let mut bytes = 0usize;
    let mut last: Option<(CognitionDurableEntity, String)> = None;
    let mut remaining = false;
    for (entity, entity_id, envelope) in state {
        if after
            .as_ref()
            .is_some_and(|(rank, id)| (entity_rank(entity), &entity_id) <= (*rank, id))
        {
            continue;
        }
        let size = serde_json::to_vec(&envelope).map(|v| v.len()).unwrap_or(0);
        if !response.envelopes.is_empty() && bytes + size > COGNITION_STATE_PAGE_BYTES {
            remaining = true;
            break;
        }
        bytes += size;
        response.envelopes.push(envelope);
        last = Some((entity, entity_id));
    }
    if remaining {
        response.next = last.map(|(entity, id)| {
            let entity = serde_json::to_value(entity)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_default();
            (entity, id)
        });
    }
    response
}

/// `COGNITION_STATE_GET` (§6.2): the state of one communication, for its cognition's rebuild. Only
/// to an SY.cognition, as its router stamped it.
async fn answer_cognition_state(
    storage: Arc<Storage>,
    dispatcher: Arc<RouterDispatcher>,
    msg: Message,
) {
    let from = msg.routing.src_l2_name.as_deref().unwrap_or("");
    let is_cognition = from
        .get(..13)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("SY.cognition@"));
    let response = if !is_cognition {
        tracing::warn!(
            from,
            "sy.storage refused a COGNITION_STATE_GET not from SY.cognition"
        );
        CognitionStateResponse {
            error: Some("only SY.cognition may read a communication's state".to_string()),
            ..CognitionStateResponse::default()
        }
    } else {
        match serde_json::from_value::<CognitionStateGet>(msg.payload.clone()) {
            Err(err) => CognitionStateResponse {
                error: Some(format!("invalid COGNITION_STATE_GET: {err}")),
                ..CognitionStateResponse::default()
            },
            Ok(request) => match storage
                .cognition_state(&request.thread_id, &request.agent_ilk)
                .await
            {
                Ok(state) => page_cognition_state(state, request.after.as_ref()),
                Err(err) => CognitionStateResponse {
                    error: Some(err.to_string()),
                    ..CognitionStateResponse::default()
                },
            },
        }
    };
    let sender = dispatcher.sender_snapshot();
    let reply = build_system_reply(
        &sender,
        &msg,
        MSG_COGNITION_STATE_RESPONSE,
        serde_json::to_value(response).unwrap_or(Value::Null),
    );
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(error = %err, "sy.storage could not answer a COGNITION_STATE_GET");
    }
}

/// The inbox metrics over the mesh (A-70 stage 2b): SY.admin's storage metrics and
/// SY.orchestrator's boot wait for the database ask here. Read-only counts, so any node may ask;
/// each answer scans the inbox, so one runs at a time and a request that finds one running is
/// told so (its caller asks again).
async fn answer_storage_metrics(
    storage: Option<Arc<Storage>>,
    gate: Arc<tokio::sync::Semaphore>,
    dispatcher: Arc<RouterDispatcher>,
    msg: Message,
) {
    let permit = gate.try_acquire_owned();
    let response = match (storage, permit) {
        (_, Err(_)) => StorageMetricsResponse {
            ok: false,
            metrics: None,
            error: Some("sy.storage is answering another metrics request".to_string()),
        },
        (Some(storage), Ok(_permit)) => match storage.inbox_metrics_snapshot().await {
            Ok(metrics) => StorageMetricsResponse {
                ok: true,
                metrics: Some(metrics),
                error: None,
            },
            Err(err) => StorageMetricsResponse {
                ok: false,
                metrics: None,
                error: Some(err.to_string()),
            },
        },
        (None, Ok(_)) => StorageMetricsResponse {
            ok: false,
            metrics: None,
            error: Some("sy.storage has no database".to_string()),
        },
    };
    let sender = dispatcher.sender_snapshot();
    let reply = build_system_reply(
        &sender,
        &msg,
        MSG_STORAGE_METRICS_RESPONSE,
        serde_json::to_value(response).unwrap_or(Value::Null),
    );
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(error = %err, "sy.storage could not answer a STORAGE_METRICS_GET");
    }
}

/// `USAGE_QUERY` (§7.5): the usage records, for SY.admin@motherbee only, as its router stamped it;
/// the operator, Archi and Cloud ask through it.
async fn answer_usage_query(
    storage: Option<Arc<Storage>>,
    dispatcher: Arc<RouterDispatcher>,
    msg: Message,
) {
    let response = usage_query_answer(storage.as_deref(), &msg)
        .await
        .unwrap_or_else(|(code, error)| UsageQueryResponse {
            error: Some(error),
            error_code: Some(code.to_string()),
            ..UsageQueryResponse::default()
        });
    let sender = dispatcher.sender_snapshot();
    let reply = build_system_reply(
        &sender,
        &msg,
        MSG_USAGE_QUERY_RESPONSE,
        serde_json::to_value(response).unwrap_or(Value::Null),
    );
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(error = %err, "sy.storage could not answer a USAGE_QUERY");
    }
}

/// The answer to a `USAGE_QUERY`, or its error with the code `UsageQueryResponse` documents.
async fn usage_query_answer(
    storage: Option<&Storage>,
    msg: &Message,
) -> Result<UsageQueryResponse, (&'static str, String)> {
    let from = msg.routing.src_l2_name.as_deref().unwrap_or("");
    if from != format!("SY.admin@{PRIMARY_HIVE_ID}") {
        tracing::warn!(
            from,
            "sy.storage refused a USAGE_QUERY not from SY.admin@motherbee"
        );
        return Err((
            "FORBIDDEN",
            "only SY.admin@motherbee may query the usage records".to_string(),
        ));
    }
    let storage = storage.ok_or((
        "USAGE_UNAVAILABLE",
        "sy.storage has no database".to_string(),
    ))?;
    let query: UsageQuery = serde_json::from_value(msg.payload.clone())
        .map_err(|err| ("INVALID_REQUEST", format!("invalid USAGE_QUERY: {err}")))?;
    let plan = plan_usage_query(&query).map_err(|err| ("INVALID_REQUEST", err))?;
    storage
        .query_usage(&plan)
        .await
        .map_err(|err| ("USAGE_UNAVAILABLE", err))
}

/// A `USAGE_QUERY`, checked: what it asks, every value ready to bind.
#[derive(Debug, PartialEq)]
struct UsageQueryPlan {
    /// `None`: every tenant.
    tenant_id: Option<String>,
    mode: UsageQueryMode,
    limit: u32,
}

#[derive(Debug, PartialEq)]
enum UsageQueryMode {
    /// The records stored after a cursor, by `(stored_at, record_id)`: a reader's sync, which takes
    /// the rows that arrive late (the streams' window, the inbox's retries).
    Since {
        stored_at: std::time::SystemTime,
        record_id: String,
    },
    /// A period's records, by `(ts, record_id)`, after a cursor.
    Records {
        from: std::time::SystemTime,
        to: std::time::SystemTime,
        after: Option<(std::time::SystemTime, String)>,
    },
    /// A period's groups, by key, after a key.
    Groups {
        from: std::time::SystemTime,
        to: std::time::SystemTime,
        group_by: UsageGroupBy,
        after: Option<String>,
    },
}

/// Checks a `USAGE_QUERY` (§7.5): exactly one of `tenant_id` and `all_tenants`; then either `since`
/// alone, or a period `[from, to)` of at most `USAGE_QUERY_MAX_DAYS`, grouped by `group_by` or
/// record by record. A blank field counts as absent.
fn plan_usage_query(query: &UsageQuery) -> Result<UsageQueryPlan, String> {
    let given = |value: &Option<String>| {
        value
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    };
    let tenant_id = match (given(&query.tenant_id), query.all_tenants) {
        (Some(tenant_id), false) => Some(tenant_id),
        (None, true) => None,
        _ => {
            return Err("a usage query takes exactly one of tenant_id and all_tenants".to_string())
        }
    };
    let limit = query
        .limit
        .unwrap_or(USAGE_QUERY_DEFAULT_LIMIT)
        .clamp(1, USAGE_QUERY_MAX_LIMIT);
    let after = given(&query.after);
    if let Some(since) = given(&query.since) {
        if given(&query.from).is_some()
            || given(&query.to).is_some()
            || query.group_by.is_some()
            || after.is_some()
        {
            return Err("since takes no period, grouping or after".to_string());
        }
        let (stored_at, record_id) = parse_usage_cursor(&since)
            .ok_or("since is neither a usage cursor nor an RFC 3339 time")?;
        return Ok(UsageQueryPlan {
            tenant_id,
            mode: UsageQueryMode::Since {
                stored_at,
                record_id,
            },
            limit,
        });
    }
    let (Some(from), Some(to)) = (given(&query.from), given(&query.to)) else {
        return Err("a usage query takes since, or a period from and to".to_string());
    };
    let time = |name: &str, value: &str| {
        chrono::DateTime::parse_from_rfc3339(value)
            .map_err(|err| format!("{name} is not an RFC 3339 time: {err}"))
    };
    let (from, to) = (time("from", &from)?, time("to", &to)?);
    if to <= from {
        return Err("to must be after from".to_string());
    }
    if to - from > chrono::Duration::days(USAGE_QUERY_MAX_DAYS) {
        return Err(format!(
            "a usage query's period is at most {USAGE_QUERY_MAX_DAYS} days"
        ));
    }
    let (from, to) = (from.into(), to.into());
    let mode = match query.group_by {
        Some(group_by) => UsageQueryMode::Groups {
            from,
            to,
            group_by,
            after,
        },
        None => UsageQueryMode::Records {
            from,
            to,
            after: after
                .map(|after| parse_usage_cursor(&after).ok_or("after is not a usage cursor"))
                .transpose()?,
        },
    };
    Ok(UsageQueryPlan {
        tenant_id,
        mode,
        limit,
    })
}

/// A record's place in an order, as `next` gives it: `<RFC 3339 time>|<record_id>`, the time to
/// the microsecond, as stored.
fn usage_cursor(time: std::time::SystemTime, record_id: &str) -> String {
    format!("{}|{record_id}", rfc3339_micros(time))
}

/// A cursor `usage_cursor` made, or a bare RFC 3339 time (where a reader's first sync starts).
fn parse_usage_cursor(cursor: &str) -> Option<(std::time::SystemTime, String)> {
    let (time, record_id) = cursor.split_once('|').unwrap_or((cursor, ""));
    let time = chrono::DateTime::parse_from_rfc3339(time).ok()?;
    Some((time.into(), record_id.to_string()))
}

fn rfc3339_micros(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// The fixed SQL expression each grouping groups by: never the caller's text.
fn usage_group_key(group_by: UsageGroupBy) -> &'static str {
    match group_by {
        UsageGroupBy::Day => "to_char(date_trunc('day', ts AT TIME ZONE 'UTC'), 'YYYY-MM-DD')",
        UsageGroupBy::Model => "model",
        UsageGroupBy::Node => "node_name",
        UsageGroupBy::Tenant => "tenant_id",
    }
}

/// A period's groups: calls and the five sums per key, in key order, after a key. Built from the
/// grouping alone; every value is a parameter.
fn usage_groups_sql(group_by: UsageGroupBy) -> String {
    format!(
        r#"
SELECT group_key, calls, uncached_input, cache_read, cache_write, output, reasoning FROM (
    SELECT {key} AS group_key,
           COUNT(*) AS calls,
           COALESCE(SUM(uncached_input), 0)::BIGINT AS uncached_input,
           COALESCE(SUM(cache_read), 0)::BIGINT AS cache_read,
           COALESCE(SUM(cache_write), 0)::BIGINT AS cache_write,
           COALESCE(SUM(output), 0)::BIGINT AS output,
           COALESCE(SUM(reasoning), 0)::BIGINT AS reasoning
    FROM llm_usage
    WHERE ($1::text IS NULL OR tenant_id = $1) AND ts >= $2 AND ts < $3
    GROUP BY 1
) AS usage_groups
WHERE ($4::text IS NULL OR group_key > $4)
ORDER BY group_key
LIMIT $5
"#,
        key = usage_group_key(group_by)
    )
}

/// One page of a checked `USAGE_QUERY`. It fetches one row past the limit, to know whether more
/// remain.
async fn usage_query_page(
    client: &Client,
    plan: &UsageQueryPlan,
) -> Result<UsageQueryResponse, StorageError> {
    let fetch = i64::from(plan.limit) + 1;
    let mut response = UsageQueryResponse {
        ok: true,
        ..UsageQueryResponse::default()
    };
    match &plan.mode {
        UsageQueryMode::Groups {
            from,
            to,
            group_by,
            after,
        } => {
            let rows = client
                .query(
                    usage_groups_sql(*group_by).as_str(),
                    &[&plan.tenant_id, from, to, after, &fetch],
                )
                .await?;
            let groups = rows
                .iter()
                .map(|row| {
                    let sum = |index: usize| u64::try_from(row.get::<_, i64>(index)).unwrap_or(0);
                    let key: String = row.get(0);
                    let group = UsageGroup {
                        key: key.clone(),
                        calls: sum(1),
                        uncached_input: sum(2),
                        cache_read: sum(3),
                        cache_write: sum(4),
                        output: sum(5),
                        reasoning: sum(6),
                    };
                    (key, group)
                })
                .collect();
            let (groups, last, more) = page_usage(groups, plan.limit);
            response.groups = groups;
            response.next = last.filter(|_| more);
        }
        UsageQueryMode::Records { from, to, after } => {
            let (after_ts, after_id) = after.clone().unzip();
            let rows = client
                .query(
                    r#"
SELECT * FROM llm_usage
WHERE ($1::text IS NULL OR tenant_id = $1) AND ts >= $2 AND ts < $3
  AND ($4::timestamptz IS NULL OR (ts, record_id) > ($4::timestamptz, $5::text))
ORDER BY ts, record_id
LIMIT $6
"#,
                    &[&plan.tenant_id, from, to, &after_ts, &after_id, &fetch],
                )
                .await?;
            let records = rows
                .iter()
                .map(|row| {
                    (
                        usage_cursor(row.get("ts"), row.get("record_id")),
                        usage_record_json(row),
                    )
                })
                .collect();
            let (records, last, more) = page_usage(records, plan.limit);
            response.records = records;
            response.next = last.filter(|_| more);
        }
        UsageQueryMode::Since {
            stored_at,
            record_id,
        } => {
            let rows = client
                .query(
                    r#"
SELECT * FROM llm_usage
WHERE ($1::text IS NULL OR tenant_id = $1)
  AND (stored_at, record_id) > ($2::timestamptz, $3::text)
ORDER BY stored_at, record_id
LIMIT $4
"#,
                    &[&plan.tenant_id, stored_at, record_id, &fetch],
                )
                .await?;
            let records = rows
                .iter()
                .map(|row| {
                    (
                        usage_cursor(row.get("stored_at"), row.get("record_id")),
                        usage_record_json(row),
                    )
                })
                .collect();
            // A sync keeps the last record's cursor, whether more remain or not.
            let (records, last, _) = page_usage(records, plan.limit);
            response.records = records;
            response.next = last;
        }
    }
    Ok(response)
}

/// One page of an answer: the items in order, at most `limit` and `USAGE_QUERY_PAGE_BYTES` (always
/// at least one), the cursor of the last one taken, and whether any were left out. Each item comes
/// with its cursor.
fn page_usage<T: Serialize>(items: Vec<(String, T)>, limit: u32) -> (Vec<T>, Option<String>, bool) {
    let total = items.len();
    let mut page = Vec::new();
    let mut last = None;
    let mut bytes = 0usize;
    for (cursor, item) in items {
        // Its bytes and a separator.
        let size = serde_json::to_vec(&item).map_or(0, |item| item.len()) + 1;
        if page.len() >= limit as usize
            || (!page.is_empty() && bytes + size > USAGE_QUERY_PAGE_BYTES)
        {
            break;
        }
        bytes += size;
        page.push(item);
        last = Some(cursor);
    }
    let more = page.len() < total;
    (page, last, more)
}

/// A stored usage record as `USAGE_QUERY` answers it: its columns, `ts` and `stored_at` in RFC 3339
/// (to the microsecond, as stored), the null ones left out.
fn usage_record_json(row: &Row) -> Value {
    let time = |column: &str| rfc3339_micros(row.get(column));
    let text = |column: &str| row.get::<_, Option<String>>(column);
    let count = |column: &str| row.get::<_, Option<i64>>(column);
    let mut record = json!({
        "record_id": text("record_id"),
        "provider_response_id": text("provider_response_id"),
        "ts": time("ts"),
        "stored_at": time("stored_at"),
        "hive": text("hive"),
        "node_name": text("node_name"),
        "node_ilk": text("node_ilk"),
        "tenant_id": text("tenant_id"),
        "tenant_source": text("tenant_source"),
        "claimed_tenant_id": text("claimed_tenant_id"),
        "verified": row.get::<_, bool>("verified"),
        "counterpart_ilk": text("counterpart_ilk"),
        "provider": text("provider"),
        "model": text("model"),
        "served_model": text("served_model"),
        "service_tier": text("service_tier"),
        "kind": text("kind"),
        "outcome": text("outcome"),
        "uncached_input": count("uncached_input"),
        "cache_read": count("cache_read"),
        "cache_write": count("cache_write"),
        "output": count("output"),
        "reasoning": count("reasoning"),
        "raw_usage": row.get::<_, Option<Value>>("raw_usage"),
        "trace_id": text("trace_id"),
        "thread_id": text("thread_id"),
        "work": row.get::<_, Option<Value>>("work"),
    });
    if let Some(fields) = record.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
    record
}

/// One record of a stream batch, ready for the inbox.
struct StreamRow {
    dedupe_key: String,
    subject: String,
    kind: HandlerKind,
    payload: Vec<u8>,
    source_ts: std::time::SystemTime,
}

/// The records of a batch SY.storage takes, how many it drops, and why it refused each usage record
/// it dropped. It drops a subject it has no handler for; a record whose producer is not a node of
/// the stream's hive (a router copies and takes only what its own nodes send); a cognition record
/// not from that hive's SY.cognition; a turn whose message names another sender than its producer
/// (Archi shows that name); a usage record `check_usage_record` refuses. SY.cognition's packed
/// envelopes (`storage.cognition.batch`) become one row each, as if each had come alone. A resent
/// record lands on the same keys: the same subject and payload, the same row.
fn stream_rows(
    batch: &StreamBatch,
    from: &str,
) -> Result<(Vec<StreamRow>, usize, Vec<&'static str>), StorageError> {
    let hive = stream_hive(from).unwrap_or_default();
    let cognition_producer = format!("SY.cognition@{hive}");
    let mut rows = Vec::with_capacity(batch.records.len());
    let mut dropped = 0usize;
    let mut usage_refused = Vec::new();
    for record in &batch.records {
        let producer_hive = record.producer.rsplit_once('@').map(|(_, hive)| hive);
        if producer_hive != Some(hive) {
            dropped += 1;
            continue;
        }
        if record.subject.starts_with(STREAM_SUBJECT_COGNITION_PREFIX)
            && !record.producer.eq_ignore_ascii_case(&cognition_producer)
        {
            dropped += 1;
            continue;
        }
        if record.subject == SUBJECT_STORAGE_COGNITION_BATCH {
            let source_ts = std::time::UNIX_EPOCH + Duration::from_millis(record.ts_ms);
            let Ok(packed) =
                serde_json::from_value::<CognitionEnvelopeBatch>(record.payload.clone())
            else {
                dropped += 1;
                continue;
            };
            for envelope in packed.envelopes {
                let subject = envelope
                    .get("entity")
                    .cloned()
                    .and_then(|entity| {
                        serde_json::from_value::<CognitionDurableEntity>(entity).ok()
                    })
                    .map(CognitionDurableEntity::subject);
                let Some((subject, kind)) =
                    subject.and_then(|subject| Some((subject, handler_kind_for_subject(subject)?)))
                else {
                    dropped += 1;
                    continue;
                };
                let payload = serde_json::to_vec(&envelope)?;
                rows.push(StreamRow {
                    dedupe_key: message_dedupe_key(subject, &payload),
                    subject: subject.to_string(),
                    kind,
                    payload,
                    source_ts,
                });
            }
            continue;
        }
        let Some(kind) = handler_kind_for_subject(&record.subject) else {
            dropped += 1;
            continue;
        };
        if record.subject == SUBJECT_STORAGE_TURNS
            && record.payload["routing"]["src_l2_name"].as_str() != Some(record.producer.as_str())
        {
            dropped += 1;
            continue;
        }
        if record.subject == SUBJECT_STORAGE_USAGE {
            if let Err(reason) = check_usage_record(&record.producer, &record.payload) {
                dropped += 1;
                usage_refused.push(reason);
                continue;
            }
        }
        let payload = serde_json::to_vec(&record.payload)?;
        rows.push(StreamRow {
            dedupe_key: message_dedupe_key(&record.subject, &payload),
            subject: record.subject.clone(),
            kind,
            payload,
            source_ts: std::time::UNIX_EPOCH + Duration::from_millis(record.ts_ms),
        });
    }
    Ok((rows, dropped, usage_refused))
}

/// Who may record usage, by the producer's name (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageProducer {
    /// A node outside `SY.*`: it pays its own tenant.
    Node,
    /// SY.cognition: the communication's agent's tenant.
    Cognition,
    /// SY.frontdesk.gov: the case's tenant.
    Frontdesk,
    /// SY.architect and SY.admin's executor: the root tenant.
    Root,
}

/// `None`: an `SY.*` name that may not record usage.
fn usage_producer(name: &str) -> Option<UsageProducer> {
    let base = name
        .split_once('@')
        .map_or(name, |(base, _)| base)
        .to_ascii_lowercase();
    if !base.starts_with("sy.") {
        return Some(UsageProducer::Node);
    }
    match base.as_str() {
        "sy.cognition" => Some(UsageProducer::Cognition),
        "sy.frontdesk.gov" => Some(UsageProducer::Frontdesk),
        "sy.architect" | "sy.admin" => Some(UsageProducer::Root),
        _ => None,
    }
}

/// The checks a usage record passes before it enters the inbox (§7.5), on top of the stream's (its
/// producer is a node of the stream's hive; the routers decide which hives may produce each `SY.*`
/// name). `Err` names why it is refused. Its tenant is decided when it is processed.
fn check_usage_record(producer: &str, payload: &Value) -> Result<(), &'static str> {
    let role = usage_producer(producer).ok_or("foreign_sy")?;
    let record = UsageRecord::deserialize(payload).map_err(|_| "malformed")?;
    // Its time and counts must fit their columns; a negative count does not parse.
    let counts = [
        record.uncached_input,
        record.cache_read,
        record.cache_write,
        record.output,
        record.reasoning,
    ];
    if chrono::DateTime::parse_from_rfc3339(&record.ts).is_err()
        || counts
            .into_iter()
            .flatten()
            .any(|count| i64::try_from(count).is_err())
    {
        return Err("malformed");
    }
    if record.defect().is_some() {
        return Err("defective");
    }
    if record.node_name != producer {
        return Err("not_producer");
    }
    let fits = match record.tenant_source {
        TenantSource::Node => role == UsageProducer::Node,
        // The agent's tenant is looked up by the agent's ILK.
        TenantSource::Agent => role == UsageProducer::Cognition && record.counterpart_ilk.is_some(),
        TenantSource::Case => role == UsageProducer::Frontdesk,
        TenantSource::Root => role == UsageProducer::Root,
        TenantSource::FallbackRoot => true,
    };
    if !fits {
        return Err("wrong_source");
    }
    if (role == UsageProducer::Root || record.tenant_source == TenantSource::FallbackRoot)
        && record.tenant_id != DEFAULT_ROOT_TENANT_ID
    {
        return Err("not_root");
    }
    Ok(())
}

/// Takes the stream batches the main loop hands over, one at a time, in arrival order, and hands
/// their new rows to a task of their own: an ack never waits for the previous batch's processing.
async fn run_stream_ingest(
    storage: Arc<Storage>,
    dispatcher: Arc<RouterDispatcher>,
    mut rx: tokio::sync::mpsc::Receiver<Message>,
) {
    let (process_tx, mut process_rx) =
        tokio::sync::mpsc::channel::<Vec<(StreamRow, i32)>>(STREAM_PROCESS_QUEUE);
    let processor = Arc::clone(&storage);
    std::mem::drop(tokio::spawn(async move {
        while let Some(rows) = process_rx.recv().await {
            for (row, attempt) in rows {
                // A failure is recorded on the row, which the inbox retry takes again.
                let _ = processor
                    .process_inbox_message(
                        &row.dedupe_key,
                        &row.subject,
                        row.kind,
                        &row.payload,
                        Some(row.source_ts),
                        attempt,
                    )
                    .await;
            }
        }
    }));
    while let Some(msg) = rx.recv().await {
        match ingest_stream_batch(&storage, &dispatcher, &msg).await {
            Ok(rows) if rows.is_empty() => {}
            Ok(rows) => {
                if process_tx.try_send(rows).is_err() {
                    tracing::warn!("sy.storage stream processing queue full; the rows wait in the inbox for the retry");
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "sy.storage could not take a stream batch; its stream sends it again");
            }
        }
    }
}

/// A batch from a router's stream (docs/router-stream-v1.md §2.7): into the inbox in one
/// transaction, then acked. Returns the rows not processed yet, with their attempts, for the
/// processing task. A record SY.storage does not take (see `stream_rows`) is acked and dropped; a
/// refused usage record is counted once acked.
async fn ingest_stream_batch(
    storage: &Storage,
    dispatcher: &RouterDispatcher,
    msg: &Message,
) -> Result<Vec<(StreamRow, i32)>, StorageError> {
    let from = msg.routing.src_l2_name.as_deref().unwrap_or("");
    if !is_stream_node_name(from) {
        tracing::warn!(
            from,
            "sy.storage dropped a STREAM_BATCH that does not come from a router stream"
        );
        return Ok(Vec::new());
    }
    let batch: StreamBatch = serde_json::from_value(msg.payload.clone())?;
    let Some(last_seq) = batch.records.last().map(|record| record.seq) else {
        return Ok(Vec::new());
    };
    let (rows, dropped, usage_refused) = stream_rows(&batch, from)?;
    let attempts = storage.upsert_stream_rows(&rows).await?;
    let sender = dispatcher.sender_snapshot();
    let ack = build_system_reply(
        &sender,
        msg,
        MSG_STREAM_ACK,
        json!({ "up_to_seq": last_seq }),
    );
    sender.send(ack).await?;
    if !usage_refused.is_empty() {
        let mut counts = storage.usage_counts();
        for reason in usage_refused {
            *counts.refused.entry(reason).or_default() += 1;
        }
    }
    tracing::debug!(
        stream = from,
        records = batch.records.len(),
        taken = rows.len(),
        dropped,
        up_to_seq = last_seq,
        "sy.storage took a stream batch"
    );
    Ok(rows
        .into_iter()
        .zip(attempts)
        .filter_map(|(row, attempt)| attempt.map(|attempt| (row, attempt)))
        .collect())
}

/// The communication an envelope belongs to: its agent and the turn it was written at (§6.2).
fn envelope_communication<T>(
    envelope: &CognitionDurableEnvelope<T>,
) -> Result<(String, i64), StorageError> {
    let agent_ilk = envelope
        .agent_ilk
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .ok_or_else(|| missing_field("agent_ilk"))?;
    let turn = i64::try_from(envelope.turn.unwrap_or(0)).unwrap_or(i64::MAX);
    Ok((agent_ilk.to_string(), turn))
}

/// The ILK whose tenant pays for a usage record, as SY.storage finds it in identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityLookup<'a> {
    /// A node's own ILK, by its `handler_node`.
    HandlerNode(&'a str),
    /// The ILK of the agent SY.cognition served.
    Ilk(&'a str),
}

/// The tenant a usage record is stored with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageCharge {
    tenant_id: String,
    /// False when identity never answered and the record keeps the tenant it claims.
    verified: bool,
}

/// Decides the tenant a usage record is charged to (§7.5). A node outside `SY.*` pays its own
/// tenant and SY.cognition its agent's, both as identity holds them (`tenant_of` gives an ILK's
/// tenant, `None` when identity does not hold it); SY.frontdesk.gov's case tenant is taken as
/// recorded; `fallback_root`, SY.architect and SY.admin pay with the root tenant, which the ingest
/// checks held them to. Identity that cannot answer is an error, which the inbox retries; on the
/// row's last attempt the record keeps the tenant it claims, unverified: spend is never dropped
/// for a lookup.
fn decide_usage_tenant(
    record: &UsageRecord,
    last_attempt: bool,
    tenant_of: impl FnOnce(IdentityLookup<'_>) -> Result<Option<String>, StorageError>,
) -> Result<UsageCharge, StorageError> {
    let role = usage_producer(&record.node_name)
        .ok_or_else(|| format!("{} may not record usage", record.node_name))?;
    let lookup = match (role, record.tenant_source) {
        (UsageProducer::Node, _) => IdentityLookup::HandlerNode(&record.node_name),
        (UsageProducer::Cognition, TenantSource::Agent) => IdentityLookup::Ilk(
            record
                .counterpart_ilk
                .as_deref()
                .ok_or_else(|| missing_field("counterpart_ilk"))?,
        ),
        (UsageProducer::Frontdesk, TenantSource::Case) => {
            return Ok(UsageCharge {
                tenant_id: record.tenant_id.clone(),
                verified: true,
            })
        }
        _ => {
            return Ok(UsageCharge {
                tenant_id: DEFAULT_ROOT_TENANT_ID.to_string(),
                verified: true,
            })
        }
    };
    match tenant_of(lookup) {
        Ok(Some(tenant_id)) => Ok(UsageCharge {
            tenant_id,
            verified: true,
        }),
        Ok(None) | Err(_) if last_attempt => Ok(UsageCharge {
            tenant_id: record.tenant_id.clone(),
            verified: false,
        }),
        Ok(None) => Err(format!("identity holds no tenant for {lookup:?} yet").into()),
        Err(err) => Err(err),
    }
}

/// The tenant of an ILK in the motherbee's identity region, read for each record: never from a
/// cache, so a node name that another tenant took since is never charged to the old one.
fn identity_tenant(lookup: IdentityLookup<'_>) -> Result<Option<String>, StorageError> {
    Ok(match lookup {
        IdentityLookup::HandlerNode(node_name) => {
            fluxbee_sdk::identity::find_ilk_by_handler_node_from_hive_id(
                PRIMARY_HIVE_ID,
                node_name,
            )?
            .map(|(_, ilk)| ilk.tenant_id)
        }
        IdentityLookup::Ilk(ilk_id) => {
            fluxbee_sdk::identity::list_ilks_from_hive_id(PRIMARY_HIVE_ID)?
                .ilks
                .into_iter()
                .find(|ilk| ilk.ilk_id == ilk_id)
                .map(|ilk| ilk.tenant_id)
        }
    })
}

fn truncate_error(input: &str, max_len: usize) -> String {
    if input.len() <= max_len {
        return input.to_string();
    }
    input.chars().take(max_len).collect()
}

async fn run_storage_metrics_loop(storage: Arc<Storage>) {
    let mut ticker = time::interval(Duration::from_secs(STORAGE_METRICS_LOG_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        report_usage_counts(&storage);
        match storage.inbox_metrics().await {
            Ok(metrics) => {
                if metrics.pending >= STORAGE_METRICS_WARN_PENDING
                    || metrics.oldest_pending_age_s >= STORAGE_METRICS_WARN_AGE_SECS
                    || metrics.pending_with_error > 0
                {
                    tracing::warn!(
                        pending = metrics.pending,
                        pending_with_error = metrics.pending_with_error,
                        oldest_pending_age_s = metrics.oldest_pending_age_s,
                        "storage inbox lag is above threshold"
                    );
                } else {
                    tracing::debug!(
                        pending = metrics.pending,
                        pending_with_error = metrics.pending_with_error,
                        oldest_pending_age_s = metrics.oldest_pending_age_s,
                        "storage inbox metrics"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to collect storage inbox metrics");
            }
        }
    }
}

/// One WARN line when usage records were refused, charged to another tenant than they claimed, or
/// stored unverified since the last (§7.5); `ops.py health` counts it as `usagerej`.
fn report_usage_counts(storage: &Storage) {
    let counts = std::mem::take(&mut *storage.usage_counts());
    let refused: u64 = counts.refused.values().sum();
    if refused + counts.mismatched + counts.unverified == 0 {
        return;
    }
    let reasons = counts
        .refused
        .iter()
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(",");
    tracing::warn!(
        refused,
        mismatched = counts.mismatched,
        unverified = counts.unverified,
        reasons = %reasons,
        "sy.storage usage records refused, mismatched or unverified in the last 30 s"
    );
}

async fn load_hive(config_dir: &Path) -> Result<HiveFile, StorageError> {
    let data = tokio::fs::read_to_string(config_dir.join("hive.yaml")).await?;
    Ok(serde_yaml::from_str(&data)?)
}

/// Resolve storage's Postgres URL by reading the locally persisted
/// `postgres_url_ref` and looking it up in SY.vault via an ephemeral SDK
/// client. Returns `(Some(url), LocalFile, None)` on success,
/// `(None, Missing, Some(reason))` on any failure (degraded boot).
/// Resolve the postgres URL from SY.vault using the caller's **already
/// established persistent router connection**. This is the option-C fix to
/// the boot race: we no longer open an ephemeral connection here. If we
/// did, we would close it before vault boots, never re-register, and miss
/// vault's bootstrap broadcast. By reusing the persistent connection that
/// is already announced in the router, any subsequent broadcast lands in
/// our receive loop (or the rescue handler in handle_vault_secret_changed).
///
/// That rescue is PUSH, though, and the vault's bootstrap broadcast is a one-shot event: a
/// storage that was not listening yet never sees it and stays degraded until a human restarts
/// it. So this also PULLS — see `resolve_resource_awaiting_vault`, shared with SY.identity,
/// which had the retry all along. Storage was the outlier.
async fn resolve_database_url(
    vault: &VaultClient,
    node_name: &str,
    my_tenant: &str,
) -> (Option<String>, StorageDbSecretSource, Option<String>) {
    let result = vault
        .resolve_resource_awaiting_vault(
            fluxbee_sdk::ResourceType::Postgres,
            my_tenant,
            Duration::from_secs(5),
            fluxbee_sdk::VAULT_BOOT_WAIT,
            node_name,
        )
        .await;
    let _ = node_name; // silence unused when feature flags trim logging
    match result {
        Ok(Some(value)) => match extract_postgres_url_from_vault_value(&value) {
            Some(url) => (Some(url), StorageDbSecretSource::LocalFile, None),
            None => (
                None,
                StorageDbSecretSource::Missing,
                Some("vault postgres secret did not carry a usable URL value".to_string()),
            ),
        },
        Ok(None) => (None, StorageDbSecretSource::Missing, None),
        Err(err) => (
            None,
            StorageDbSecretSource::Missing,
            Some(format!("vault resource lookup for postgres failed: {err}")),
        ),
    }
}

fn extract_postgres_url_from_vault_value(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(s.to_string());
    }
    value
        .get("postgres_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn database_config_from_url(url: &str) -> Result<PgConfig, StorageError> {
    let config: PgConfig = url.parse()?;
    // The vault postgres secret must contain credentials + host ONLY.
    // The dbname is the consumer's responsibility (storage uses STORAGE_DB_NAME,
    // identity uses IDENTITY_DB_NAME, etc.) — that way one secret per cluster
    // serves every consumer in the tenant. Rejecting up-front so the operator
    // notices early instead of getting a stale/duplicate secret.
    if !config.get_dbname().map(str::trim).unwrap_or("").is_empty() {
        return Err(format!(
            "postgres secret must not include a dbname (got '{}'); load only credentials + host (postgresql://user:pass@host:port)",
            config.get_dbname().unwrap_or("")
        )
        .into());
    }
    Ok(config)
}

async fn initialize_storage_backend(
    database_url: Option<&str>,
    node_name: &str,
) -> Option<Arc<Storage>> {
    let Some(database_url) = database_url.filter(|value| !value.trim().is_empty()) else {
        return None;
    };
    let base_database_config = match database_config_from_url(database_url) {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "invalid SY.storage database url; starting without DB backend"
            );
            return None;
        }
    };
    if let Err(err) = ensure_database_exists(&base_database_config, STORAGE_DB_NAME).await {
        exit_without_database(node_name, "ensure the storage database exists", &err);
    }
    let storage_database_config = with_dbname(&base_database_config, STORAGE_DB_NAME);
    let storage = match Storage::connect(&storage_database_config).await {
        Ok(storage) => Arc::new(storage),
        Err(err) => exit_without_database(node_name, "connect to the storage database", &err),
    };
    if let Err(err) = storage.ensure_schema().await {
        exit_without_database(node_name, "ensure the storage schema", &err);
    }
    Some(storage)
}

/// A database is configured but cannot be used. SY.storage exits, and systemd starts it again
/// (Restart=always) until postgres answers. Running on without a database, as it did until 0.1.74,
/// left it with no ingestion and no retry until someone restarted it (A-70).
fn exit_without_database(node_name: &str, what: &str, err: &StorageError) -> ! {
    tracing::error!(
        node_name = %node_name,
        error = %err,
        "SY.storage could not {what}; exiting so that systemd retries"
    );
    std::process::exit(1);
}

fn with_dbname(base: &PgConfig, dbname: &str) -> PgConfig {
    let mut cfg = base.clone();
    cfg.dbname(dbname);
    cfg
}

fn admin_db_config(base: &PgConfig) -> PgConfig {
    with_dbname(base, "postgres")
}

async fn ensure_database_exists(base: &PgConfig, dbname: &str) -> Result<(), StorageError> {
    let admin_cfg = admin_db_config(base);
    let (client, connection) = admin_cfg.connect(NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "postgres admin connection closed");
        }
    });
    let exists = client
        .query_opt("SELECT 1 FROM pg_database WHERE datname = $1", &[&dbname])
        .await?
        .is_some();
    if !exists {
        let create_sql = format!("CREATE DATABASE \"{dbname}\"");
        if let Err(err) = client.execute(&create_sql, &[]).await {
            if err.code() == Some(&SqlState::DUPLICATE_DATABASE) {
                tracing::info!(db = dbname, "storage database already exists (race)");
            } else {
                return Err(err.into());
            }
        } else {
            tracing::info!(db = dbname, "created storage database");
        }
    }
    Ok(())
}

fn parse_turn(value: Value) -> Result<TurnRecord, StorageError> {
    let trace_id = required_str(&value, &["routing", "trace_id"], "routing.trace_id")?;
    let ctx = get_str(&value, &["meta", "ctx"]).unwrap_or_else(|| format!("trace:{trace_id}"));
    let seq = get_i64(&value, &["meta", "ctx_seq"]).unwrap_or_else(|| stable_i64(&trace_id));
    let from_ilk = get_str(&value, &["meta", "src_ilk"])
        .or_else(|| get_str(&value, &["routing", "src"]))
        .ok_or_else(|| missing_field("meta.src_ilk|routing.src"))?;
    let to_ilk = get_str(&value, &["meta", "dst_ilk"]);
    let ich = get_str(&value, &["meta", "ich"]).unwrap_or_else(|| "unknown".to_string());
    let msg_type = get_str(&value, &["meta", "msg_type"])
        .or_else(|| get_str(&value, &["meta", "type"]))
        .unwrap_or_else(|| "unknown".to_string());
    let content = value.get("payload").cloned().unwrap_or(Value::Null);
    let mut tags = get_string_vec(&value, &["payload", "tags"])
        .or_else(|| get_string_vec(&value, &["meta", "tags"]))
        .unwrap_or_default();
    dedup_sort(&mut tags);
    let thread_id = get_str(&value, &["meta", "thread_id"]);
    Ok(TurnRecord {
        ctx,
        seq,
        from_ilk,
        to_ilk,
        ich,
        msg_type,
        content,
        tags,
        thread_id,
        trace_id,
        message: value,
    })
}

fn parse_cognition_envelope<T>(
    payload: &[u8],
    expected: CognitionDurableEntity,
) -> Result<CognitionDurableEnvelope<T>, StorageError>
where
    T: DeserializeOwned,
{
    let envelope: CognitionDurableEnvelope<T> = serde_json::from_slice(payload)?;
    if envelope.schema_version != COGNITION_DURABLE_SCHEMA_VERSION {
        return Err(format!(
            "invalid payload: cognition schema {} (this SY.storage takes {})",
            envelope.schema_version, COGNITION_DURABLE_SCHEMA_VERSION
        )
        .into());
    }
    if envelope.entity != expected {
        return Err(format!(
            "invalid payload: cognition entity mismatch (expected {:?}, got {:?})",
            expected, envelope.entity
        )
        .into());
    }
    if envelope.entity.requires_thread_id()
        && envelope
            .thread_id
            .as_deref()
            .map_or(true, |value| value.trim().is_empty())
    {
        return Err(missing_field("thread_id"));
    }
    Ok(envelope)
}

fn json_value<T: Serialize>(value: &T) -> Result<Value, StorageError> {
    Ok(serde_json::to_value(value)?)
}

fn opt_i64(value: Option<u64>) -> Option<i64> {
    value.and_then(|v| i64::try_from(v).ok())
}

fn normalized_status(op: &CognitionDurableOp, status: &str) -> String {
    if matches!(op, CognitionDurableOp::Close) {
        "closed".to_string()
    } else {
        status.to_string()
    }
}

fn missing_field(field: &str) -> StorageError {
    format!("invalid payload: missing or invalid {field}").into()
}

fn required_str(value: &Value, path: &[&str], field: &str) -> Result<String, StorageError> {
    get_str(value, path).ok_or_else(|| missing_field(field))
}

fn get_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn get_str(value: &Value, path: &[&str]) -> Option<String> {
    match get_value(value, path) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        Some(v) if !v.is_null() => Some(v.to_string()),
        _ => None,
    }
}

fn get_i64(value: &Value, path: &[&str]) -> Option<i64> {
    let v = get_value(value, path)?;
    if let Some(i) = v.as_i64() {
        return Some(i);
    }
    v.as_str()?.parse::<i64>().ok()
}

fn get_string_vec(value: &Value, path: &[&str]) -> Option<Vec<String>> {
    let array = get_value(value, path)?.as_array()?;
    let mut out = Vec::new();
    for item in array {
        if let Some(s) = item.as_str() {
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    }
    Some(out)
}

fn dedup_sort(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn stable_u64(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn stable_i64(value: &str) -> i64 {
    (stable_u64(value) & (i64::MAX as u64)) as i64
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee")
}

const RPC_CH_SYSTEM: &str = "system";

fn build_storage_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}

/// Handle a `VAULT_SECRET_CHANGED` broadcast from SY.vault. If the event
/// matches our interest (postgres in our root-tenant pool), we auto-restart
/// the process via `exit(0)` so systemd reboots us and we pick up the
/// freshly-published secret cleanly. This is the canonical way Model D'
/// closes the "boot without secret → secret arrives later" race that
/// stranded sy-storage in `secret_source = Missing` (the polling refresh
/// loop only flipped `live_in_vault` for reporting — the pool itself
/// stayed disconnected).
///
/// Exit-based reconnect is chosen over hot-swap because the storage
/// backend is handed to its tasks (the stream ingest, the inbox maintenance,
/// the metrics and state answers) by Arc-cloned ownership at boot. Restart is
/// one line; hot-swap would require an Arc<RwLock<Option<...>>> migration
/// across all of them. For rotations, a restart only pauses the streams'
/// deliveries, which wait on disk for the next ack.
fn handle_vault_secret_changed(
    msg: &Message,
    control_state: &StorageControlState,
    node_name: &str,
) {
    tracing::info!(
        node_name = %node_name,
        trace_id = %msg.routing.trace_id,
        my_tenant = %control_state.self_tenant_id,
        my_ilk = %control_state.self_ilk_id,
        "sy.storage handle_vault_secret_changed: entered"
    );
    // Fail-closed origin check (F-03): the reaction is exit(0) (systemd
    // restart), so only the LOCAL SY.vault may trigger it. src_l2_name is
    // stamped authoritatively by the router; a forged VAULT_SECRET_CHANGED from
    // any other node (or another hive's vault) must NOT be able to restart-loop
    // this storage node. Own hive is the `@<hive>` suffix of our node_name.
    let own_hive = node_name
        .rsplit_once('@')
        .map(|(_, h)| h.trim())
        .unwrap_or("");
    let expected_origin = format!("SY.vault@{own_hive}");
    match msg.routing.src_l2_name.as_deref().map(str::trim) {
        Some(origin) if !own_hive.is_empty() && origin == expected_origin => {}
        other => {
            tracing::warn!(
                node_name = %node_name,
                src_l2_name = %other.unwrap_or("<none>"),
                expected = %expected_origin,
                "VAULT_SECRET_CHANGED from a non-vault / cross-hive origin; refusing to restart storage"
            );
            return;
        }
    }
    let payload: VaultSecretChangedPayload = match serde_json::from_value(msg.payload.clone()) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(error = %err, "ignoring malformed VAULT_SECRET_CHANGED payload");
            return;
        }
    };
    let interest = VaultSecretInterest {
        resource_type: "postgres",
        my_tenant: &control_state.self_tenant_id,
        my_ilk: Some(control_state.self_ilk_id.as_str()),
        system_caller: true,
    };
    if !payload.matches_interest(&interest) {
        tracing::info!(
            node_name = %node_name,
            resource_type = %payload.resource_type,
            payload_tenant = %payload.tenant_id,
            payload_ilk = %payload.ilk.as_deref().unwrap_or(""),
            my_tenant = %control_state.self_tenant_id,
            my_ilk = %control_state.self_ilk_id,
            "VAULT_SECRET_CHANGED does not match our interest; ignoring"
        );
        return;
    }
    tracing::warn!(
        node_name = %node_name,
        op = %payload.op.as_str(),
        resource_type = %payload.resource_type,
        version = payload.version,
        key = %payload.key,
        "VAULT_SECRET_CHANGED matches our interest; exiting for systemd-managed restart to reconnect the postgres pool"
    );
    // Flush logs before exit so the message above appears in journalctl.
    // `exit(0)` is the clean shutdown signal — systemd interprets it as
    // a normal stop and applies its Restart= policy.
    std::process::exit(0);
}

async fn process_router_message(
    sender: &NodeSender,
    msg: &Message,
    control_state: &mut StorageControlState,
    node_name: &str,
) -> Result<(), StorageError> {
    tracing::info!(
        node_name = %node_name,
        trace_id = %msg.routing.trace_id,
        src = %msg.routing.src,
        dst = ?msg.routing.dst,
        msg_type = %msg.meta.msg_type,
        msg = msg.meta.msg.as_deref().unwrap_or(""),
        action = msg.meta.action.as_deref().unwrap_or(""),
        target = msg.meta.target.as_deref().unwrap_or(""),
        "sy.storage received router message"
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
            handle_vault_secret_changed(msg, control_state, node_name);
        }
        "CONFIG_GET" => {
            let payload = build_storage_config_get_payload(node_name, control_state, None);
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            tracing::info!(
                node_name = %node_name,
                trace_id = %msg.routing.trace_id,
                response_src = %response.routing.src,
                response_dst = ?response.routing.dst,
                state = %storage_state_label(control_state.secret_source),
                "sy.storage replying CONFIG_GET"
            );
            sender.send(response).await?;
        }
        "CONFIG_SET" => {
            let payload = apply_storage_config_set(msg, node_name, control_state)?;
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            tracing::info!(
                node_name = %node_name,
                trace_id = %msg.routing.trace_id,
                response_src = %response.routing.src,
                response_dst = ?response.routing.dst,
                state = %storage_state_label(control_state.secret_source),
                config_version = control_state.config_version,
                "sy.storage replying CONFIG_SET"
            );
            sender.send(response).await?;
        }
        "PING" => {
            sender
                .send(build_system_reply(
                    sender,
                    msg,
                    "PONG",
                    json!({
                        "ok": true,
                        "node_name": node_name,
                        "state": storage_state_label(control_state.secret_source),
                        "database": {
                            "mode": "postgres",
                            "source": control_state.secret_source.as_str(),
                            "configured": control_state.secret_source != StorageDbSecretSource::Missing
                        }
                    }),
                ))
                .await?;
        }
        "STATUS" => {
            sender
                .send(build_system_reply(
                    sender,
                    msg,
                    "STATUS_RESPONSE",
                    json!({
                        "ok": true,
                        "node_name": node_name,
                        "state": storage_state_label(control_state.secret_source),
                        "schema_version": control_state.schema_version,
                        "config_version": control_state.config_version,
                        "database": {
                            "mode": "postgres",
                            "source": control_state.secret_source.as_str(),
                            "configured": control_state.secret_source != StorageDbSecretSource::Missing
                        }
                    }),
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

fn bootstrap_storage_control_state(
    node_name: &str,
    secret_source: StorageDbSecretSource,
    self_ilk_id: String,
    self_tenant_id: String,
) -> Result<StorageControlState, StorageError> {
    let persisted = load_storage_config_state(node_name);
    let schema_version = persisted
        .as_ref()
        .map(|value| value.schema_version)
        .unwrap_or(STORAGE_CONFIG_SCHEMA_VERSION);
    let config_version = persisted
        .as_ref()
        .map(|value| value.config_version)
        .unwrap_or(0);
    let state = StorageControlState {
        schema_version,
        config_version,
        secret_source,
        self_ilk_id,
        self_tenant_id,
    };
    persist_storage_config_state(node_name, &state)?;
    Ok(state)
}

fn load_storage_config_state(node_name: &str) -> Option<StorageConfigStateFile> {
    let path = managed_node_config_path(node_name).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<StorageConfigStateFile>(&raw).ok()
}

fn persist_storage_config_state(
    node_name: &str,
    state: &StorageControlState,
) -> Result<(), StorageError> {
    let path = managed_node_config_path(node_name)?;
    let payload = StorageConfigStateFile {
        schema_version: state.schema_version,
        config_version: state.config_version,
        node_name: node_name.to_string(),
        config: storage_public_config(node_name, state.secret_source),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&path, &serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn storage_public_config(_node_name: &str, secret_source: StorageDbSecretSource) -> Value {
    json!({
        "database": {
            "mode": "postgres",
            "source": secret_source.as_str(),
            "resolved_from": "vault://resource_type=postgres"
        }
    })
}

fn build_storage_config_get_payload(
    node_name: &str,
    state: &StorageControlState,
    note: Option<&str>,
) -> Value {
    let configured = state.secret_source != StorageDbSecretSource::Missing;
    let mut notes = vec![
        Value::String("SY.storage uses PostgreSQL only on motherbee.".to_string()),
        Value::String(
            "Model D': credentials live entirely in SY.vault. Operator loads them with vault_put + resource_type=postgres; storage resolves from the pool at boot.".to_string(),
        ),
        Value::String(
            "VAULT_SECRET_CHANGED broadcasts trigger an exit(0) for systemd-managed restart so the pool reconnects with the new secret. CONFIG_SET on SY.storage takes no secret-bearing fields.".to_string(),
        ),
    ];
    if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
        notes.push(Value::String(note.to_string()));
    }
    let resources = json!([
        {
            "resource_type": "postgres",
            "required": true,
            "configured": configured,
            "scope": "pool (tenant or root)",
            "consumer_dbname": STORAGE_DB_NAME
        }
    ]);
    json!({
        "ok": configured,
        "node_name": node_name,
        "state": storage_state_label(state.secret_source),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "config": storage_public_config(node_name, state.secret_source),
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.storage",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": [],
            "optional_fields": [],
            "resources": resources,
            "notes": notes,
        },
        "error": if configured {
            Value::Null
        } else {
            json!({
                "code": "missing_secret",
                "message": "postgres secret not resolvable from vault pool; load it via vault_put with resource_type=postgres."
            })
        }
    })
}

fn apply_storage_config_set(
    msg: &Message,
    node_name: &str,
    control_state: &mut StorageControlState,
) -> Result<Value, StorageError> {
    let Some(requested_node_name) = msg.payload.get("node_name").and_then(Value::as_str) else {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.node_name".to_string(),
        ));
    };
    if !storage_node_name_matches(node_name, requested_node_name) {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            format!(
                "Invalid payload.node_name: expected '{}' or '{}', got '{}'",
                node_name, STORAGE_NODE_BASE_NAME, requested_node_name
            ),
        ));
    }
    let Some(schema_version_raw) = msg.payload.get("schema_version").and_then(Value::as_u64) else {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.schema_version".to_string(),
        ));
    };
    let schema_version = schema_version_raw as u32;
    let Some(config_version) = msg.payload.get("config_version").and_then(Value::as_u64) else {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.config_version".to_string(),
        ));
    };
    if config_version < control_state.config_version {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "stale_config_version",
            format!(
                "Stale config_version: received {}, current {}",
                config_version, control_state.config_version
            ),
        ));
    }
    let Some(apply_mode) = msg.payload.get("apply_mode").and_then(Value::as_str) else {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.apply_mode".to_string(),
        ));
    };
    if apply_mode != NODE_CONFIG_APPLY_MODE_REPLACE {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "unsupported_apply_mode",
            format!("Unsupported payload.apply_mode='{apply_mode}'"),
        ));
    }
    let Some(config) = msg.payload.get("config").and_then(Value::as_object) else {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            "Missing required field: payload.config".to_string(),
        ));
    };
    // Model D' — sy.storage has no secret-bearing config fields on the
    // CONFIG_SET surface. The postgres credentials live entirely in vault
    // (operator: vault_put + resource_type=postgres). On boot/refresh
    // storage discovers them via `resolve_resource`. So CONFIG_SET here
    // accepts only the envelope (schema_version/config_version/apply_mode)
    // and rejects any secret-bearing field with a clear error.
    let database = config.get("database").and_then(Value::as_object);
    if let Some(database) = database {
        for forbidden in ["postgres_url", "postgres_url_ref"] {
            if database.contains_key(forbidden) {
                return Ok(storage_config_error_response(
                    node_name,
                    control_state,
                    "invalid_config",
                    format!(
                        "config.database.{forbidden} is no longer accepted; load the postgres secret via vault_put (resource_type=postgres) and storage will discover it from the pool"
                    ),
                ));
            }
        }
    }
    control_state.schema_version = schema_version;
    control_state.config_version = config_version;
    persist_storage_config_state(node_name, control_state)?;
    Ok(json!({
        "ok": true,
        "node_name": node_name,
        "state": storage_state_label(control_state.secret_source),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": storage_public_config(node_name, control_state.secret_source),
        "resources": [{
            "resource_type": "postgres",
            "required": true,
            "configured": control_state.secret_source != StorageDbSecretSource::Missing,
            "scope": "pool (tenant or root)",
            "consumer_dbname": STORAGE_DB_NAME
        }],
        "notes": [
            "sy.storage has no secret-bearing CONFIG_SET fields in Model D'.",
            "Load postgres credentials via vault_put with resource_type=postgres; storage will pick them up at boot. VAULT_SECRET_CHANGED broadcasts trigger exit(0) + systemd restart so the pool reconnects."
        ],
        "error": Value::Null
    }))
}

fn storage_config_error_response(
    node_name: &str,
    control_state: &StorageControlState,
    code: &str,
    message: String,
) -> Value {
    json!({
        "ok": false,
        "node_name": node_name,
        "state": storage_state_label(control_state.secret_source),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": storage_public_config(node_name, control_state.secret_source),
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn storage_state_label(secret_source: StorageDbSecretSource) -> &'static str {
    match secret_source {
        StorageDbSecretSource::Missing => "missing_secret",
        _ => "configured",
    }
}

fn storage_node_name_matches(expected: &str, requested: &str) -> bool {
    requested == expected || requested == STORAGE_NODE_BASE_NAME
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{name}@{hive_id}")
    }
}

fn write_json_atomic(path: &Path, content: &str) -> Result<(), StorageError> {
    let Some(parent) = path.parent() else {
        return Err("target path has no parent directory".into());
    };
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
    use std::io::Write;
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

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::stream::StreamRecord;
    use fluxbee_sdk::{UsageKind, UsageOutcome};

    fn message(trace_id: &str, src: &str, text: &str) -> Value {
        serde_json::json!({
            "routing": {"src": src, "dst": "x", "ttl": 8, "trace_id": trace_id},
            "meta": {"type": "user", "src_ilk": src},
            "payload": {"type": "text", "content": text}
        })
    }

    #[test]
    fn a_reply_lands_on_a_key_of_its_own() {
        // The request and its reply share the trace_id, so the old (ctx, seq) key was the same and
        // the reply was dropped (A-70). The inbox key is not.
        let request = message("trace-1", "ilk:person", "hola");
        let reply = message("trace-1", "ilk:ai", "hola, en qué te ayudo");
        let request_turn = parse_turn(request.clone()).expect("request parses");
        let reply_turn = parse_turn(reply.clone()).expect("reply parses");
        assert_eq!(
            (request_turn.ctx.as_str(), request_turn.seq),
            (reply_turn.ctx.as_str(), reply_turn.seq)
        );
        let key = |value: &Value| {
            message_dedupe_key(SUBJECT_STORAGE_TURNS, &serde_json::to_vec(value).unwrap())
        };
        assert_ne!(key(&request), key(&reply));
        assert_eq!(
            key(&request),
            key(&request),
            "a redelivered turn keeps its key"
        );
    }

    #[test]
    fn the_message_log_is_created_whole() {
        let start = TURNS_SCHEMA_SQL
            .find("CREATE TABLE IF NOT EXISTS turns (")
            .unwrap();
        let turns = &TURNS_SCHEMA_SQL[start..];
        let turns = &turns[..turns.find(");").unwrap()];
        for column in [
            "dedupe_key  TEXT NOT NULL,",
            "stored_at   TIMESTAMPTZ NOT NULL DEFAULT now(),",
            "thread_id   TEXT,",
            "trace_id    TEXT,",
            "message     JSONB NOT NULL",
        ] {
            assert!(turns.contains(column), "{column}");
        }
        // The key is a unique index: `persist_turn` inserts with `ON CONFLICT (dedupe_key)`.
        assert!(!turns.contains("PRIMARY KEY"));
        for index in [
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_turns_dedupe_key ON turns (dedupe_key);",
            "CREATE INDEX IF NOT EXISTS idx_turns_ts_key ON turns (ts DESC, dedupe_key DESC);",
            "CREATE INDEX IF NOT EXISTS idx_turns_stored_at ON turns (stored_at, dedupe_key);",
            "CREATE INDEX IF NOT EXISTS idx_turns_thread ON turns (thread_id);",
        ] {
            assert!(TURNS_SCHEMA_SQL.contains(index), "{index}");
        }
        assert!(INBOX_SCHEMA_SQL.contains("source_ts    TIMESTAMPTZ,"));
    }

    fn record(seq: u64, subject: &str, producer: &str, payload: Value) -> StreamRecord {
        StreamRecord {
            seq,
            ts_ms: 1_700_000_000_000 + seq,
            subject: subject.to_string(),
            producer: producer.to_string(),
            payload,
        }
    }

    /// A turn as the router copies it: the sender's name stamped on it.
    fn copied_turn(trace_id: &str, sender: &str, text: &str) -> Value {
        let mut turn = message(trace_id, "ilk:person", text);
        turn["routing"]["src_l2_name"] = Value::String(sender.to_string());
        turn
    }

    #[test]
    fn a_batch_becomes_inbox_rows_keyed_as_before() {
        let turn = copied_turn("trace-1", "IO.slack.T1@worker1", "hola");
        let batch = StreamBatch {
            stream: "RT.gateway.stream@worker1".to_string(),
            epoch: 1,
            records: vec![record(
                7,
                SUBJECT_STORAGE_TURNS,
                "IO.slack.T1@worker1",
                turn.clone(),
            )],
        };
        let (rows, dropped, _) = stream_rows(&batch, "RT.gateway.stream@worker1").unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(rows.len(), 1);
        let bytes = serde_json::to_vec(&turn).unwrap();
        assert_eq!(
            rows[0].dedupe_key,
            message_dedupe_key(SUBJECT_STORAGE_TURNS, &bytes)
        );
        assert_eq!(rows[0].payload, bytes);
        assert!(matches!(rows[0].kind, HandlerKind::Turns));
        // `ts` is when the router copied the turn, not when the batch got here.
        assert_eq!(
            rows[0].source_ts,
            std::time::UNIX_EPOCH + Duration::from_millis(1_700_000_000_007)
        );
        let (again, _, _) = stream_rows(&batch, "RT.gateway.stream@worker1").unwrap();
        assert_eq!(
            again[0].dedupe_key, rows[0].dedupe_key,
            "a resent batch lands on the same rows"
        );
    }

    #[test]
    fn cognition_records_come_only_from_the_streams_own_cognition() {
        let subject = "storage.cognition.threads";
        let batch = StreamBatch {
            stream: "RT.gateway.stream@worker1".to_string(),
            epoch: 1,
            records: vec![
                record(
                    1,
                    subject,
                    "SY.cognition@worker1",
                    serde_json::json!({"a": 1}),
                ),
                record(2, subject, "AI.sales@worker1", serde_json::json!({"a": 2})),
                record(
                    3,
                    subject,
                    "SY.cognition@motherbee",
                    serde_json::json!({"a": 3}),
                ),
                record(
                    4,
                    "storage.usage",
                    "AI.sales@worker1",
                    serde_json::json!({"a": 4}),
                ),
                record(
                    5,
                    "storage.unknown",
                    "AI.sales@worker1",
                    serde_json::json!({"a": 5}),
                ),
            ],
        };
        let (rows, dropped, _) = stream_rows(&batch, "RT.gateway.stream@worker1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject, subject);
        assert!(matches!(rows[0].kind, HandlerKind::CognitionThreads));
        assert_eq!(dropped, 4);
    }

    #[test]
    fn a_record_comes_from_a_node_of_the_streams_hive() {
        let stream = "RT.gateway.stream@ingress1";
        let batch = StreamBatch {
            stream: stream.to_string(),
            epoch: 1,
            records: vec![
                record(
                    1,
                    SUBJECT_STORAGE_TURNS,
                    "IO.web@ingress1",
                    copied_turn("t1", "IO.web@ingress1", "ok"),
                ),
                // Another hive's node, or a message naming another sender than its producer.
                record(
                    2,
                    SUBJECT_STORAGE_TURNS,
                    "SY.admin@motherbee",
                    copied_turn("t2", "SY.admin@motherbee", "forged"),
                ),
                record(
                    3,
                    SUBJECT_STORAGE_TURNS,
                    "IO.web@ingress1",
                    copied_turn("t3", "SY.admin@motherbee", "forged"),
                ),
                record(
                    4,
                    SUBJECT_STORAGE_TURNS,
                    "IO.web@ingress1",
                    message("t4", "x", "no name"),
                ),
            ],
        };
        let (rows, dropped, _) = stream_rows(&batch, stream).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(dropped, 3);
        let kept: Value = serde_json::from_slice(&rows[0].payload).unwrap();
        assert_eq!(kept["routing"]["trace_id"], "t1");
    }

    fn cognition_envelope(entity: &str, id: &str) -> Value {
        serde_json::json!({
            "schema_version": COGNITION_DURABLE_SCHEMA_VERSION,
            "entity_version": 1,
            "entity": entity,
            "op": "upsert",
            "entity_id": id,
            "thread_id": "thread:t",
            "agent_ilk": "ilk:agent",
            "turn": 3,
            "hive": "worker1",
            "writer": "SY.cognition@worker1",
            "ts": "2026-10-17T00:00:00Z",
            "data": {"label": id, "status": "open"},
        })
    }

    #[test]
    fn a_packed_cognition_batch_becomes_one_row_per_envelope() {
        let packed = |producer: &str| StreamBatch {
            stream: "RT.gateway.stream@worker1".to_string(),
            epoch: 1,
            records: vec![record(
                1,
                SUBJECT_STORAGE_COGNITION_BATCH,
                producer,
                serde_json::to_value(CognitionEnvelopeBatch {
                    envelopes: vec![
                        cognition_envelope("context", "context:a"),
                        cognition_envelope("thread", "thread:x"),
                        cognition_envelope("nonsense", "x"),
                    ],
                })
                .unwrap(),
            )],
        };
        let (rows, dropped, _) =
            stream_rows(&packed("SY.cognition@worker1"), "RT.gateway.stream@worker1").unwrap();
        assert_eq!(dropped, 1, "an envelope of no known entity");
        assert_eq!(
            rows.iter()
                .map(|row| row.subject.as_str())
                .collect::<Vec<_>>(),
            vec![
                SUBJECT_STORAGE_COGNITION_CONTEXTS,
                SUBJECT_STORAGE_COGNITION_THREADS
            ]
        );
        assert!(matches!(rows[0].kind, HandlerKind::CognitionContexts));
        assert_eq!(
            rows[0].dedupe_key,
            message_dedupe_key(SUBJECT_STORAGE_COGNITION_CONTEXTS, &rows[0].payload)
        );
        let (rows, dropped, _) =
            stream_rows(&packed("AI.sales@worker1"), "RT.gateway.stream@worker1").unwrap();
        assert!(rows.is_empty());
        assert_eq!(
            dropped, 1,
            "cognition's records come from SY.cognition only"
        );
    }

    #[test]
    fn a_communications_state_is_paged_in_order() {
        let big = "x".repeat(COGNITION_STATE_PAGE_BYTES / 3);
        let mut state = Vec::new();
        for (entity, id) in [
            (CognitionDurableEntity::Memory, "memory:1"),
            (CognitionDurableEntity::Thread, "thread:1"),
            (CognitionDurableEntity::Context, "context:2"),
            (CognitionDurableEntity::Context, "context:1"),
        ] {
            let mut envelope = cognition_envelope("context", id);
            envelope["data"]["pad"] = Value::String(big.clone());
            state.push((entity, id.to_string(), envelope));
        }
        state.sort_by(|a, b| (entity_rank(a.0), &a.1).cmp(&(entity_rank(b.0), &b.1)));
        let first = page_cognition_state(state.clone(), None);
        assert!(first.ok);
        let ids = |page: &CognitionStateResponse| {
            page.envelopes
                .iter()
                .map(|e| e["entity_id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), vec!["thread:1", "context:1"]);
        assert_eq!(
            first.next,
            Some(("context".to_string(), "context:1".to_string()))
        );
        let second = page_cognition_state(state, first.next.as_ref());
        assert_eq!(ids(&second), vec!["context:2", "memory:1"]);
        assert_eq!(second.next, None);
    }

    #[test]
    fn cognition_rows_belong_to_a_communication() {
        for table in [
            "cognition_threads",
            "cognition_contexts",
            "cognition_reasons",
            "cognition_cooccurrences",
            "cognition_scope_instances",
            "cognition_memories",
            "cognition_episodes",
        ] {
            let ddl_start = COGNITION_SCHEMA_SQL
                .find(&format!("CREATE TABLE IF NOT EXISTS {table} ("))
                .unwrap();
            let ddl = &COGNITION_SCHEMA_SQL[ddl_start..];
            let ddl = &ddl[..ddl.find(");").unwrap()];
            assert!(ddl.contains("agent_ilk        TEXT NOT NULL"), "{table}");
            assert!(ddl.contains("source_turn      BIGINT NOT NULL"), "{table}");
            assert!(COGNITION_SCHEMA_SQL.contains(&format!(
                "idx_{table}_communication ON {table}(thread_id, agent_ilk)"
            )));
        }
        assert!(COGNITION_SCHEMA_SQL.contains("communication_id TEXT PRIMARY KEY"));
        assert!(!COGNITION_SCHEMA_SQL.contains("thread_seq"));
    }

    const TENANT: &str = "tnt:22222222-2222-4222-8222-222222222222";
    const OTHER_TENANT: &str = "tnt:33333333-3333-4333-8333-333333333333";

    fn usage_record(node_name: &str, tenant_id: &str, tenant_source: TenantSource) -> UsageRecord {
        UsageRecord {
            record_id: "8b6b3c1e-0000-4000-8000-000000000001".to_string(),
            provider_response_id: Some("msg_01".to_string()),
            ts: "2026-10-17T12:00:00.000Z".to_string(),
            hive: node_name.rsplit_once('@').unwrap().1.to_string(),
            node_name: node_name.to_string(),
            node_ilk: None,
            tenant_id: tenant_id.to_string(),
            tenant_source,
            counterpart_ilk: None,
            provider: "anthropic".to_string(),
            model: "claude-sonnet-5-5".to_string(),
            served_model: None,
            service_tier: None,
            kind: UsageKind::FunctionTurn,
            outcome: UsageOutcome::Ok,
            uncached_input: Some(1200),
            cache_read: Some(0),
            cache_write: Some(0),
            output: Some(80),
            reasoning: None,
            raw_usage: None,
            trace_id: None,
            thread_id: None,
            work: None,
        }
    }

    fn with_agent(mut record: UsageRecord) -> UsageRecord {
        record.counterpart_ilk = Some("ilk:44444444-4444-4444-8444-444444444444".to_string());
        record
    }

    #[test]
    fn a_usage_record_fits_its_producer() {
        let root = DEFAULT_ROOT_TENANT_ID;
        let cases = [
            (
                "a tenant's node",
                usage_record("AI.sales@worker1", TENANT, TenantSource::Node),
                Ok(()),
            ),
            (
                "a node that knows no tenant",
                usage_record("AI.sales@worker1", root, TenantSource::FallbackRoot),
                Ok(()),
            ),
            (
                "SY.cognition, its agent's",
                with_agent(usage_record(
                    "SY.cognition@worker1",
                    TENANT,
                    TenantSource::Agent,
                )),
                Ok(()),
            ),
            (
                "SY.cognition, an agent with no tenant",
                usage_record("SY.cognition@worker1", root, TenantSource::FallbackRoot),
                Ok(()),
            ),
            (
                "SY.frontdesk.gov, the case's",
                usage_record("SY.frontdesk.gov@motherbee", TENANT, TenantSource::Case),
                Ok(()),
            ),
            (
                "SY.architect",
                usage_record("SY.architect@motherbee", root, TenantSource::Root),
                Ok(()),
            ),
            (
                "SY.admin",
                usage_record("SY.admin@motherbee", root, TenantSource::FallbackRoot),
                Ok(()),
            ),
            (
                "another SY.* name",
                usage_record("SY.policy@motherbee", root, TenantSource::Root),
                Err("foreign_sy"),
            ),
            (
                "SY.architect charging a tenant",
                usage_record("SY.architect@motherbee", TENANT, TenantSource::Root),
                Err("not_root"),
            ),
            (
                "fallback_root charging a tenant",
                usage_record("AI.sales@worker1", TENANT, TenantSource::FallbackRoot),
                Err("not_root"),
            ),
            (
                "a node claiming a case",
                usage_record("AI.sales@worker1", TENANT, TenantSource::Case),
                Err("wrong_source"),
            ),
            (
                "a node claiming root",
                usage_record("AI.sales@worker1", root, TenantSource::Root),
                Err("wrong_source"),
            ),
            (
                "SY.cognition as a node",
                usage_record("SY.cognition@worker1", TENANT, TenantSource::Node),
                Err("wrong_source"),
            ),
            (
                "SY.cognition, an agent it does not name",
                usage_record("SY.cognition@worker1", TENANT, TenantSource::Agent),
                Err("wrong_source"),
            ),
            (
                "SY.frontdesk.gov as an agent",
                with_agent(usage_record(
                    "SY.frontdesk.gov@motherbee",
                    TENANT,
                    TenantSource::Agent,
                )),
                Err("wrong_source"),
            ),
        ];
        for (case, record, expected) in cases {
            let payload = serde_json::to_value(&record).unwrap();
            assert_eq!(
                check_usage_record(&record.node_name, &payload),
                expected,
                "{case}"
            );
        }
        let record =
            serde_json::to_value(usage_record("AI.sales@worker1", TENANT, TenantSource::Node))
                .unwrap();
        assert_eq!(
            check_usage_record("AI.other@worker1", &record),
            Err("not_producer")
        );
        let refused = |field: &str, value: Value| {
            let mut record = record.clone();
            record[field] = value;
            check_usage_record("AI.sales@worker1", &record)
        };
        assert_eq!(refused("output", json!(-1)), Err("malformed"));
        assert_eq!(refused("output", json!(u64::MAX)), Err("malformed"));
        assert_eq!(refused("ts", json!("yesterday")), Err("malformed"));
        assert_eq!(refused("tenant_source", json!("cloud")), Err("malformed"));
        assert_eq!(refused("model", json!(" ")), Err("defective"));
        assert_eq!(
            check_usage_record("AI.sales@worker1", &json!({"tokens": 12})),
            Err("malformed")
        );
    }

    #[test]
    fn a_batch_keeps_its_usage_records_and_names_the_refused() {
        let stream = "RT.gateway.stream@motherbee";
        let usage = |seq: u64, producer: &str, usage: &UsageRecord| {
            record(
                seq,
                SUBJECT_STORAGE_USAGE,
                producer,
                serde_json::to_value(usage).unwrap(),
            )
        };
        let kept = usage_record("AI.sales@motherbee", TENANT, TenantSource::Node);
        let batch = StreamBatch {
            stream: stream.to_string(),
            epoch: 1,
            records: vec![
                usage(1, "AI.sales@motherbee", &kept),
                usage(
                    2,
                    "SY.policy@motherbee",
                    &usage_record("SY.policy@motherbee", TENANT, TenantSource::Node),
                ),
                usage(3, "AI.other@motherbee", &kept),
                // Another hive's node: the stream's own check, not a usage refusal.
                usage(
                    4,
                    "AI.sales@worker1",
                    &usage_record("AI.sales@worker1", TENANT, TenantSource::Node),
                ),
            ],
        };
        let (rows, dropped, refused) = stream_rows(&batch, stream).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subject, SUBJECT_STORAGE_USAGE);
        assert!(matches!(rows[0].kind, HandlerKind::Usage));
        assert_eq!(
            rows[0].payload,
            serde_json::to_vec(&serde_json::to_value(&kept).unwrap()).unwrap()
        );
        assert_eq!(dropped, 3);
        assert_eq!(refused, vec!["foreign_sy", "not_producer"]);
    }

    /// An identity that holds `answer` for `expected`, and is asked nothing else.
    fn identity(
        expected: IdentityLookup<'static>,
        answer: Result<Option<&'static str>, &'static str>,
    ) -> impl FnOnce(IdentityLookup<'_>) -> Result<Option<String>, StorageError> {
        move |lookup| {
            assert_eq!(lookup, expected);
            answer
                .map(|tenant| tenant.map(str::to_string))
                .map_err(StorageError::from)
        }
    }

    fn not_asked(_: IdentityLookup<'_>) -> Result<Option<String>, StorageError> {
        panic!("identity is not asked for this tenant")
    }

    #[test]
    fn a_usage_records_tenant_comes_from_identity_where_it_can() {
        let charge = |tenant_id: &str, verified: bool| UsageCharge {
            tenant_id: tenant_id.to_string(),
            verified,
        };
        let node = IdentityLookup::HandlerNode("AI.sales@worker1");
        // A tenant's node pays identity's tenant, whatever it claims: a mismatch is stored with
        // identity's (and counted, `persist_usage`).
        for (claimed, source) in [
            (TENANT, TenantSource::Node),
            (OTHER_TENANT, TenantSource::Node),
            (DEFAULT_ROOT_TENANT_ID, TenantSource::FallbackRoot),
        ] {
            let record = usage_record("AI.sales@worker1", claimed, source);
            let decided =
                decide_usage_tenant(&record, false, identity(node, Ok(Some(TENANT)))).unwrap();
            assert_eq!(decided, charge(TENANT, true), "{claimed}");
        }
        // Identity without the ILK, or unreadable: an error the inbox retries, until the last
        // attempt keeps the claimed tenant, unverified.
        let record = usage_record("AI.sales@worker1", OTHER_TENANT, TenantSource::Node);
        for answer in [Ok(None), Err("identity SHM seqlock read timeout")] {
            assert!(decide_usage_tenant(&record, false, identity(node, answer)).is_err());
            assert_eq!(
                decide_usage_tenant(&record, true, identity(node, answer)).unwrap(),
                charge(OTHER_TENANT, false)
            );
        }
        // SY.cognition pays its agent's tenant, by the agent's ILK.
        let agent = with_agent(usage_record(
            "SY.cognition@worker1",
            OTHER_TENANT,
            TenantSource::Agent,
        ));
        let agent_ilk = IdentityLookup::Ilk("ilk:44444444-4444-4444-8444-444444444444");
        assert_eq!(
            decide_usage_tenant(&agent, false, identity(agent_ilk, Ok(Some(TENANT)))).unwrap(),
            charge(TENANT, true)
        );
        // The case's tenant as recorded; the root tenant for the rest; identity not asked.
        for (record, tenant) in [
            (
                usage_record("SY.frontdesk.gov@motherbee", TENANT, TenantSource::Case),
                TENANT,
            ),
            (
                with_agent(usage_record(
                    "SY.cognition@worker1",
                    DEFAULT_ROOT_TENANT_ID,
                    TenantSource::FallbackRoot,
                )),
                DEFAULT_ROOT_TENANT_ID,
            ),
            (
                usage_record(
                    "SY.architect@motherbee",
                    DEFAULT_ROOT_TENANT_ID,
                    TenantSource::Root,
                ),
                DEFAULT_ROOT_TENANT_ID,
            ),
            (
                usage_record(
                    "SY.admin@motherbee",
                    DEFAULT_ROOT_TENANT_ID,
                    TenantSource::Root,
                ),
                DEFAULT_ROOT_TENANT_ID,
            ),
        ] {
            assert_eq!(
                decide_usage_tenant(&record, false, not_asked).unwrap(),
                charge(tenant, true),
                "{}",
                record.node_name
            );
        }
        let foreign = usage_record(
            "SY.policy@motherbee",
            DEFAULT_ROOT_TENANT_ID,
            TenantSource::Root,
        );
        assert!(decide_usage_tenant(&foreign, true, not_asked).is_err());
    }

    #[test]
    fn a_usage_query_is_checked_before_it_runs() {
        let plan =
            |value: Value| plan_usage_query(&serde_json::from_value::<UsageQuery>(value).unwrap());
        let time = |value: &str| {
            std::time::SystemTime::from(chrono::DateTime::parse_from_rfc3339(value).unwrap())
        };
        let (from, to) = ("2026-07-01T00:00:00Z", "2026-10-01T00:00:00Z");
        // Exactly one of a tenant and every tenant; a blank field is absent.
        assert!(plan(json!({"from": from, "to": to})).is_err());
        assert!(plan(json!({"tenant_id": " ", "from": from, "to": to})).is_err());
        assert!(
            plan(json!({"tenant_id": TENANT, "all_tenants": true, "from": from, "to": to}))
                .is_err()
        );
        // A period of at most 92 days, `to` after `from`; records by default.
        assert_eq!(
            plan(json!({"all_tenants": true, "from": from, "to": to})).unwrap(),
            UsageQueryPlan {
                tenant_id: None,
                mode: UsageQueryMode::Records {
                    from: time(from),
                    to: time(to),
                    after: None
                },
                limit: USAGE_QUERY_DEFAULT_LIMIT,
            }
        );
        for (from, to) in [
            (from, "2026-10-01T00:00:01Z"),
            (to, to),
            (to, from),
            (from, "2026-10-01"),
        ] {
            assert!(
                plan(json!({"tenant_id": TENANT, "from": from, "to": to})).is_err(),
                "{from} {to}"
            );
        }
        assert!(plan(json!({"tenant_id": TENANT, "from": from})).is_err());
        // Grouped: the key it is after as given. Record by record: a cursor.
        assert_eq!(
            plan(
                json!({"tenant_id": TENANT, "from": from, "to": to, "group_by": "day",
                        "after": "2026-07-02", "limit": 5000})
            )
            .unwrap(),
            UsageQueryPlan {
                tenant_id: Some(TENANT.to_string()),
                mode: UsageQueryMode::Groups {
                    from: time(from),
                    to: time(to),
                    group_by: UsageGroupBy::Day,
                    after: Some("2026-07-02".to_string()),
                },
                limit: USAGE_QUERY_MAX_LIMIT,
            }
        );
        let after = format!("{}|8b6b3c1e", "2026-07-02T10:00:00.000001Z");
        let records =
            plan(json!({"tenant_id": TENANT, "from": from, "to": to, "after": after})).unwrap();
        assert_eq!(
            records.mode,
            UsageQueryMode::Records {
                from: time(from),
                to: time(to),
                after: Some((time("2026-07-02T10:00:00.000001Z"), "8b6b3c1e".to_string())),
            }
        );
        assert!(
            plan(json!({"tenant_id": TENANT, "from": from, "to": to, "after": "page 2"})).is_err()
        );
        // A sync: `since` alone, a cursor or a bare time; no period bound.
        assert_eq!(
            plan(json!({"all_tenants": true, "since": "2020-01-01T00:00:00Z", "limit": 0}))
                .unwrap(),
            UsageQueryPlan {
                tenant_id: None,
                mode: UsageQueryMode::Since {
                    stored_at: time("2020-01-01T00:00:00Z"),
                    record_id: String::new(),
                },
                limit: 1,
            }
        );
        for extra in [
            json!({"from": from}),
            json!({"to": to}),
            json!({"group_by": "model"}),
            json!({"after": after}),
        ] {
            let mut query = json!({"tenant_id": TENANT, "since": after});
            query
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(plan(query.clone()).is_err(), "{query}");
        }
        assert!(plan(json!({"tenant_id": TENANT, "since": "yesterday"})).is_err());
    }

    #[test]
    fn a_grouping_is_a_fixed_sql_expression() {
        for (group_by, key) in [
            (
                UsageGroupBy::Day,
                "to_char(date_trunc('day', ts AT TIME ZONE 'UTC'), 'YYYY-MM-DD')",
            ),
            (UsageGroupBy::Model, "model"),
            (UsageGroupBy::Node, "node_name"),
            (UsageGroupBy::Tenant, "tenant_id"),
        ] {
            assert_eq!(usage_group_key(group_by), key);
            // Built from the grouping alone: the tenant, the period, the cursor and the limit are
            // parameters.
            let sql = usage_groups_sql(group_by);
            assert!(
                sql.contains(&format!("SELECT {key} AS group_key,")),
                "{sql}"
            );
            for bound in [
                "($1::text IS NULL OR tenant_id = $1)",
                "ts >= $2 AND ts < $3",
                "($4::text IS NULL OR group_key > $4)",
                "LIMIT $5",
            ] {
                assert!(sql.contains(bound), "{bound}");
            }
            assert!(!sql.contains("$6"));
        }
    }

    #[test]
    fn a_usage_answer_is_paged_by_count_and_size() {
        let items = |sizes: &[usize]| {
            sizes
                .iter()
                .enumerate()
                .map(|(index, size)| (format!("c{index}"), json!({"pad": "x".repeat(*size)})))
                .collect::<Vec<_>>()
        };
        let (page, last, more) = page_usage(items(&[10, 10, 10]), 2);
        assert_eq!((page.len(), last.as_deref(), more), (2, Some("c1"), true));
        let (page, last, more) = page_usage(items(&[10, 10]), 2);
        assert_eq!((page.len(), last.as_deref(), more), (2, Some("c1"), false));
        // Cut by size, and always at least one.
        let third = USAGE_QUERY_PAGE_BYTES / 3;
        let (page, last, more) = page_usage(items(&[third, third, third]), 1000);
        assert_eq!((page.len(), last.as_deref(), more), (2, Some("c1"), true));
        let (page, last, more) = page_usage(items(&[2 * USAGE_QUERY_PAGE_BYTES, 10]), 1000);
        assert_eq!((page.len(), last.as_deref(), more), (1, Some("c0"), true));
        let (page, last, more) = page_usage(items(&[]), 10);
        assert_eq!((page.len(), last, more), (0, None, false));
    }

    #[test]
    fn a_usage_cursor_comes_back_as_it_went() {
        let time = std::time::UNIX_EPOCH + Duration::from_micros(1_760_000_000_123_456);
        let cursor = usage_cursor(time, "id|with|bars");
        assert_eq!(cursor, "2025-10-09T08:53:20.123456Z|id|with|bars");
        assert_eq!(
            parse_usage_cursor(&cursor),
            Some((time, "id|with|bars".to_string()))
        );
        assert_eq!(
            parse_usage_cursor("2025-10-09T08:53:20Z"),
            Some((
                std::time::UNIX_EPOCH + Duration::from_secs(1_760_000_000),
                String::new()
            ))
        );
        assert_eq!(parse_usage_cursor("page 2|x"), None);
    }
}
