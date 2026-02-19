use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{timeout, Instant};

pub const SUBJECT_STORAGE_TURNS: &str = "storage.turns";
pub const SUBJECT_STORAGE_EVENTS: &str = "storage.events";
pub const SUBJECT_STORAGE_ITEMS: &str = "storage.items";
pub const SUBJECT_STORAGE_REACTIVATION: &str = "storage.reactivation";

const CONNECT_LINE: &str = "CONNECT {\"lang\":\"rust\",\"version\":\"0.1\",\"verbose\":false,\"pedantic\":false,\"tls_required\":false}\r\n";
const EMBEDDED_INFO_LINE: &str =
    "INFO {\"server_id\":\"fluxbee-router\",\"server_name\":\"fluxbee-router\",\"version\":\"1.0.0\",\"proto\":1,\"max_payload\":1048576}\r\n";
const EMBEDDED_ACK_SUBJECT_PREFIX: &str = "_JSR.ACK.";
const EMBEDDED_ACK_TIMEOUT_SECS: u64 = 2;
const EMBEDDED_ACK_MAX_ATTEMPTS: u32 = 8;
const EMBEDDED_REDELIVERY_TICK_MS: u64 = 250;
const EMBEDDED_STATE_FLUSH_TICK_MS: u64 = 500;
const DURABLE_QUEUE_PREFIX: &str = "durable.";
const DURABLE_STREAM_MAX_MESSAGES: usize = 50_000;

#[derive(Clone)]
struct EmbeddedSubscriber {
    connection_id: u64,
    subject: String,
    sid: String,
    durable_consumer: Option<String>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

struct PendingDelivery {
    connection_id: u64,
    subject: String,
    sid: String,
    payload: Vec<u8>,
    attempts: u32,
    last_sent_at: Instant,
    durable_consumer: Option<String>,
    stream_seq: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableMessage {
    seq: u64,
    payload: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DurableStream {
    last_seq: u64,
    messages: VecDeque<DurableMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DurableConsumer {
    subject: String,
    durable_name: String,
    acked_seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DurableStateSnapshot {
    streams: HashMap<String, DurableStream>,
    consumers: HashMap<String, DurableConsumer>,
}

#[derive(Default)]
struct EmbeddedBrokerState {
    subscribers: Vec<EmbeddedSubscriber>,
    pending: HashMap<String, PendingDelivery>,
    next_delivery_id: u64,
    acked_count: u64,
    redelivery_count: u64,
    dropped_count: u64,
    durable_streams: HashMap<String, DurableStream>,
    durable_consumers: HashMap<String, DurableConsumer>,
    durable_state_path: Option<PathBuf>,
    durable_dirty: bool,
}

struct EmbeddedBrokerHandle {
    shutdown_tx: oneshot::Sender<()>,
    task: JoinHandle<Result<(), io::Error>>,
    state: Arc<Mutex<EmbeddedBrokerState>>,
}

#[derive(Clone, Debug)]
pub struct EmbeddedBrokerMetrics {
    pub subscribers: usize,
    pub pending_unacked: usize,
    pub acked_count: u64,
    pub redelivery_count: u64,
    pub dropped_count: u64,
}

fn embedded_registry() -> &'static Mutex<HashMap<String, EmbeddedBrokerHandle>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, EmbeddedBrokerHandle>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn start_embedded_broker(endpoint: &str) -> Result<(), io::Error> {
    start_embedded_broker_with_storage(endpoint, None::<&Path>).await
}

pub async fn start_embedded_broker_with_storage<P: AsRef<Path>>(
    endpoint: &str,
    storage_dir: Option<P>,
) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let durable_state_path = storage_dir
        .map(|path| durable_state_path(path.as_ref(), &addr))
        .transpose()?;
    let existing = {
        let mut registry = embedded_registry().lock().await;
        registry.retain(|_, handle| !handle.task.is_finished());
        registry.remove(&addr)
    };

    if let Some(handle) = existing {
        if !handle.task.is_finished() {
            if check_endpoint(endpoint, Duration::from_millis(400))
                .await
                .is_ok()
            {
                let mut registry = embedded_registry().lock().await;
                registry.insert(addr.clone(), handle);
                tracing::debug!(endpoint = %endpoint, "embedded nats broker already running");
                return Ok(());
            }
            tracing::warn!(
                endpoint = %endpoint,
                "embedded nats registry had a non-responsive broker; forcing restart"
            );
            if let Err(err) = stop_embedded_broker_handle(&addr, handle).await {
                tracing::warn!(
                    endpoint = %endpoint,
                    error = %err,
                    "failed to stop stale embedded nats broker handle before restart"
                );
            }
        }
    }

    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse => {
            if check_endpoint(endpoint, Duration::from_secs(2))
                .await
                .is_ok()
            {
                tracing::info!(endpoint = %endpoint, "embedded nats broker already reachable");
                return Ok(());
            }
            return Err(err);
        }
        Err(err) => return Err(err),
    };

    let snapshot = if let Some(path) = durable_state_path.as_ref() {
        load_durable_snapshot(path)?
    } else {
        DurableStateSnapshot::default()
    };
    let state = Arc::new(Mutex::new(EmbeddedBrokerState {
        durable_streams: snapshot.streams,
        durable_consumers: snapshot.consumers,
        durable_state_path: durable_state_path.clone(),
        ..Default::default()
    }));
    let next_connection_id = Arc::new(AtomicU64::new(1));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_embedded_broker(
        listener,
        Arc::clone(&state),
        next_connection_id,
        shutdown_rx,
    ));

