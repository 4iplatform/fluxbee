use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::time;
use tokio_postgres::{error::SqlState, Client, Config as PgConfig, GenericClient, NoTls};
use tracing_subscriber::EnvFilter;

use fluxbee_sdk::nats::{
    publish_local as client_nats_publish_local, resolve_local_nats_endpoint, subscribe_local,
    NatsError as ClientNatsError, NatsRequestEnvelope, NatsResponseEnvelope,
    NATS_ENVELOPE_SCHEMA_VERSION,
};
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    build_node_config_response_message, build_node_secret_record, connect, load_node_secret_record,
    managed_node_config_path, managed_node_name, save_node_secret_record,
    try_handle_default_node_status, NodeConfig, NodeReceiver, NodeSecretDescriptor,
    NodeSecretWriteOptions, NodeSender, NodeUuidMode, NODE_CONFIG_APPLY_MODE_REPLACE,
    NODE_SECRET_REDACTION_TOKEN,
};
use json_router::nats::{
    NatsSubscriber as RouterNatsSubscriber, SUBJECT_STORAGE_EVENTS, SUBJECT_STORAGE_ITEMS,
    SUBJECT_STORAGE_REACTIVATION, SUBJECT_STORAGE_TURNS,
};

type StorageError = Box<dyn std::error::Error + Send + Sync>;
const PRIMARY_HIVE_ID: &str = "motherbee";

const NATS_ERROR_LOG_EVERY: u64 = 20;
const INBOX_REPLAY_BATCH_SIZE: i64 = 200;
const INBOX_REPLAY_MAX_ROUNDS: u32 = 20;
const INBOX_ERROR_MAX_LEN: usize = 1024;
const STORAGE_METRICS_LOG_INTERVAL_SECS: u64 = 30;
const STORAGE_METRICS_WARN_PENDING: i64 = 100;
const STORAGE_METRICS_WARN_AGE_SECS: i64 = 120;
const DURABLE_QUEUE_TURNS: &str = "durable.sy-storage.turns";
const DURABLE_QUEUE_EVENTS: &str = "durable.sy-storage.events";
const DURABLE_QUEUE_ITEMS: &str = "durable.sy-storage.items";
const DURABLE_QUEUE_REACTIVATION: &str = "durable.sy-storage.reactivation";
const SUBJECT_STORAGE_METRICS_GET: &str = "storage.metrics.get";
const STORAGE_METRICS_QUERY_SID: u32 = 19;
const STORAGE_DB_NAME: &str = "fluxbee_storage";
const STORAGE_NODE_BASE_NAME: &str = "SY.storage";
const STORAGE_NODE_VERSION: &str = "2.0";
const STORAGE_LOCAL_SECRET_KEY_POSTGRES_URL: &str = "postgres_url";
const STORAGE_CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageDbSecretSource {
    LocalFile,
    EnvCompat,
    Missing,
}

impl StorageDbSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::EnvCompat => "env_compat",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone)]
struct StorageControlState {
    schema_version: u32,
    config_version: u64,
    secret_source: StorageDbSecretSource,
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
    nats: Option<NatsSection>,
    database: Option<DatabaseSection>,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DatabaseSection {
    url: Option<String>,
}

struct Storage {
    client: Client,
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
}

#[derive(Debug)]
struct EventRecord {
    event_id: Option<i64>,
    ctx: String,
    start_seq: i64,
    end_seq: i64,
    boundary_reason: String,
    cues_agg: Vec<String>,
    outcome_status: Option<String>,
    outcome_duration_ms: Option<i64>,
    activation_strength: f64,
    context_inhibition: f64,
    use_count: i32,
    success_count: i32,
}

#[derive(Debug)]
struct MemoryItemRecord {
    memory_id: String,
    event_id: Option<i64>,
    item_type: String,
    content: Value,
    confidence: f64,
    cues_signature: Vec<String>,
    activation_strength: f64,
}

#[derive(Debug, Serialize)]
struct StorageInboxMetricsSnapshot {
    pending: i64,
    pending_with_error: i64,
    oldest_pending_age_s: i64,
    processed_total: i64,
    max_attempts: i32,
}

#[tokio::main]
async fn main() -> Result<(), StorageError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_storage supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let hive = load_hive(&config_dir).await?;
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

    let endpoint = resolve_local_nats_endpoint(&config_dir)?;
    let use_durable_consumer = hive
        .nats
        .as_ref()
        .and_then(|n| n.mode.as_deref())
        .map(|mode| mode.trim().eq_ignore_ascii_case("embedded"))
        .unwrap_or(true);
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
    let (database_url, db_secret_source) = resolve_database_url(&hive, &node_name);
    let mut control_state = bootstrap_storage_control_state(&node_name, db_secret_source)?;
    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!(node_name = %sender.full_name(), "sy.storage connected to router");

    let storage = initialize_storage_backend(database_url.as_deref(), &node_name).await;
    if let Some(storage) = storage.as_ref() {
        let replayed = storage
            .replay_pending_messages(INBOX_REPLAY_BATCH_SIZE, INBOX_REPLAY_MAX_ROUNDS)
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
        endpoint = %endpoint,
        storage_ready = storage.is_some(),
        "sy.storage started"
    );

    let nats_subscribe_errors = Arc::new(AtomicU64::new(0));
    let storage_handler_errors = Arc::new(AtomicU64::new(0));

