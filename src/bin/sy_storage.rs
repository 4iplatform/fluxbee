use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::time;
use tokio_postgres::{Client, NoTls};
use tracing_subscriber::EnvFilter;

use json_router::nats::{
    NatsSubscriber, SUBJECT_STORAGE_EVENTS, SUBJECT_STORAGE_ITEMS, SUBJECT_STORAGE_REACTIVATION,
    SUBJECT_STORAGE_TURNS,
};

type StorageError = Box<dyn std::error::Error + Send + Sync>;

const NATS_ERROR_LOG_EVERY: u64 = 20;

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
    port: Option<u16>,
    url: Option<String>,
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
    if !is_mother_role(hive.role.as_deref()) {
        tracing::warn!(
            hive = %hive.hive_id,
            "SY.storage solo corre en motherbee; role != motherbee"
        );
        return Ok(());
    }

    let endpoint = nats_endpoint(&hive);
    let database_url = database_url(&hive)?;
    let storage = Arc::new(Storage::connect(&database_url).await?);
    storage.ensure_schema().await?;

    tracing::info!(
        hive = %hive.hive_id,
        endpoint = %endpoint,
        "sy.storage started"
    );

    let nats_subscribe_errors = Arc::new(AtomicU64::new(0));
    let storage_handler_errors = Arc::new(AtomicU64::new(0));

    let turns_task = tokio::spawn(run_turns_loop(
        endpoint.clone(),
        Arc::clone(&storage),
        Arc::clone(&nats_subscribe_errors),
        Arc::clone(&storage_handler_errors),
    ));
    let events_task = tokio::spawn(run_events_loop(
        endpoint.clone(),
        Arc::clone(&storage),
        Arc::clone(&nats_subscribe_errors),
        Arc::clone(&storage_handler_errors),
    ));
    let items_task = tokio::spawn(run_items_loop(
        endpoint.clone(),
        Arc::clone(&storage),
        Arc::clone(&nats_subscribe_errors),
        Arc::clone(&storage_handler_errors),
    ));
    let react_task = tokio::spawn(run_reactivation_loop(
        endpoint,
        Arc::clone(&storage),
        Arc::clone(&nats_subscribe_errors),
        Arc::clone(&storage_handler_errors),
    ));

    let _ = tokio::join!(turns_task, events_task, items_task, react_task);
    Ok(())
}