    let mut registry = embedded_registry().lock().await;
    registry.insert(
        addr,
        EmbeddedBrokerHandle {
            shutdown_tx,
            task,
            state,
        },
    );

    Ok(())
}

pub async fn stop_embedded_broker(endpoint: &str) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let handle = {
        let mut registry = embedded_registry().lock().await;
        registry.remove(&addr)
    };

    let Some(handle) = handle else {
        return Ok(());
    };
    stop_embedded_broker_handle(&addr, handle).await
}

pub async fn stop_all_embedded_brokers() -> Result<(), io::Error> {
    let handles = {
        let mut registry = embedded_registry().lock().await;
        registry
            .drain()
            .map(|(endpoint, handle)| (endpoint, handle))
            .collect::<Vec<_>>()
    };

    for (endpoint, handle) in handles {
        stop_embedded_broker_handle(&endpoint, handle).await?;
    }

    Ok(())
}

pub async fn embedded_broker_metrics(
    endpoint: &str,
) -> Result<Option<EmbeddedBrokerMetrics>, io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let state = {
        let mut registry = embedded_registry().lock().await;
        registry.retain(|_, handle| !handle.task.is_finished());
        let Some(handle) = registry.get(&addr) else {
            return Ok(None);
        };
        Arc::clone(&handle.state)
    };
    let state = state.lock().await;
    Ok(Some(EmbeddedBrokerMetrics {
        subscribers: state.subscribers.len(),
        pending_unacked: state.pending.len(),
        acked_count: state.acked_count,
        redelivery_count: state.redelivery_count,
        dropped_count: state.dropped_count,
    }))
}

fn sanitize_addr_for_path(addr: &str) -> String {
    addr.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' => c,
            _ => '_',
        })
        .collect()
}

fn durable_state_path(storage_dir: &Path, addr: &str) -> Result<PathBuf, io::Error> {
    fs::create_dir_all(storage_dir)?;
    Ok(storage_dir.join(format!("embedded-js-{}.json", sanitize_addr_for_path(addr))))
}

fn load_durable_snapshot(path: &Path) -> Result<DurableStateSnapshot, io::Error> {
    if !path.exists() {
        return Ok(DurableStateSnapshot::default());
    }
    let raw = fs::read(path)?;
    if raw.is_empty() {
        return Ok(DurableStateSnapshot::default());
    }
    serde_json::from_slice::<DurableStateSnapshot>(&raw).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid durable embedded nats snapshot {}: {err}",
                path.display()
            ),
        )
    })
}

fn save_durable_snapshot(path: &Path, snapshot: &DurableStateSnapshot) -> Result<(), io::Error> {
    let body = serde_json::to_vec(snapshot).map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("serialize durable embedded nats snapshot failed: {err}"),
        )
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, body)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn build_durable_snapshot(state: &EmbeddedBrokerState) -> DurableStateSnapshot {
    DurableStateSnapshot {
        streams: state.durable_streams.clone(),
        consumers: state.durable_consumers.clone(),
    }
}

async fn flush_durable_state_if_dirty(state: &Arc<Mutex<EmbeddedBrokerState>>) {
    let (path, snapshot) = {
        let mut guard = state.lock().await;
        if !guard.durable_dirty {
            return;
        }
        let Some(path) = guard.durable_state_path.clone() else {
            guard.durable_dirty = false;
            return;
        };
        guard.durable_dirty = false;
        (path, build_durable_snapshot(&guard))
    };
    if let Err(err) = save_durable_snapshot(&path, &snapshot) {
        let mut guard = state.lock().await;
        guard.durable_dirty = true;
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "failed flushing embedded durable nats state"
        );
    }
}

async fn flush_durable_state_force(state: &Arc<Mutex<EmbeddedBrokerState>>) {
    let (path, snapshot) = {
        let mut guard = state.lock().await;
        let Some(path) = guard.durable_state_path.clone() else {
            return;
        };
        guard.durable_dirty = false;
        (path, build_durable_snapshot(&guard))
    };
    if let Err(err) = save_durable_snapshot(&path, &snapshot) {
        tracing::warn!(
            path = %path.display(),
            error = %err,
            "failed final flush of embedded durable nats state"
        );
    }
}