    if let Some(storage) = storage.as_ref() {
        std::mem::drop(tokio::spawn(run_turns_loop(
            endpoint.clone(),
            use_durable_consumer,
            Arc::clone(storage),
            Arc::clone(&nats_subscribe_errors),
            Arc::clone(&storage_handler_errors),
        )));
        std::mem::drop(tokio::spawn(run_events_loop(
            endpoint.clone(),
            use_durable_consumer,
            Arc::clone(storage),
            Arc::clone(&nats_subscribe_errors),
            Arc::clone(&storage_handler_errors),
        )));
        std::mem::drop(tokio::spawn(run_items_loop(
            endpoint.clone(),
            use_durable_consumer,
            Arc::clone(storage),
            Arc::clone(&nats_subscribe_errors),
            Arc::clone(&storage_handler_errors),
        )));
        std::mem::drop(tokio::spawn(run_reactivation_loop(
            endpoint.clone(),
            use_durable_consumer,
            Arc::clone(storage),
            Arc::clone(&nats_subscribe_errors),
            Arc::clone(&storage_handler_errors),
        )));
        std::mem::drop(tokio::spawn(run_storage_metrics_loop(
            Arc::clone(storage),
            Arc::clone(&nats_subscribe_errors),
            Arc::clone(&storage_handler_errors),
        )));
    }
    std::mem::drop(tokio::spawn(run_storage_metrics_query_loop(
        config_dir.clone(),
        storage.clone(),
        Arc::clone(&nats_subscribe_errors),
        Arc::clone(&storage_handler_errors),
    )));
    let mut heartbeat = time::interval(Duration::from_secs(5));
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
            received = receiver.recv() => {
                let msg = match received {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!(error = %err, "sy.storage recv error; reconnecting");
                        let (new_sender, new_receiver) =
                            connect_with_retry(&node_config, Duration::from_secs(1)).await?;
                        sender = new_sender;
                        receiver = new_receiver;
                        tracing::info!(node_name = %sender.full_name(), "sy.storage reconnected to router");
                        continue;
                    }
                };
                if let Err(err) = process_router_message(&sender, &msg, &mut control_state, &node_name).await {
                    tracing::warn!(error = %err, action = ?msg.meta.msg, "failed to process sy.storage system message");
                }
            }
        }
    }
}

