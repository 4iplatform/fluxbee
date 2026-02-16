use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;

pub const SUBJECT_STORAGE_TURNS: &str = "storage.turns";
pub const SUBJECT_STORAGE_EVENTS: &str = "storage.events";
pub const SUBJECT_STORAGE_ITEMS: &str = "storage.items";
pub const SUBJECT_STORAGE_REACTIVATION: &str = "storage.reactivation";

const CONNECT_LINE: &str = "CONNECT {\"lang\":\"rust\",\"version\":\"0.1\",\"verbose\":false,\"pedantic\":false,\"tls_required\":false}\r\n";
const EMBEDDED_INFO_LINE: &str =
    "INFO {\"server_id\":\"fluxbee-router\",\"server_name\":\"fluxbee-router\",\"version\":\"1.0.0\",\"proto\":1,\"max_payload\":1048576}\r\n";

#[derive(Clone)]
struct EmbeddedSubscriber {
    connection_id: u64,
    subject: String,
    sid: String,
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

#[derive(Default)]
struct EmbeddedBrokerState {
    subscribers: Vec<EmbeddedSubscriber>,
}

pub async fn start_embedded_broker(endpoint: &str) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let listener = TcpListener::bind(&addr).await?;
    let state = Arc::new(Mutex::new(EmbeddedBrokerState::default()));
    let next_connection_id = Arc::new(AtomicU64::new(1));

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(error = %err, "embedded nats accept failed");
                    continue;
                }
            };

            let connection_id = next_connection_id.fetch_add(1, Ordering::Relaxed);
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                if let Err(err) = handle_embedded_connection(stream, connection_id, state).await {
                    tracing::warn!(
                        connection_id,
                        error = %err,
                        "embedded nats connection closed with error"
                    );
                }
            });
        }
    });

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
            publish_embedded_subject(&state, &subject, &payload).await;
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
}

async fn remove_embedded_connection(state: &Arc<Mutex<EmbeddedBrokerState>>, connection_id: u64) {
    let mut state = state.lock().await;
    state
        .subscribers
        .retain(|entry| entry.connection_id != connection_id);
}

async fn publish_embedded_subject(
    state: &Arc<Mutex<EmbeddedBrokerState>>,
    subject: &str,
    payload: &[u8],
) {
    let mut stale_connections = Vec::new();
    {
        let state = state.lock().await;
        for subscriber in state.subscribers.iter().filter(|s| s.subject == subject) {
            let mut frame =
                format!("MSG {} {} {}\r\n", subject, subscriber.sid, payload.len()).into_bytes();
            frame.extend_from_slice(payload);
            frame.extend_from_slice(b"\r\n");
            if subscriber.tx.send(frame).is_err() {
                stale_connections.push(subscriber.connection_id);
            }
        }
    }
    if stale_connections.is_empty() {
        return;
    }

    let mut state = state.lock().await;
    state
        .subscribers
        .retain(|entry| !stale_connections.contains(&entry.connection_id));
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
                let payload_len = parse_payload_len(line)?;
                let mut payload = vec![0u8; payload_len + 2];
                reader.read_exact(&mut payload).await?;
                payload.truncate(payload_len);
                handler(payload).await?;
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

fn parse_payload_len(msg_line: &str) -> Result<usize, io::Error> {
    let mut parts = msg_line.split_whitespace();
    let _ = parts.next(); // MSG
    let _subject = parts.next();
    let _sid = parts.next();
    let maybe_reply_or_len = parts.next();
    let last = parts.next();
    let len_str = match (maybe_reply_or_len, last) {
        (Some(len), None) => len,
        (Some(_reply), Some(len)) => len,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid MSG line: {msg_line}"),
            ))
        }
    };
    len_str.parse::<usize>().map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MSG length '{len_str}': {err}"),
        )
    })
}