async fn stop_embedded_broker_handle(
    endpoint: &str,
    handle: EmbeddedBrokerHandle,
) -> Result<(), io::Error> {
    let EmbeddedBrokerHandle {
        shutdown_tx,
        mut task,
        state: _,
    } = handle;
    let _ = shutdown_tx.send(());

    match timeout(Duration::from_secs(5), &mut task).await {
        Ok(join_result) => match join_result {
            Ok(inner) => inner,
            Err(err) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("embedded nats task join failed for {endpoint}: {err}"),
            )),
        },
        Err(_) => {
            task.abort();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("embedded nats shutdown timeout for {endpoint}"),
            ))
        }
    }
}

async fn run_embedded_broker(
    listener: TcpListener,
    state: Arc<Mutex<EmbeddedBrokerState>>,
    next_connection_id: Arc<AtomicU64>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<(), io::Error> {
    let mut connection_tasks = JoinSet::new();
    let mut redelivery_ticker =
        tokio::time::interval(Duration::from_millis(EMBEDDED_REDELIVERY_TICK_MS));
    let mut flush_ticker =
        tokio::time::interval(Duration::from_millis(EMBEDDED_STATE_FLUSH_TICK_MS));
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::debug!("embedded nats broker shutdown requested");
                break;
            }
            _ = redelivery_ticker.tick() => {
                redeliver_embedded_pending(&state).await;
            }
            _ = flush_ticker.tick() => {
                flush_durable_state_if_dirty(&state).await;
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!(error = %err, "embedded nats accept failed");
                        continue;
                    }
                };

                let connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);
                let state = Arc::clone(&state);
                connection_tasks.spawn(async move {
                    if let Err(err) = handle_embedded_connection(stream, connection_id, state).await {
                        tracing::warn!(
                            connection_id,
                            error = %err,
                            "embedded nats connection closed with error"
                        );
                    }
                });
            }
        }
    }

    {
        let state = state.lock().await;
        tracing::debug!(
            pending = state.pending.len(),
            acks = state.acked_count,
            redeliveries = state.redelivery_count,
            dropped = state.dropped_count,
            "embedded nats broker stopping"
        );
    }
    let drain_deadline = Instant::now() + Duration::from_millis(1200);
    while !connection_tasks.is_empty() && Instant::now() < drain_deadline {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        match timeout(remaining, connection_tasks.join_next()).await {
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(err))) => {
                if !err.is_cancelled() {
                    tracing::debug!(error = %err, "embedded nats connection task join failed");
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    connection_tasks.abort_all();
    while let Some(result) = connection_tasks.join_next().await {
        if let Err(err) = result {
            if !err.is_cancelled() {
                tracing::debug!(error = %err, "embedded nats connection task join failed");
            }
        }
    }
    flush_durable_state_force(&state).await;

    Ok(())
}

#[derive(Clone)]
pub struct NatsPublisher {
    endpoint: String,
    subject: String,
}

impl NatsPublisher {
    pub fn new(endpoint: String, subject: String) -> Self {
        Self { endpoint, subject }
    }

    pub async fn publish(&self, payload: &[u8]) -> Result<(), io::Error> {
        publish(&self.endpoint, &self.subject, payload).await
    }
}

async fn handle_embedded_connection(
    stream: TcpStream,
    connection_id: u64,
    state: Arc<Mutex<EmbeddedBrokerState>>,
) -> Result<(), io::Error> {
    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(EMBEDDED_INFO_LINE.as_bytes()).await?;
    writer_half.flush().await?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            writer_half.write_all(&frame).await?;
            writer_half.flush().await?;
        }
        Ok::<(), io::Error>(())
    });

    let mut reader = BufReader::new(reader_half);
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        if line.starts_with("CONNECT") || line == "PONG" || line.starts_with("+OK") {
            continue;
        }
        if line == "PING" {
            let _ = tx.send(b"PONG\r\n".to_vec());
            continue;
        }
        if line.starts_with("SUB ") {
            let (subject, sid, queue) = parse_sub_command(line)?;
            let durable_consumer =
                queue.and_then(|value| parse_durable_consumer_key(&subject, &value));
            add_embedded_subscriber(
                &state,
                EmbeddedSubscriber {
                    connection_id,
                    subject,
                    sid,
                    durable_consumer,
                    tx: tx.clone(),
                },
            )
            .await;
            continue;
        }
        if line.starts_with("UNSUB ") {
            if let Some(sid) = parse_unsub_command(line) {
                remove_embedded_sid(&state, connection_id, &sid).await;
            }
            continue;
        }
        if line.starts_with("PUB ") {
            let (subject, payload_len) = parse_pub_command(line)?;
            let mut payload = vec![0u8; payload_len + 2];
            reader.read_exact(&mut payload).await?;
            payload.truncate(payload_len);
            if subject.starts_with(EMBEDDED_ACK_SUBJECT_PREFIX) {
                mark_embedded_ack(&state, &subject).await;
            } else {
                publish_embedded_subject(&state, &subject, &payload).await;
            }
            continue;
        }

        let _ = tx.send(b"-ERR 'unsupported command'\r\n".to_vec());
    }

    remove_embedded_connection(&state, connection_id).await;
    drop(tx);
    if let Ok(join_result) = writer_task.await {
        let _ = join_result;
    }
    Ok(())
}

