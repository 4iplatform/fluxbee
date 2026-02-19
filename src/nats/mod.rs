use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

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

#[derive(Clone)]
struct EmbeddedSubscriber {
    connection_id: u64,
    subject: String,
    sid: String,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

struct PendingDelivery {
    connection_id: u64,
    subject: String,
    sid: String,
    payload: Vec<u8>,
    attempts: u32,
    last_sent_at: Instant,
}

#[derive(Default)]
struct EmbeddedBrokerState {
    subscribers: Vec<EmbeddedSubscriber>,
    pending: HashMap<String, PendingDelivery>,
    next_delivery_id: u64,
    acked_count: u64,
    redelivery_count: u64,
    dropped_count: u64,
}

struct EmbeddedBrokerHandle {
    shutdown_tx: oneshot::Sender<()>,
    task: JoinHandle<Result<(), io::Error>>,
}

fn embedded_registry() -> &'static Mutex<HashMap<String, EmbeddedBrokerHandle>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, EmbeddedBrokerHandle>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub async fn start_embedded_broker(endpoint: &str) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
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

    let state = Arc::new(Mutex::new(EmbeddedBrokerState::default()));
    let next_connection_id = Arc::new(AtomicU64::new(1));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(run_embedded_broker(
        listener,
        state,
        next_connection_id,
        shutdown_rx,
    ));

    let mut registry = embedded_registry().lock().await;
    registry.insert(addr, EmbeddedBrokerHandle { shutdown_tx, task });

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

async fn stop_embedded_broker_handle(
    endpoint: &str,
    handle: EmbeddedBrokerHandle,
) -> Result<(), io::Error> {
    let EmbeddedBrokerHandle {
        shutdown_tx,
        mut task,
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
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::debug!("embedded nats broker shutdown requested");
                break;
            }
            _ = redelivery_ticker.tick() => {
                redeliver_embedded_pending(&state).await;
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

    connection_tasks.abort_all();
    while let Some(result) = connection_tasks.join_next().await {
        if let Err(err) = result {
            if !err.is_cancelled() {
                tracing::debug!(error = %err, "embedded nats connection task join failed");
            }
        }
    }

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
            let (subject, sid) = parse_sub_command(line)?;
            add_embedded_subscriber(
                &state,
                EmbeddedSubscriber {
                    connection_id,
                    subject,
                    sid,
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
    state.subscribers.push(subscriber);
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
    let targets: Vec<EmbeddedSubscriber> = state
        .subscribers
        .iter()
        .filter(|s| s.subject == subject)
        .cloned()
        .collect();
    let mut stale_connections = Vec::new();

    for subscriber in targets {
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
    if state.pending.remove(ack_subject).is_some() {
        state.acked_count = state.acked_count.saturating_add(1);
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
        if now.duration_since(pending.last_sent_at) < Duration::from_secs(EMBEDDED_ACK_TIMEOUT_SECS) {
            continue;
        }
        if pending.attempts >= EMBEDDED_ACK_MAX_ATTEMPTS {
            remove_keys.push(ack_subject.clone());
            dropped = dropped.saturating_add(1);
            continue;
        }
        let target = subscribers.iter().find(|subscriber| {
            subscriber.connection_id == pending.connection_id
                && subscriber.sid == pending.sid
                && subscriber.subject == pending.subject
        });
        let Some(subscriber) = target else {
            remove_keys.push(ack_subject.clone());
            dropped = dropped.saturating_add(1);
            continue;
        };

        let frame = build_msg_frame(
            &pending.subject,
            &pending.sid,
            Some(ack_subject.as_str()),
            &pending.payload,
        );
        if subscriber.tx.send(frame).is_ok() {
            pending.attempts = pending.attempts.saturating_add(1);
            pending.last_sent_at = now;
            redelivered = redelivered.saturating_add(1);
        } else {
            remove_keys.push(ack_subject.clone());
            dropped = dropped.saturating_add(1);
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
}

impl NatsSubscriber {
    pub fn new(endpoint: String, subject: String, sid: u32) -> Self {
        Self {
            endpoint,
            subject,
            sid,
        }
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
        writer_half
            .write_all(format!("SUB {} {}\r\n", self.subject, self.sid).as_bytes())
            .await?;
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

fn parse_sub_command(line: &str) -> Result<(String, String), io::Error> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
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
    Ok((subject.to_string(), sid.to_string()))
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
    use std::net::TcpListener as StdTcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use tokio::time::sleep;

    fn free_endpoint() -> String {
        let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);
        format!("nats://127.0.0.1:{port}")
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
}