impl Storage {
    async fn connect(database_url: &str) -> Result<Self, StorageError> {
        let (client, connection) = tokio_postgres::connect(database_url, NoTls).await?;
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

CREATE INDEX IF NOT EXISTS idx_turns_ctx ON turns (ctx);
CREATE INDEX IF NOT EXISTS idx_events_cues ON events USING GIN(cues_agg);
CREATE INDEX IF NOT EXISTS idx_events_activation ON events(activation_strength DESC);
CREATE INDEX IF NOT EXISTS idx_items_cues ON memory_items USING GIN(cues_signature);
"#,
            )
            .await?;
        Ok(())
    }

    async fn handle_turn(&self, payload: &[u8]) -> Result<(), StorageError> {
        let value: Value = serde_json::from_slice(payload)?;
        let turn = parse_turn(value)?;
        self.client
            .execute(
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

    async fn handle_event(&self, payload: &[u8]) -> Result<(), StorageError> {
        let value: Value = serde_json::from_slice(payload)?;
        let event = parse_event(value)?;
        if let Some(event_id) = event.event_id {
            self.client
                .execute(
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
                        &event.activation_strength,
                        &event.context_inhibition,
                        &event.use_count,
                        &event.success_count,
                    ],
                )
                .await?;
        } else {
            self.client
                .execute(
                    r#"
INSERT INTO events (
    ctx, start_seq, end_seq, boundary_reason, cues_agg,
    outcome_status, outcome_duration_ms, activation_strength, context_inhibition,
    use_count, success_count
)
VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
"#,
                    &[
                        &event.ctx,
                        &event.start_seq,
                        &event.end_seq,
                        &event.boundary_reason,
                        &event.cues_agg,
                        &event.outcome_status,
                        &event.outcome_duration_ms,
                        &event.activation_strength,
                        &event.context_inhibition,
                        &event.use_count,
                        &event.success_count,
                    ],
                )
                .await?;
        }
        Ok(())
    }

    async fn handle_item(&self, payload: &[u8]) -> Result<(), StorageError> {
        let value: Value = serde_json::from_slice(payload)?;
        let item = parse_memory_item(value)?;
        self.client
            .execute(
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
                    &item.confidence,
                    &item.cues_signature,
                    &item.activation_strength,
                ],
            )
            .await?;
        Ok(())
    }

    async fn handle_reactivation(&self, payload: &[u8]) -> Result<(), StorageError> {
        let value: Value = serde_json::from_slice(payload)?;
        let parsed = parse_reactivation_payload(value)?;

        for ev in parsed.events {
            if ev.used {
                self.client
                    .execute(
                        r#"
UPDATE events SET
    activation_strength = LEAST(COALESCE(activation_strength, 0.5) * 1.1, 1.0),
    use_count = COALESCE(use_count, 0) + 1,
    success_count = COALESCE(success_count, 0) + CASE WHEN $2 THEN 1 ELSE 0 END,
    last_used_at = now()
WHERE event_id = $1
"#,
                        &[&ev.event_id, &parsed.outcome_ok],
                    )
                    .await?;
            } else {
                self.client
                    .execute(
                        r#"
UPDATE events SET
    activation_strength = GREATEST(COALESCE(activation_strength, 0.5) * 0.95, 0.1)
WHERE event_id = $1
"#,
                        &[&ev.event_id],
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

async fn run_turns_loop(
    endpoint: String,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_TURNS,
        10,
        storage,
        HandlerKind::Turns,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

async fn run_events_loop(
    endpoint: String,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_EVENTS,
        11,
        storage,
        HandlerKind::Events,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

async fn run_items_loop(
    endpoint: String,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_ITEMS,
        12,
        storage,
        HandlerKind::Items,
        nats_subscribe_errors,
        storage_handler_errors,
    )
    .await;
}

async fn run_reactivation_loop(
    endpoint: String,
    storage: Arc<Storage>,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    run_subject_loop(
        endpoint,
        SUBJECT_STORAGE_REACTIVATION,
        13,
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

async fn run_subject_loop(
    endpoint: String,
    subject: &str,
    sid: u32,
    storage: Arc<Storage>,
    kind: HandlerKind,
    nats_subscribe_errors: Arc<AtomicU64>,
    storage_handler_errors: Arc<AtomicU64>,
) {
    loop {
        let subscriber = NatsSubscriber::new(endpoint.clone(), subject.to_string(), sid);
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
                    let result = match kind {
                        HandlerKind::Turns => worker.handle_turn(&payload).await,
                        HandlerKind::Events => worker.handle_event(&payload).await,
                        HandlerKind::Items => worker.handle_item(&payload).await,
                        HandlerKind::Reactivation => worker.handle_reactivation(&payload).await,
                    };
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

async fn load_hive(config_dir: &Path) -> Result<HiveFile, StorageError> {
    let data = tokio::fs::read_to_string(config_dir.join("hive.yaml")).await?;
    Ok(serde_yaml::from_str(&data)?)
}

fn nats_endpoint(hive: &HiveFile) -> String {
    let Some(nats) = hive.nats.as_ref() else {
        return "nats://127.0.0.1:4222".to_string();
    };
    if let Some(url) = nats.url.as_ref() {
        let url = url.trim();
        if !url.is_empty() {
            return url.to_string();
        }
    }
    let mode = nats
        .mode
        .as_deref()
        .unwrap_or("embedded")
        .trim()
        .to_ascii_lowercase();
    let port = nats.port.unwrap_or(4222);
    if mode == "embedded" || mode == "client" {
        format!("nats://127.0.0.1:{port}")
    } else {
        "nats://127.0.0.1:4222".to_string()
    }
}

fn database_url(hive: &HiveFile) -> Result<String, StorageError> {
    if let Ok(url) = std::env::var("FLUXBEE_DATABASE_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    if let Ok(url) = std::env::var("JSR_DATABASE_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    let Some(db) = hive.database.as_ref() else {
        return Err("database.url missing in hive.yaml and env".into());
    };
    let Some(url) = db.url.as_ref() else {
        return Err("database.url missing in hive.yaml and env".into());
    };
    if url.trim().is_empty() {
        return Err("database.url empty".into());
    }
    Ok(url.clone())
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
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee" || r == "mother")
}