async fn add_embedded_subscriber(
    state: &Arc<Mutex<EmbeddedBrokerState>>,
    subscriber: EmbeddedSubscriber,
) {
    let mut state = state.lock().await;
    if let Some(consumer_key) = subscriber.durable_consumer.clone() {
        state
            .durable_consumers
            .entry(consumer_key.clone())
            .or_insert_with(|| DurableConsumer {
                subject: subscriber.subject.clone(),
                durable_name: consumer_key.clone(),
                acked_seq: 0,
            });
        state.durable_dirty = true;
        state.subscribers.retain(|entry| {
            !entry
                .durable_consumer
                .as_deref()
                .is_some_and(|key| key == consumer_key)
        });
    }
    state.subscribers.push(subscriber);
    if let Some(consumer_key) = state
        .subscribers
        .last()
        .and_then(|entry| entry.durable_consumer.clone())
    {
        let _ = deliver_next_durable_message(&mut state, &consumer_key);
    }
}

async fn remove_embedded_sid(
    state: &Arc<Mutex<EmbeddedBrokerState>>,
    connection_id: u64,
    sid: &str,
) {
    let mut state = state.lock().await;
    state
        .subscribers
        .retain(|entry| !(entry.connection_id == connection_id && entry.sid == sid));
    state
        .pending
        .retain(|_, pending| !(pending.connection_id == connection_id && pending.sid == sid));
}

async fn remove_embedded_connection(state: &Arc<Mutex<EmbeddedBrokerState>>, connection_id: u64) {
    let mut state = state.lock().await;
    state
        .subscribers
        .retain(|entry| entry.connection_id != connection_id);
    state
        .pending
        .retain(|_, pending| pending.connection_id != connection_id);
}

async fn publish_embedded_subject(
    state: &Arc<Mutex<EmbeddedBrokerState>>,
    subject: &str,
    payload: &[u8],
) {
    let mut state = state.lock().await;
    if should_persist_subject(&state, subject) {
        append_stream_message(&mut state, subject, payload.to_vec());
    }
    let targets: Vec<EmbeddedSubscriber> = state
        .subscribers
        .iter()
        .filter(|s| s.subject == subject)
        .cloned()
        .collect();
    let mut stale_connections = Vec::new();

    for subscriber in targets {
        if let Some(consumer_key) = subscriber.durable_consumer.clone() {
            let _ = deliver_next_durable_message(&mut state, &consumer_key);
            continue;
        }
        state.next_delivery_id = state.next_delivery_id.wrapping_add(1);
        if state.next_delivery_id == 0 {
            state.next_delivery_id = 1;
        }
        let ack_subject = format!("{}{}", EMBEDDED_ACK_SUBJECT_PREFIX, state.next_delivery_id);
        let frame = build_msg_frame(subject, &subscriber.sid, Some(&ack_subject), payload);
        if subscriber.tx.send(frame).is_err() {
            stale_connections.push(subscriber.connection_id);
            continue;
        }
        state.pending.insert(
            ack_subject,
            PendingDelivery {
                connection_id: subscriber.connection_id,
                subject: subject.to_string(),
                sid: subscriber.sid,
                payload: payload.to_vec(),
                attempts: 1,
                last_sent_at: Instant::now(),
                durable_consumer: None,
                stream_seq: None,
            },
        );
    }

    if stale_connections.is_empty() {
        return;
    }

    state
        .subscribers
        .retain(|entry| !stale_connections.contains(&entry.connection_id));
    state
        .pending
        .retain(|_, pending| !stale_connections.contains(&pending.connection_id));
}

async fn mark_embedded_ack(state: &Arc<Mutex<EmbeddedBrokerState>>, ack_subject: &str) {
    let mut state = state.lock().await;
    if let Some(pending) = state.pending.remove(ack_subject) {
        state.acked_count = state.acked_count.saturating_add(1);
        if let (Some(consumer_key), Some(seq)) = (pending.durable_consumer, pending.stream_seq) {
            let mut subject_to_prune = None::<String>;
            let mut updated_consumer = false;
            if let Some(consumer) = state.durable_consumers.get_mut(&consumer_key) {
                if seq > consumer.acked_seq {
                    consumer.acked_seq = seq;
                    updated_consumer = true;
                }
                subject_to_prune = Some(consumer.subject.clone());
            }
            if updated_consumer {
                state.durable_dirty = true;
            }
            if let Some(subject) = subject_to_prune {
                prune_stream_for_subject(&mut state, &subject);
            }
            let _ = deliver_next_durable_message(&mut state, &consumer_key);
        }
    }
}