impl Storage {
    async fn connect(database_config: &PgConfig) -> Result<Self, StorageError> {
        let (client, connection) = database_config.connect(NoTls).await?;
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::error!(error = %err, "postgres connection closed");
            }
        });
        Ok(Self { client })
    }

    async fn ensure_schema(&self) -> Result<(), StorageError> {
        self.client
            .batch_execute(
                r#"
CREATE TABLE IF NOT EXISTS turns (
    ctx         TEXT NOT NULL,
    seq         BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now(),
    from_ilk    TEXT NOT NULL,
    to_ilk      TEXT,
    ich         TEXT NOT NULL,
    msg_type    TEXT NOT NULL,
    content     JSONB NOT NULL,
    tags        TEXT[],
    PRIMARY KEY (ctx, seq)
);

CREATE TABLE IF NOT EXISTS events (
    event_id        BIGSERIAL PRIMARY KEY,
    ctx             TEXT NOT NULL,
    start_seq       BIGINT NOT NULL,
    end_seq         BIGINT NOT NULL,
    boundary_reason TEXT NOT NULL,
    cues_agg        TEXT[] NOT NULL,
    outcome_status  TEXT,
    outcome_duration_ms BIGINT,
    activation_strength REAL DEFAULT 0.5,
    context_inhibition  REAL DEFAULT 0.0,
    use_count       INT DEFAULT 0,
    success_count   INT DEFAULT 0,
    last_used_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE IF NOT EXISTS memory_items (
    memory_id       TEXT PRIMARY KEY,
    event_id        BIGINT REFERENCES events(event_id),
    item_type       TEXT NOT NULL,
    content         JSONB NOT NULL,
    confidence      REAL NOT NULL,
    cues_signature  TEXT[] NOT NULL,
    activation_strength REAL DEFAULT 0.5,
    created_at      TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE IF NOT EXISTS storage_inbox (
    dedupe_key  TEXT PRIMARY KEY,
    subject     TEXT NOT NULL,
    payload     BYTEA NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts    INT NOT NULL DEFAULT 0,
    processed_at TIMESTAMPTZ,
    last_error  TEXT
);

CREATE TABLE IF NOT EXISTS storage_reactivation_applied (
    dedupe_key  TEXT NOT NULL,
    event_id    BIGINT NOT NULL,
    used        BOOLEAN NOT NULL,
    outcome_ok  BOOLEAN NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (dedupe_key, event_id)
);

CREATE INDEX IF NOT EXISTS idx_turns_ctx ON turns (ctx);
CREATE INDEX IF NOT EXISTS idx_events_cues ON events USING GIN(cues_agg);
CREATE INDEX IF NOT EXISTS idx_events_activation ON events(activation_strength DESC);
CREATE INDEX IF NOT EXISTS idx_events_natural_key ON events(ctx, start_seq, end_seq, boundary_reason);
CREATE INDEX IF NOT EXISTS idx_items_cues ON memory_items USING GIN(cues_signature);
CREATE INDEX IF NOT EXISTS idx_storage_inbox_pending ON storage_inbox(processed_at, received_at);
"#,
            )
            .await?;
        Ok(())
    }

    async fn replay_pending_messages(
        &self,
        batch_size: i64,
        max_rounds: u32,
    ) -> Result<u64, StorageError> {
        let mut replayed = 0u64;
        for _ in 0..max_rounds {
            let rows = self
                .client
                .query(
                    r#"
SELECT dedupe_key, subject, payload
FROM storage_inbox
WHERE processed_at IS NULL
ORDER BY received_at ASC
LIMIT $1
"#,
                    &[&batch_size],
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
                    .process_inbox_message(&dedupe_key, &subject, kind, &payload)
                    .await
                {
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

    async fn inbox_metrics_snapshot(&self) -> Result<StorageInboxMetricsSnapshot, StorageError> {
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
        Ok(StorageInboxMetricsSnapshot {
            pending: row.get::<_, i64>(0),
            pending_with_error: row.get::<_, i64>(1),
            oldest_pending_age_s: row.get::<_, i64>(2),
            processed_total: row.get::<_, i64>(3),
            max_attempts: row.get::<_, i32>(4),
        })
    }

    async fn ingest_message(
        &self,
        subject: &str,
        kind: HandlerKind,
        payload: &[u8],
    ) -> Result<(), StorageError> {
        let dedupe_key = message_dedupe_key(subject, payload);
        let already_processed = self
            .upsert_inbox_message(&dedupe_key, subject, payload)
            .await?;
        if already_processed {
            return Ok(());
        }
        self.process_inbox_message(&dedupe_key, subject, kind, payload)
            .await
    }

    async fn upsert_inbox_message(
        &self,
        dedupe_key: &str,
        subject: &str,
        payload: &[u8],
    ) -> Result<bool, StorageError> {
        let row = self
            .client
            .query_one(
                r#"
INSERT INTO storage_inbox (dedupe_key, subject, payload, attempts, last_error)
VALUES ($1, $2, $3, 1, NULL)
ON CONFLICT (dedupe_key) DO UPDATE SET
    attempts = storage_inbox.attempts + 1,
    last_error = NULL
RETURNING processed_at IS NOT NULL
"#,
                &[&dedupe_key, &subject, &payload],
            )
            .await?;
        Ok(row.get::<_, bool>(0))
    }

    async fn process_inbox_message(
        &self,
        dedupe_key: &str,
        subject: &str,
        kind: HandlerKind,
        payload: &[u8],
    ) -> Result<(), StorageError> {
        let result = self
            .process_subject_payload(kind, payload, dedupe_key)
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
    ) -> Result<(), StorageError> {
        match kind {
            HandlerKind::Turns => {
                let value: Value = serde_json::from_slice(payload)?;
                let turn = parse_turn(value)?;
                self.persist_turn(&self.client, &turn).await
            }
            HandlerKind::Events => {
                let value: Value = serde_json::from_slice(payload)?;
                let event = parse_event(value)?;
                self.persist_event(&self.client, &event).await
            }
            HandlerKind::Items => {
                let value: Value = serde_json::from_slice(payload)?;
                let item = parse_memory_item(value)?;
                self.persist_item(&self.client, &item).await
            }
            HandlerKind::Reactivation => {
                let value: Value = serde_json::from_slice(payload)?;
                let parsed = parse_reactivation_payload(value)?;
                self.apply_reactivation(&self.client, dedupe_key, &parsed)
                    .await
            }
        }
    }

    async fn persist_turn<C>(&self, db: &C, turn: &TurnRecord) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        db.execute(
            r#"
INSERT INTO turns (ctx, seq, ts, from_ilk, to_ilk, ich, msg_type, content, tags)
VALUES ($1, $2, now(), $3, $4, $5, $6, $7, $8)
ON CONFLICT (ctx, seq) DO NOTHING
"#,
            &[
                &turn.ctx,
                &turn.seq,
                &turn.from_ilk,
                &turn.to_ilk,
                &turn.ich,
                &turn.msg_type,
                &turn.content,
                &turn.tags,
            ],
        )
        .await?;
        Ok(())
    }

    async fn persist_event<C>(&self, db: &C, event: &EventRecord) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        let activation_strength = event.activation_strength as f32;
        let context_inhibition = event.context_inhibition as f32;
        if let Some(event_id) = event.event_id {
            db.execute(
                r#"
INSERT INTO events (
    event_id, ctx, start_seq, end_seq, boundary_reason, cues_agg,
    outcome_status, outcome_duration_ms, activation_strength, context_inhibition,
    use_count, success_count
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
ON CONFLICT (event_id) DO UPDATE SET
    ctx = EXCLUDED.ctx,
    start_seq = EXCLUDED.start_seq,
    end_seq = EXCLUDED.end_seq,
    boundary_reason = EXCLUDED.boundary_reason,
    cues_agg = EXCLUDED.cues_agg,
    outcome_status = EXCLUDED.outcome_status,
    outcome_duration_ms = EXCLUDED.outcome_duration_ms,
    activation_strength = EXCLUDED.activation_strength,
    context_inhibition = EXCLUDED.context_inhibition,
    use_count = EXCLUDED.use_count,
    success_count = EXCLUDED.success_count
"#,
                &[
                    &event_id,
                    &event.ctx,
                    &event.start_seq,
                    &event.end_seq,
                    &event.boundary_reason,
                    &event.cues_agg,
                    &event.outcome_status,
                    &event.outcome_duration_ms,
                    &activation_strength,
                    &context_inhibition,
                    &event.use_count,
                    &event.success_count,
                ],
            )
            .await?;
        } else {
            db.execute(
                r#"
WITH updated AS (
    UPDATE events
    SET
        cues_agg = $5,
        outcome_status = $6,
        outcome_duration_ms = $7,
        activation_strength = $8,
        context_inhibition = $9,
        use_count = $10,
        success_count = $11
    WHERE
        ctx = $1
        AND start_seq = $2
        AND end_seq = $3
        AND boundary_reason = $4
    RETURNING event_id
)
INSERT INTO events (
    ctx, start_seq, end_seq, boundary_reason, cues_agg,
    outcome_status, outcome_duration_ms, activation_strength, context_inhibition,
    use_count, success_count
)
SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11
WHERE NOT EXISTS (SELECT 1 FROM updated)
"#,
                &[
                    &event.ctx,
                    &event.start_seq,
                    &event.end_seq,
                    &event.boundary_reason,
                    &event.cues_agg,
                    &event.outcome_status,
                    &event.outcome_duration_ms,
                    &activation_strength,
                    &context_inhibition,
                    &event.use_count,
                    &event.success_count,
                ],
            )
            .await?;
        }
        Ok(())
    }

    async fn persist_item<C>(&self, db: &C, item: &MemoryItemRecord) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        let confidence = item.confidence as f32;
        let activation_strength = item.activation_strength as f32;
        db.execute(
            r#"
INSERT INTO memory_items (
    memory_id, event_id, item_type, content, confidence, cues_signature, activation_strength
)
VALUES ($1,$2,$3,$4,$5,$6,$7)
ON CONFLICT (memory_id) DO NOTHING
"#,
            &[
                &item.memory_id,
                &item.event_id,
                &item.item_type,
                &item.content,
                &confidence,
                &item.cues_signature,
                &activation_strength,
            ],
        )
        .await?;
        Ok(())
    }

    async fn apply_reactivation<C>(
        &self,
        db: &C,
        dedupe_key: &str,
        parsed: &ReactivationPayload,
    ) -> Result<(), StorageError>
    where
        C: GenericClient + Sync,
    {
        for ev in &parsed.events {
            db.execute(
                r#"
WITH marker AS (
    INSERT INTO storage_reactivation_applied (dedupe_key, event_id, used, outcome_ok)
    VALUES ($1, $2, $3, $4)
    ON CONFLICT (dedupe_key, event_id) DO NOTHING
    RETURNING 1
)
UPDATE events SET
    activation_strength = CASE
        WHEN $3 THEN LEAST(COALESCE(activation_strength, 0.5) * 1.1, 1.0)
        ELSE GREATEST(COALESCE(activation_strength, 0.5) * 0.95, 0.1)
    END,
    use_count = COALESCE(use_count, 0) + CASE WHEN $3 THEN 1 ELSE 0 END,
    success_count = COALESCE(success_count, 0) + CASE WHEN $3 AND $4 THEN 1 ELSE 0 END,
    last_used_at = CASE WHEN $3 THEN now() ELSE last_used_at END
WHERE event_id = $2
  AND EXISTS (SELECT 1 FROM marker)
"#,
                &[&dedupe_key, &ev.event_id, &ev.used, &parsed.outcome_ok],
            )
            .await?;
        }
        Ok(())
    }
}

async fn run_turns_loop(
    endpoint: String,
    use_durable_consumer: bool,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_TURNS,
        10,
        use_durable_consumer.then_some(DURABLE_QUEUE_TURNS),
        storage,
        HandlerKind::Turns,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

async fn run_events_loop(
    endpoint: String,
    use_durable_consumer: bool,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_EVENTS,
        11,
        use_durable_consumer.then_some(DURABLE_QUEUE_EVENTS),
        storage,
        HandlerKind::Events,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

async fn run_items_loop(
    endpoint: String,
    use_durable_consumer: bool,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_ITEMS,
        12,
        use_durable_consumer.then_some(DURABLE_QUEUE_ITEMS),
        storage,
        HandlerKind::Items,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

async fn run_reactivation_loop(
    endpoint: String,
    use_durable_consumer: bool,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_REACTIVATION,
        13,
        use_durable_consumer.then_some(DURABLE_QUEUE_REACTIVATION),
        storage,
        HandlerKind::Reactivation,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

#[derive(Clone, Copy)]
enum HandlerKind {
    Turns,
    Events,
    Items,
    Reactivation,
}

struct InboxMetrics {
    pending: i64,
    pending_with_error: i64,
    oldest_pending_age_s: i64,
}

fn handler_kind_for_subject(subject: &str) -> Option<HandlerKind> {
    match subject {
        SUBJECT_STORAGE_TURNS => Some(HandlerKind::Turns),
        SUBJECT_STORAGE_EVENTS => Some(HandlerKind::Events),
        SUBJECT_STORAGE_ITEMS => Some(HandlerKind::Items),
        SUBJECT_STORAGE_REACTIVATION => Some(HandlerKind::Reactivation),
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

fn truncate_error(input: &str, max_len: usize) -> String {
    if input.len() <= max_len {
        return input.to_string();
    }
    input.chars().take(max_len).collect()
}

async fn run_subject_loop(
    endpoint: String,
    subject: &str,
    sid: u32,
    durable_queue: Option<&'static str>,
    storage: Arc<Storage>,
    kind: HandlerKind,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    loop {
        let subscriber = if let Some(queue) = durable_queue {
            RouterNatsSubscriber::new(endpoint.clone(), subject.to_string(), sid).with_queue(queue)
        } else {
            RouterNatsSubscriber::new(endpoint.clone(), subject.to_string(), sid)
        };
        let worker = Arc::clone(&storage);
        let subject_name = subject.to_string();
        let nats_subscribe_errors = Arc::clone(&nats_subscribe_errors);
        let storage_handler_errors = Arc::clone(&storage_handler_errors);
        let run_result = subscriber
            .run(move |payload| {
                let worker = Arc::clone(&worker);
                let subject_name = subject_name.clone();
                let storage_handler_errors = Arc::clone(&storage_handler_errors);
                async move {
                    let result = worker.ingest_message(&subject_name, kind, &payload).await;
                    if let Err(err) = result {
                        let count = storage_handler_errors.fetch_add(1, Ordering::Relaxed) + 1;
                        if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                            tracing::warn!(
                                subject = %subject_name,
                                error = %err,
                                failures = count,
                                "storage handler failed"
                            );
                        }
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            err.to_string(),
                        ));
                    }
                    Ok(())
                }
            })
            .await;
        if let Err(err) = run_result {
            let count = nats_subscribe_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                tracing::warn!(
                    subject = %subject,
                    error = %err,
                    failures = count,
                    "nats subscribe loop failed; retrying"
                );
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn run_storage_metrics_query_loop(
    config_dir: PathBuf,
    storage: Option<Arc<Storage>>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    loop {
        let subscriber = match subscribe_local(
            &config_dir,
            SUBJECT_STORAGE_METRICS_GET.to_string(),
            STORAGE_METRICS_QUERY_SID,
        ) {
            Ok(subscriber) => subscriber,
            Err(err) => {
                let count = nats_subscribe_errors.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                    tracing::warn!(
                        subject = SUBJECT_STORAGE_METRICS_GET,
                        error = %err,
                        failures = count,
                        "storage metrics nats subscribe setup failed; retrying"
                    );
                }
                time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let storage = storage.clone();
        let config_dir_out = config_dir.clone();
        let nats_subscribe_errors = Arc::clone(&nats_subscribe_errors);
        let storage_handler_errors = Arc::clone(&storage_handler_errors);
        let run_result = subscriber
            .run(move |msg| {
                let storage = storage.clone();
                let config_dir_out = config_dir_out.clone();
                let storage_handler_errors = Arc::clone(&storage_handler_errors);
                async move {
                    tracing::info!(
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        message_subject = %msg.subject,
                        sid = %msg.sid,
                        reply_to = ?msg.reply_to,
                        payload_bytes = msg.payload.len(),
                        "storage metrics nats raw message received"
                    );
                    let req: NatsRequestEnvelope<Value> = match serde_json::from_slice(&msg.payload)
                    {
                        Ok(req) => req,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                subject = SUBJECT_STORAGE_METRICS_GET,
                                payload_bytes = msg.payload.len(),
                                "invalid storage metrics request payload"
                            );
                            return Ok(());
                        }
                    };
                    if req.schema_version != NATS_ENVELOPE_SCHEMA_VERSION {
                        tracing::warn!(
                            trace_id = %req.trace_id,
                            subject = SUBJECT_STORAGE_METRICS_GET,
                            schema_version = req.schema_version,
                            expected_schema_version = NATS_ENVELOPE_SCHEMA_VERSION,
                            "storage metrics request rejected due to schema version mismatch"
                        );
                        if !req.reply_subject.trim().is_empty() {
                            let response =
                                NatsResponseEnvelope::<StorageInboxMetricsSnapshot>::error(
                                    SUBJECT_STORAGE_METRICS_GET,
                                    req.trace_id.clone(),
                                    "UNSUPPORTED_SCHEMA_VERSION",
                                    format!(
                                        "unsupported schema_version={} expected={}",
                                        req.schema_version, NATS_ENVELOPE_SCHEMA_VERSION
                                    ),
                                );
                            if let Ok(body) = serde_json::to_vec(&response) {
                                let _ = client_nats_publish_local(
                                    &config_dir_out,
                                    &req.reply_subject,
                                    &body,
                                )
                                .await;
                            }
                        }
                        return Ok(());
                    }
                    if req.action != SUBJECT_STORAGE_METRICS_GET {
                        tracing::warn!(
                            trace_id = %req.trace_id,
                            subject = SUBJECT_STORAGE_METRICS_GET,
                            action = %req.action,
                            "storage metrics request rejected due to action mismatch"
                        );
                        if !req.reply_subject.trim().is_empty() {
                            let response =
                                NatsResponseEnvelope::<StorageInboxMetricsSnapshot>::error(
                                    SUBJECT_STORAGE_METRICS_GET,
                                    req.trace_id.clone(),
                                    "ACTION_MISMATCH",
                                    format!(
                                        "action mismatch expected={} got={}",
                                        SUBJECT_STORAGE_METRICS_GET, req.action
                                    ),
                                );
                            if let Ok(body) = serde_json::to_vec(&response) {
                                let _ = client_nats_publish_local(
                                    &config_dir_out,
                                    &req.reply_subject,
                                    &body,
                                )
                                .await;
                            }
                        }
                        return Ok(());
                    }
                    if req.reply_subject.trim().is_empty() {
                        tracing::warn!(
                            subject = SUBJECT_STORAGE_METRICS_GET,
                            "storage metrics request missing reply_subject"
                        );
                        return Ok(());
                    }
                    let request_started = Instant::now();
                    let trace_id = req.trace_id.clone();
                    let reply_subject = req.reply_subject.clone();
                    tracing::info!(
                        trace_id = %trace_id,
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        reply_subject = %reply_subject,
                        payload_bytes = msg.payload.len(),
                        "storage metrics nats request received"
                    );

                    let db_started = Instant::now();
                    tracing::debug!(
                        trace_id = %trace_id,
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        reply_subject = %reply_subject,
                        "storage metrics db snapshot start"
                    );
                    let (response, response_status) = match storage.as_ref() {
                        Some(storage) => match storage.inbox_metrics_snapshot().await {
                            Ok(metrics) => {
                                tracing::debug!(
                                    trace_id = %trace_id,
                                    request_subject = SUBJECT_STORAGE_METRICS_GET,
                                    reply_subject = %reply_subject,
                                    pending = metrics.pending,
                                    pending_with_error = metrics.pending_with_error,
                                    oldest_pending_age_s = metrics.oldest_pending_age_s,
                                    max_attempts = metrics.max_attempts,
                                    processed_total = metrics.processed_total,
                                    "storage metrics db snapshot success"
                                );
                                (
                                    NatsResponseEnvelope::ok(
                                        SUBJECT_STORAGE_METRICS_GET,
                                        trace_id.clone(),
                                        metrics,
                                    ),
                                    "ok",
                                )
                            }
                            Err(err) => {
                                tracing::warn!(
                                    trace_id = %trace_id,
                                    request_subject = SUBJECT_STORAGE_METRICS_GET,
                                    reply_subject = %reply_subject,
                                    error = %err,
                                    "storage metrics db snapshot failed"
                                );
                                (
                                    NatsResponseEnvelope::<StorageInboxMetricsSnapshot>::error(
                                        SUBJECT_STORAGE_METRICS_GET,
                                        trace_id.clone(),
                                        "STORAGE_METRICS_UNAVAILABLE",
                                        err.to_string(),
                                    ),
                                    "error",
                                )
                            }
                        },
                        None => (
                            NatsResponseEnvelope::<StorageInboxMetricsSnapshot>::error(
                                SUBJECT_STORAGE_METRICS_GET,
                                trace_id.clone(),
                                "STORAGE_NOT_READY",
                                "storage backend not initialized; configure DB secret and restart sy-storage".to_string(),
                            ),
                            "error",
                        ),
                    };
                    let db_elapsed_ms = db_started.elapsed().as_millis() as u64;
                    tracing::info!(
                        trace_id = %trace_id,
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        reply_subject = %reply_subject,
                        status = response_status,
                        db_elapsed_ms = db_elapsed_ms,
                        "storage metrics db snapshot finished"
                    );
                    let body = serde_json::to_vec(&response)
                        .map_err(|err| ClientNatsError::Protocol(err.to_string()))?;
                    tracing::debug!(
                        trace_id = %trace_id,
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        reply_subject = %reply_subject,
                        response_bytes = body.len(),
                        status = response_status,
                        "storage metrics response serialized"
                    );
                    let publish_started = Instant::now();
                    tracing::debug!(
                        trace_id = %trace_id,
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        reply_subject = %reply_subject,
                        response_bytes = body.len(),
                        "storage metrics response publish start"
                    );
                    if let Err(err) =
                        client_nats_publish_local(&config_dir_out, &reply_subject, &body).await
                    {
                        let count = storage_handler_errors.fetch_add(1, Ordering::Relaxed) + 1;
                        let publish_elapsed_ms = publish_started.elapsed().as_millis() as u64;
                        let total_elapsed_ms = request_started.elapsed().as_millis() as u64;
                        if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                            tracing::warn!(
                                trace_id = %trace_id,
                                error = %err,
                                failures = count,
                                request_subject = SUBJECT_STORAGE_METRICS_GET,
                                reply_subject = %reply_subject,
                                db_elapsed_ms = db_elapsed_ms,
                                publish_elapsed_ms = publish_elapsed_ms,
                                total_elapsed_ms = total_elapsed_ms,
                                subject = SUBJECT_STORAGE_METRICS_GET,
                                "storage metrics response publish failed"
                            );
                        }
                        return Err(err);
                    }
                    let publish_elapsed_ms = publish_started.elapsed().as_millis() as u64;
                    let total_elapsed_ms = request_started.elapsed().as_millis() as u64;
                    tracing::debug!(
                        trace_id = %trace_id,
                        request_subject = SUBJECT_STORAGE_METRICS_GET,
                        reply_subject = %reply_subject,
                        status = response_status,
                        db_elapsed_ms = db_elapsed_ms,
                        publish_elapsed_ms = publish_elapsed_ms,
                        total_elapsed_ms = total_elapsed_ms,
                        "storage metrics nats response sent"
                    );
                    Ok(())
                }
            })
            .await;

        if let Err(err) = run_result {
            let count = nats_subscribe_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                tracing::warn!(
                    subject = SUBJECT_STORAGE_METRICS_GET,
                    error = %err,
                    failures = count,
                    "storage metrics nats loop failed; retrying"
                );
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn run_storage_metrics_loop(
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    let mut ticker = time::interval(Duration::from_secs(STORAGE_METRICS_LOG_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        match storage.inbox_metrics().await {
            Ok(metrics) => {
                let subscribe_failures = nats_subscribe_errors.load(Ordering::Relaxed);
                let handler_failures = storage_handler_errors.load(Ordering::Relaxed);
                if metrics.pending >= STORAGE_METRICS_WARN_PENDING
                    || metrics.oldest_pending_age_s >= STORAGE_METRICS_WARN_AGE_SECS
                    || metrics.pending_with_error > 0
                {
                    tracing::warn!(
                        pending = metrics.pending,
                        pending_with_error = metrics.pending_with_error,
                        oldest_pending_age_s = metrics.oldest_pending_age_s,
                        nats_subscribe_failures = subscribe_failures,
                        storage_handler_failures = handler_failures,
                        "storage inbox lag is above threshold"
                    );
                } else {
                    tracing::debug!(
                        pending = metrics.pending,
                        pending_with_error = metrics.pending_with_error,
                        oldest_pending_age_s = metrics.oldest_pending_age_s,
                        nats_subscribe_failures = subscribe_failures,
                        storage_handler_failures = handler_failures,
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

async fn load_hive(config_dir: &Path) -> Result<HiveFile, StorageError> {
    let data = tokio::fs::read_to_string(config_dir.join("hive.yaml")).await?;
    Ok(serde_yaml::from_str(&data)?)
}

fn resolve_database_url(
    _hive: &HiveFile,
    node_name: &str,
) -> (Option<String>, StorageDbSecretSource) {
    if let Some(url) = load_local_postgres_url(node_name) {
        return (Some(url), StorageDbSecretSource::LocalFile);
    }
    if let Ok(url) = std::env::var("FLUXBEE_DATABASE_URL") {
        if !url.trim().is_empty() {
            return (Some(url), StorageDbSecretSource::EnvCompat);
        }
    }
    if let Ok(url) = std::env::var("JSR_DATABASE_URL") {
        if !url.trim().is_empty() {
            return (Some(url), StorageDbSecretSource::EnvCompat);
        }
    }
    (None, StorageDbSecretSource::Missing)
}

fn database_config_from_url(url: &str) -> Result<PgConfig, StorageError> {
    Ok(url.parse::<PgConfig>()?)
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
        tracing::warn!(
            node_name = %node_name,
            error = %err,
            "failed to ensure storage database exists; starting without DB backend"
        );
        return None;
    }
    let storage_database_config = with_dbname(&base_database_config, STORAGE_DB_NAME);
    let storage = match Storage::connect(&storage_database_config).await {
        Ok(storage) => Arc::new(storage),
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "failed to connect SY.storage DB backend; starting without DB backend"
            );
            return None;
        }
    };
    if let Err(err) = storage.ensure_schema().await {
        tracing::warn!(
            node_name = %node_name,
            error = %err,
            "failed to ensure SY.storage schema; starting without DB backend"
        );
        return None;
    }
    Some(storage)
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
    Ok(TurnRecord {
        ctx,
        seq,
        from_ilk,
        to_ilk,
        ich,
        msg_type,
        content,
        tags,
    })
}

fn parse_event(value: Value) -> Result<EventRecord, StorageError> {
    let event_root = if value.get("event").is_some() {
        value.get("event").cloned().unwrap_or(Value::Null)
    } else {
        value
    };
    if !event_root.is_object() {
        return Err(missing_field("event"));
    }

    let ctx = required_str(&event_root, &["ctx"], "ctx")?;
    let start_seq = required_i64(&event_root, &["start_seq"], "start_seq")?;
    let end_seq = required_i64(&event_root, &["end_seq"], "end_seq")?;
    if end_seq < start_seq {
        return Err("invalid payload: end_seq < start_seq".into());
    }
    let boundary_reason = required_str(&event_root, &["boundary_reason"], "boundary_reason")?;

    let mut cues_agg = get_string_vec(&event_root, &["cues_agg"])
        .or_else(|| get_string_vec(&event_root, &["tags"]))
        .unwrap_or_default();
    if cues_agg.is_empty() {
        return Err(missing_field("cues_agg|tags"));
    }
    dedup_sort(&mut cues_agg);

    Ok(EventRecord {
        event_id: get_i64(&event_root, &["event_id"]),
        ctx,
        start_seq,
        end_seq,
        boundary_reason,
        cues_agg,
        outcome_status: get_str(&event_root, &["outcome_status"]),
        outcome_duration_ms: get_i64(&event_root, &["outcome_duration_ms"]),
        activation_strength: get_f64(&event_root, &["activation_strength"]).unwrap_or(0.5),
        context_inhibition: get_f64(&event_root, &["context_inhibition"]).unwrap_or(0.0),
        use_count: get_i64(&event_root, &["use_count"]).unwrap_or(0) as i32,
        success_count: get_i64(&event_root, &["success_count"]).unwrap_or(0) as i32,
    })
}

fn parse_memory_item(value: Value) -> Result<MemoryItemRecord, StorageError> {
    let item_root = if value.get("item").is_some() {
        value.get("item").cloned().unwrap_or(Value::Null)
    } else {
        value
    };
    if !item_root.is_object() {
        return Err(missing_field("item"));
    }

    let content = required_value(&item_root, &["content"], "content")?.clone();
    let item_type = required_str(&item_root, &["item_type"], "item_type")?;
    let confidence = required_f64(&item_root, &["confidence"], "confidence")?;

    let mut cues_signature = get_string_vec(&item_root, &["cues_signature"])
        .or_else(|| get_string_vec(&item_root, &["tags"]))
        .unwrap_or_default();
    if cues_signature.is_empty() {
        return Err(missing_field("cues_signature|tags"));
    }
    dedup_sort(&mut cues_signature);

    let memory_id = get_str(&item_root, &["memory_id"]).unwrap_or_else(|| {
        let canonical = serde_json::to_string(&item_root).unwrap_or_else(|_| "item".to_string());
        format!("mid-{:x}", stable_u64(&canonical))
    });

    Ok(MemoryItemRecord {
        memory_id,
        event_id: get_i64(&item_root, &["event_id"]),
        item_type,
        content,
        confidence,
        cues_signature,
        activation_strength: get_f64(&item_root, &["activation_strength"]).unwrap_or(0.5),
    })
}

#[derive(Debug)]
struct ReactivationEventUse {
    event_id: i64,
    used: bool,
}

#[derive(Debug)]
struct ReactivationPayload {
    outcome_ok: bool,
    events: Vec<ReactivationEventUse>,
}

fn parse_reactivation_payload(value: Value) -> Result<ReactivationPayload, StorageError> {
    let outcome_ok =
        get_str(&value, &["outcome", "status"]).is_some_and(|s| s.eq_ignore_ascii_case("resolved"));

    let events = get_array(&value, &["reactivated", "events"])
        .or_else(|| get_array(&value, &["events"]))
        .ok_or_else(|| missing_field("reactivated.events|events"))?;

    let mut parsed = Vec::with_capacity(events.len());
    for (idx, ev) in events.iter().enumerate() {
        let field = format!("events[{idx}].event_id");
        let event_id = required_i64(ev, &["event_id"], &field)?;
        let used = get_bool(ev, &["used"]).unwrap_or(false);
        parsed.push(ReactivationEventUse { event_id, used });
    }

    Ok(ReactivationPayload {
        outcome_ok,
        events: parsed,
    })
}

fn missing_field(field: &str) -> StorageError {
    format!("invalid payload: missing or invalid {field}").into()
}

fn required_value<'a>(
    value: &'a Value,
    path: &[&str],
    field: &str,
) -> Result<&'a Value, StorageError> {
    get_value(value, path).ok_or_else(|| missing_field(field))
}

fn required_str(value: &Value, path: &[&str], field: &str) -> Result<String, StorageError> {
    get_str(value, path).ok_or_else(|| missing_field(field))
}

fn required_i64(value: &Value, path: &[&str], field: &str) -> Result<i64, StorageError> {
    get_i64(value, path).ok_or_else(|| missing_field(field))
}

fn required_f64(value: &Value, path: &[&str], field: &str) -> Result<f64, StorageError> {
    get_f64(value, path).ok_or_else(|| missing_field(field))
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

fn get_f64(value: &Value, path: &[&str]) -> Option<f64> {
    let v = get_value(value, path)?;
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    v.as_str()?.parse::<f64>().ok()
}

fn get_bool(value: &Value, path: &[&str]) -> Option<bool> {
    let v = get_value(value, path)?;
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    let s = v.as_str()?.trim().to_ascii_lowercase();
    match s.as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
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

fn get_array<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Vec<Value>> {
    get_value(value, path)?.as_array()
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

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!(error = %err, "sy.storage router connect failed; retrying");
                time::sleep(delay).await;
            }
        }
    }
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
    if msg.meta.msg_type != SYSTEM_KIND {
        return Ok(());
    }
    let Some(command) = msg.meta.msg.as_deref() else {
        return Ok(());
    };
    match command {
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
        },
        payload,
    }
}

fn bootstrap_storage_control_state(
    node_name: &str,
    secret_source: StorageDbSecretSource,
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
        config: storage_public_config(state.secret_source),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&path, &serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn storage_public_config(secret_source: StorageDbSecretSource) -> Value {
    json!({
        "database": {
            "mode": "postgres",
            "postgres_url": if secret_source == StorageDbSecretSource::Missing {
                Value::Null
            } else {
                Value::String(NODE_SECRET_REDACTION_TOKEN.to_string())
            },
            "source": secret_source.as_str()
        }
    })
}

fn build_storage_config_get_payload(
    node_name: &str,
    state: &StorageControlState,
    note: Option<&str>,
) -> Value {
    let configured = state.secret_source != StorageDbSecretSource::Missing;
    let mut secret_descriptor = NodeSecretDescriptor::new(
        "config.database.postgres_url",
        STORAGE_LOCAL_SECRET_KEY_POSTGRES_URL,
    );
    secret_descriptor.required = true;
    secret_descriptor.configured = configured;
    secret_descriptor.persistence = state.secret_source.as_str().to_string();
    let mut notes = vec![
        Value::String("SY.storage uses PostgreSQL only on motherbee.".to_string()),
        Value::String(
            "Secret values are persisted in local secrets.json and always returned redacted."
                .to_string(),
        ),
        Value::String(
            "Applying a new DB secret persists locally and requires sy-storage restart to affect live connections."
                .to_string(),
        ),
    ];
    if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
        notes.push(Value::String(note.to_string()));
    }
    json!({
        "ok": configured,
        "node_name": node_name,
        "state": storage_state_label(state.secret_source),
        "schema_version": state.schema_version,
        "config_version": state.config_version,
        "config": storage_public_config(state.secret_source),
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.storage",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": ["config.database.postgres_url"],
            "optional_fields": [],
            "secrets": [secret_descriptor],
            "notes": notes,
        },
        "error": if configured {
            Value::Null
        } else {
            json!({
                "code": "missing_secret",
                "message": "Missing database secret in local secrets.json or env overrides."
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
    let Some(postgres_url) = config
        .get("database")
        .and_then(Value::as_object)
        .and_then(|value| value.get("postgres_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            "config.database.postgres_url is required".to_string(),
        ));
    };
    if let Err(err) = postgres_url.parse::<PgConfig>() {
        return Ok(storage_config_error_response(
            node_name,
            control_state,
            "invalid_config",
            format!("invalid postgres_url: {err}"),
        ));
    }
    persist_local_postgres_url(
        node_name,
        postgres_url,
        &build_secret_write_options_from_message(msg),
    )?;
    control_state.schema_version = schema_version;
    control_state.config_version = config_version;
    control_state.secret_source = StorageDbSecretSource::LocalFile;
    persist_storage_config_state(node_name, control_state)?;
    Ok(json!({
        "ok": true,
        "node_name": node_name,
        "state": storage_state_label(control_state.secret_source),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": storage_public_config(control_state.secret_source),
        "notes": [
            "postgres_url persisted in local secrets.json",
            "restart_required: sy-storage must be restarted to apply new DB connection settings"
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
        "config": storage_public_config(control_state.secret_source),
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

fn persist_local_postgres_url(
    node_name: &str,
    postgres_url: &str,
    options: &NodeSecretWriteOptions,
) -> Result<(), fluxbee_sdk::NodeSecretError> {
    let mut secrets = load_node_secret_record(node_name)
        .map(|record| record.secrets)
        .unwrap_or_else(|_| Map::new());
    secrets.insert(
        STORAGE_LOCAL_SECRET_KEY_POSTGRES_URL.to_string(),
        Value::String(postgres_url.to_string()),
    );
    let record = build_node_secret_record(secrets, options);
    save_node_secret_record(node_name, &record)?;
    Ok(())
}

fn load_local_postgres_url(node_name: &str) -> Option<String> {
    load_node_secret_record(node_name)
        .ok()
        .and_then(|record| {
            record
                .secrets
                .get(STORAGE_LOCAL_SECRET_KEY_POSTGRES_URL)
                .cloned()
        })
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