async fn redeliver_embedded_pending(state: &Arc<Mutex<EmbeddedBrokerState>>) {
    let now = Instant::now();
    let mut state = state.lock().await;
    if state.pending.is_empty() {
        return;
    }

    let subscribers = state.subscribers.clone();
    let mut remove_keys = Vec::new();
    let mut redelivered = 0u64;
    let mut dropped = 0u64;

    for (ack_subject, pending) in state.pending.iter_mut() {
        if now.duration_since(pending.last_sent_at) < Duration::from_secs(EMBEDDED_ACK_TIMEOUT_SECS)
        {
            continue;
        }
        let target = if let Some(consumer_key) = pending.durable_consumer.as_deref() {
            subscribers
                .iter()
                .find(|subscriber| subscriber.durable_consumer.as_deref() == Some(consumer_key))
        } else {
            if pending.attempts >= EMBEDDED_ACK_MAX_ATTEMPTS {
                remove_keys.push(ack_subject.clone());
                dropped = dropped.saturating_add(1);
                continue;
            }
            subscribers.iter().find(|subscriber| {
                subscriber.connection_id == pending.connection_id
                    && subscriber.sid == pending.sid
                    && subscriber.subject == pending.subject
            })
        };
        let Some(subscriber) = target else {
            if pending.durable_consumer.is_none() {
                remove_keys.push(ack_subject.clone());
                dropped = dropped.saturating_add(1);
            }
            continue;
        };

        let frame = build_msg_frame(
            &pending.subject,
            &pending.sid,
            Some(ack_subject.as_str()),
            &pending.payload,
        );
        if subscriber.tx.send(frame).is_ok() {
            pending.connection_id = subscriber.connection_id;
            pending.sid = subscriber.sid.clone();
            pending.attempts = pending.attempts.saturating_add(1);
            pending.last_sent_at = now;
            redelivered = redelivered.saturating_add(1);
        } else {
            if pending.durable_consumer.is_none() {
                remove_keys.push(ack_subject.clone());
                dropped = dropped.saturating_add(1);
            }
        }
    }

    for key in remove_keys {
        state.pending.remove(&key);
    }
    state.redelivery_count = state.redelivery_count.saturating_add(redelivered);
    state.dropped_count = state.dropped_count.saturating_add(dropped);

    if redelivered > 0 {
        tracing::debug!(
            count = redelivered,
            pending = state.pending.len(),
            "embedded nats redelivered unacked messages"
        );
    }
    if dropped > 0 {
        tracing::warn!(
            count = dropped,
            pending = state.pending.len(),
            "embedded nats dropped unacked messages"
        );
    }
}

fn should_persist_subject(state: &EmbeddedBrokerState, subject: &str) -> bool {
    is_storage_subject(subject)
        || state
            .durable_consumers
            .values()
            .any(|consumer| consumer.subject == subject)
}

fn is_storage_subject(subject: &str) -> bool {
    matches!(
        subject,
        SUBJECT_STORAGE_TURNS
            | SUBJECT_STORAGE_EVENTS
            | SUBJECT_STORAGE_ITEMS
            | SUBJECT_STORAGE_REACTIVATION
    )
}

fn append_stream_message(state: &mut EmbeddedBrokerState, subject: &str, payload: Vec<u8>) -> u64 {
    let stream = state
        .durable_streams
        .entry(subject.to_string())
        .or_default();
    stream.last_seq = stream.last_seq.saturating_add(1);
    if stream.last_seq == 0 {
        stream.last_seq = 1;
    }
    let seq = stream.last_seq;
    stream.messages.push_back(DurableMessage { seq, payload });
    while stream.messages.len() > DURABLE_STREAM_MAX_MESSAGES {
        let _ = stream.messages.pop_front();
    }
    state.durable_dirty = true;
    seq
}

fn consumer_has_pending(state: &EmbeddedBrokerState, consumer_key: &str) -> bool {
    state.pending.values().any(|pending| {
        pending
            .durable_consumer
            .as_deref()
            .is_some_and(|value| value == consumer_key)
    })
}

fn deliver_next_durable_message(state: &mut EmbeddedBrokerState, consumer_key: &str) -> bool {
    if consumer_has_pending(state, consumer_key) {
        return false;
    }
    let Some(consumer) = state.durable_consumers.get(consumer_key).cloned() else {
        return false;
    };
    let Some(subscriber) = state
        .subscribers
        .iter()
        .find(|entry| entry.durable_consumer.as_deref() == Some(consumer_key))
        .cloned()
    else {
        return false;
    };
    let next_seq = consumer.acked_seq.saturating_add(1);
    let Some(stream) = state.durable_streams.get(&consumer.subject) else {
        return false;
    };
    let mut next_seq = next_seq;
    let message = if let Some(message) = stream
        .messages
        .iter()
        .find(|msg| msg.seq == next_seq)
        .cloned()
    {
        message
    } else if let Some(front) = stream.messages.front().cloned() {
        if front.seq > next_seq {
            if let Some(consumer_mut) = state.durable_consumers.get_mut(consumer_key) {
                consumer_mut.acked_seq = front.seq.saturating_sub(1);
                state.durable_dirty = true;
                next_seq = consumer_mut.acked_seq.saturating_add(1);
            }
            stream
                .messages
                .iter()
                .find(|msg| msg.seq == next_seq)
                .cloned()
                .unwrap_or(front)
        } else {
            return false;
        }
    } else {
        return false;
    };

    state.next_delivery_id = state.next_delivery_id.wrapping_add(1);
    if state.next_delivery_id == 0 {
        state.next_delivery_id = 1;
    }
    let ack_subject = format!("{}{}", EMBEDDED_ACK_SUBJECT_PREFIX, state.next_delivery_id);
    let frame = build_msg_frame(
        &consumer.subject,
        &subscriber.sid,
        Some(&ack_subject),
        &message.payload,
    );
    if subscriber.tx.send(frame).is_err() {
        state
            .subscribers
            .retain(|entry| entry.connection_id != subscriber.connection_id);
        return false;
    }
    state.pending.insert(
        ack_subject,
        PendingDelivery {
            connection_id: subscriber.connection_id,
            subject: consumer.subject,
            sid: subscriber.sid,
            payload: message.payload,
            attempts: 1,
            last_sent_at: Instant::now(),
            durable_consumer: Some(consumer_key.to_string()),
            stream_seq: Some(message.seq),
        },
    );
    true
}

fn prune_stream_for_subject(state: &mut EmbeddedBrokerState, subject: &str) {
    let min_acked = state
        .durable_consumers
        .values()
        .filter(|consumer| consumer.subject == subject)
        .map(|consumer| consumer.acked_seq)
        .min();
    let Some(stream) = state.durable_streams.get_mut(subject) else {
        return;
    };
    let mut changed = false;
    if let Some(min_acked) = min_acked {
        while stream
            .messages
            .front()
            .is_some_and(|msg| msg.seq <= min_acked)
        {
            let _ = stream.messages.pop_front();
            changed = true;
        }
    }
    while stream.messages.len() > DURABLE_STREAM_MAX_MESSAGES {
        let _ = stream.messages.pop_front();
        changed = true;
    }
    if changed {
        state.durable_dirty = true;
    }
}

fn build_msg_frame(subject: &str, sid: &str, reply_to: Option<&str>, payload: &[u8]) -> Vec<u8> {
    let mut frame = if let Some(reply) = reply_to {
        format!("MSG {subject} {sid} {reply} {}\r\n", payload.len()).into_bytes()
    } else {
        format!("MSG {subject} {sid} {}\r\n", payload.len()).into_bytes()
    };
    frame.extend_from_slice(payload);
    frame.extend_from_slice(b"\r\n");
    frame
}

pub async fn publish(endpoint: &str, subject: &str, payload: &[u8]) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(CONNECT_LINE.as_bytes()).await?;
    stream
        .write_all(format!("PUB {subject} {}\r\n", payload.len()).as_bytes())
        .await?;
    stream.write_all(payload).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}

pub async fn check_endpoint(endpoint: &str, timeout_duration: Duration) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let mut stream = timeout(timeout_duration, TcpStream::connect(addr))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("nats connect timeout to {endpoint}"),
            )
        })??;

    stream.write_all(CONNECT_LINE.as_bytes()).await?;
    stream.write_all(b"PING\r\n").await?;
    stream.flush().await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = timeout(timeout_duration, reader.read_line(&mut line))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("nats handshake timeout from {endpoint}"),
            )
        })??;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "nats handshake closed",
        ));
    }

    let line = line.trim_end_matches(['\r', '\n']);
    if line.starts_with("-ERR") {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("nats handshake error: {line}"),
        ));
    }
    Ok(())
}

pub struct NatsSubscriber {
    endpoint: String,
    subject: String,
    sid: u32,
    queue: Option<String>,
}

impl NatsSubscriber {
    pub fn new(endpoint: String, subject: String, sid: u32) -> Self {
        Self {
            endpoint,
            subject,
            sid,
            queue: None,
        }
    }

    pub fn with_queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    pub async fn run<F, Fut>(&self, mut handler: F) -> Result<(), io::Error>
    where
        F: FnMut(Vec<u8>) -> Fut,
        Fut: std::future::Future<Output = Result<(), io::Error>>,
    {
        let addr = endpoint_to_addr(&self.endpoint)?;
        let stream = TcpStream::connect(addr).await?;
        let (reader_half, mut writer_half) = stream.into_split();
        writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
        let sub_line = match self.queue.as_deref() {
            Some(queue) if !queue.trim().is_empty() => {
                format!("SUB {} {} {}\r\n", self.subject, queue.trim(), self.sid)
            }
            _ => format!("SUB {} {}\r\n", self.subject, self.sid),
        };
        writer_half.write_all(sub_line.as_bytes()).await?;
        writer_half.write_all(b"PING\r\n").await?;
        writer_half.flush().await?;

        let mut reader = BufReader::new(reader_half);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "nats socket closed",
                ));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            if line == "PING" {
                writer_half.write_all(b"PONG\r\n").await?;
                writer_half.flush().await?;
                continue;
            }
            if line == "PONG" || line.starts_with("+OK") || line.starts_with("INFO ") {
                continue;
            }
            if line.starts_with("-ERR") {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("nats error: {line}"),
                ));
            }
            if line.starts_with("MSG ") {
                let msg = parse_msg_line(line)?;
                let mut payload = vec![0u8; msg.payload_len + 2];
                reader.read_exact(&mut payload).await?;
                payload.truncate(msg.payload_len);
                if let Err(err) = handler(payload).await {
                    tracing::warn!(
                        subject = %msg.subject,
                        sid = %msg.sid,
                        error = %err,
                        "nats subscriber handler error (message not acked)"
                    );
                    continue;
                }
                if let Some(reply_to) = msg.reply_to.as_deref() {
                    writer_half
                        .write_all(format!("PUB {reply_to} 0\r\n\r\n").as_bytes())
                        .await?;
                    writer_half.flush().await?;
                }
            }
        }
    }
}

fn endpoint_to_addr(endpoint: &str) -> Result<String, io::Error> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty nats endpoint",
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("nats://") {
        if rest.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid nats endpoint",
            ));
        }
        return Ok(rest.to_string());
    }
    Ok(trimmed.to_string())
}

fn parse_sub_command(line: &str) -> Result<(String, String, Option<String>), io::Error> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid SUB line: {line}"),
        ));
    }
    let subject = parts[1].trim();
    let sid = parts.last().copied().unwrap_or_default().trim();
    if subject.is_empty() || sid.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid SUB line: {line}"),
        ));
    }
    let queue = if parts.len() == 4 {
        let queue = parts[2].trim();
        if queue.is_empty() {
            None
        } else {
            Some(queue.to_string())
        }
    } else {
        None
    };
    Ok((subject.to_string(), sid.to_string(), queue))
}

fn parse_durable_consumer_key(subject: &str, queue: &str) -> Option<String> {
    let trimmed = queue.trim();
    if trimmed.is_empty() || !trimmed.starts_with(DURABLE_QUEUE_PREFIX) {
        return None;
    }
    Some(format!("{subject}|{trimmed}"))
}

fn parse_unsub_command(line: &str) -> Option<String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let sid = parts[1].trim();
    if sid.is_empty() {
        return None;
    }
    Some(sid.to_string())
}

fn parse_pub_command(line: &str) -> Result<(String, usize), io::Error> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid PUB line: {line}"),
        ));
    }
    let subject = parts[1].trim();
    if subject.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid PUB line: {line}"),
        ));
    }
    let len_raw = if parts.len() == 3 { parts[2] } else { parts[3] };
    let payload_len = len_raw.parse::<usize>().map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid PUB length '{len_raw}': {err}"),
        )
    })?;
    Ok((subject.to_string(), payload_len))
}

struct MsgFrameHeader {
    subject: String,
    sid: String,
    reply_to: Option<String>,
    payload_len: usize,
}

fn parse_msg_line(msg_line: &str) -> Result<MsgFrameHeader, io::Error> {
    let parts: Vec<&str> = msg_line.split_whitespace().collect();
    if parts.len() != 4 && parts.len() != 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MSG line: {msg_line}"),
        ));
    }
    if parts[0] != "MSG" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MSG line: {msg_line}"),
        ));
    }
    let subject = parts[1].trim();
    let sid = parts[2].trim();
    if subject.is_empty() || sid.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MSG line: {msg_line}"),
        ));
    }
    let (reply_to, len_raw) = if parts.len() == 4 {
        (None, parts[3])
    } else {
        (Some(parts[3]), parts[4])
    };
    let payload_len = len_raw.parse::<usize>().map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MSG length '{len_raw}': {err}"),
        )
    })?;

    Ok(MsgFrameHeader {
        subject: subject.to_string(),
        sid: sid.to_string(),
        reply_to: reply_to.map(|value| value.to_string()),
        payload_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use std::net::TcpListener as StdTcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::time::sleep;

    fn free_endpoint() -> String {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        format!("nats://127.0.0.1:{port}")
    }

    fn temp_storage_dir(name: &str) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos()
            .hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        let suffix = hasher.finish();
        std::env::temp_dir().join(format!("fluxbee-nats-{name}-{suffix:016x}"))
    }

    async fn wait_endpoint_down(endpoint: &str, timeout_ms: u64) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if check_endpoint(endpoint, Duration::from_millis(100))
                .await
                .is_err()
            {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn embedded_broker_start_health_stop_cycle() {
        let endpoint = free_endpoint();
        start_embedded_broker(&endpoint)
            .await
            .expect("start embedded broker");
        check_endpoint(&endpoint, Duration::from_secs(2))
            .await
            .expect("health");

        // Idempotent start on same endpoint should not fail.
        start_embedded_broker(&endpoint)
            .await
            .expect("idempotent start");

        stop_embedded_broker(&endpoint)
            .await
            .expect("stop embedded broker");
        assert!(
            wait_endpoint_down(&endpoint, 1500).await,
            "endpoint stayed up after stop"
        );

        // Recovery: start again after stop.
        start_embedded_broker(&endpoint)
            .await
            .expect("start after stop");
        check_endpoint(&endpoint, Duration::from_secs(2))
            .await
            .expect("health after recovery");
        stop_embedded_broker(&endpoint)
            .await
            .expect("final stop after recovery");
    }

    #[tokio::test]
    async fn embedded_broker_redelivers_when_message_not_acked() {
        let endpoint = free_endpoint();
        start_embedded_broker(&endpoint)
            .await
            .expect("start embedded broker");

        let calls = Arc::new(AtomicU64::new(0));
        let subscriber = NatsSubscriber::new(endpoint.clone(), "test.redelivery".to_string(), 77);
        let calls_for_task = Arc::clone(&calls);
        let subscriber_task = tokio::spawn(async move {
            let _ = subscriber
                .run(move |_payload| {
                    let calls_for_handler = Arc::clone(&calls_for_task);
                    async move {
                        let call = calls_for_handler.fetch_add(1, Ordering::Relaxed) + 1;
                        if call == 1 {
                            return Err(io::Error::new(io::ErrorKind::Other, "fail-first-attempt"));
                        }
                        Ok(())
                    }
                })
                .await;
        });

        sleep(Duration::from_millis(200)).await;
        publish(&endpoint, "test.redelivery", b"hello")
            .await
            .expect("publish");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        while tokio::time::Instant::now() < deadline {
            if calls.load(Ordering::Relaxed) >= 2 {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }

        assert!(
            calls.load(Ordering::Relaxed) >= 2,
            "expected redelivery after missing ack"
        );

        subscriber_task.abort();
        let _ = subscriber_task.await;
        stop_embedded_broker(&endpoint)
            .await
            .expect("stop embedded broker");
    }

    #[tokio::test]
    async fn embedded_broker_recovers_durable_stream_after_restart() {
        let endpoint = free_endpoint();
        let storage_dir = temp_storage_dir("durable-restart");
        start_embedded_broker_with_storage(&endpoint, Some(storage_dir.as_path()))
            .await
            .expect("start embedded broker with durable storage");

        publish(&endpoint, SUBJECT_STORAGE_TURNS, br#"{"seq":1}"#)
            .await
            .expect("publish first");
        publish(&endpoint, SUBJECT_STORAGE_TURNS, br#"{"seq":2}"#)
            .await
            .expect("publish second");

        stop_embedded_broker(&endpoint)
            .await
            .expect("stop embedded broker");

        start_embedded_broker_with_storage(&endpoint, Some(storage_dir.as_path()))
            .await
            .expect("restart embedded broker with durable storage");

        let calls = Arc::new(AtomicU64::new(0));
        let subscriber =
            NatsSubscriber::new(endpoint.clone(), SUBJECT_STORAGE_TURNS.to_string(), 10)
                .with_queue("durable.sy-storage.turns");
        let calls_for_task = Arc::clone(&calls);
        let subscriber_task = tokio::spawn(async move {
            let _ = subscriber
                .run(move |_payload| {
                    let calls_for_handler = Arc::clone(&calls_for_task);
                    async move {
                        calls_for_handler.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                })
                .await;
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        while tokio::time::Instant::now() < deadline {
            if calls.load(Ordering::Relaxed) >= 2 {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }

        assert!(
            calls.load(Ordering::Relaxed) >= 2,
            "expected durable replay after restart"
        );

        subscriber_task.abort();
        let _ = subscriber_task.await;
        stop_embedded_broker(&endpoint)
            .await
            .expect("final stop embedded broker");
        let _ = std::fs::remove_dir_all(storage_dir);
    }
}
