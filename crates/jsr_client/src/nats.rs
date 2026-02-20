use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

use crate::client_config::ClientConfig;

const CONNECT_LINE: &str = "CONNECT {\"lang\":\"rust\",\"version\":\"0.1\",\"verbose\":false,\"pedantic\":false,\"tls_required\":false}\r\n";
const NATS_LOG_LINE_MAX_LEN: usize = 220;
const PUBLISH_SYNC_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_LOCAL_NATS_PORT: u16 = 4222;
const DEFAULT_LOCAL_NATS_ENDPOINT: &str = "nats://127.0.0.1:4222";
const DEFAULT_CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_CLIENT_SYNC_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_CLIENT_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const DEFAULT_CLIENT_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(2);
const DEFAULT_CLIENT_RECONNECT_ATTEMPTS: usize = 4;
const SESSION_INBOX_PREFIX: &str = "_INBOX.JSR";
const SESSION_INBOX_SID_BASE: u32 = 20_000;
const SESSION_INBOX_SID_RANGE: u64 = 20_000;
pub const NATS_ENVELOPE_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, thiserror::Error)]
pub enum NatsError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("timeout: {0}")]
    Timeout(String),
}

#[derive(Clone, Debug)]
pub struct NatsMessage {
    pub subject: String,
    pub sid: String,
    pub reply_to: Option<String>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NatsRequestEnvelope<T> {
    pub schema_version: u16,
    pub action: String,
    pub trace_id: String,
    pub reply_subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<T>,
}

impl<T> NatsRequestEnvelope<T> {
    pub fn new(
        action: impl Into<String>,
        trace_id: impl Into<String>,
        reply_subject: impl Into<String>,
        payload: Option<T>,
    ) -> Self {
        Self {
            schema_version: NATS_ENVELOPE_SCHEMA_VERSION,
            action: action.into(),
            trace_id: trace_id.into(),
            reply_subject: reply_subject.into(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NatsResponseEnvelope<T> {
    pub schema_version: u16,
    pub action: String,
    pub trace_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
}

impl<T> NatsResponseEnvelope<T> {
    pub fn ok(action: impl Into<String>, trace_id: impl Into<String>, payload: T) -> Self {
        Self {
            schema_version: NATS_ENVELOPE_SCHEMA_VERSION,
            action: action.into(),
            trace_id: trace_id.into(),
            status: "ok".to_string(),
            payload: Some(payload),
            error_code: None,
            error_detail: None,
        }
    }

    pub fn error(
        action: impl Into<String>,
        trace_id: impl Into<String>,
        error_code: impl Into<String>,
        error_detail: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: NATS_ENVELOPE_SCHEMA_VERSION,
            action: action.into(),
            trace_id: trace_id.into(),
            status: "error".to_string(),
            payload: None,
            error_code: Some(error_code.into()),
            error_detail: Some(error_detail.into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NatsClientOptions {
    pub connect_timeout: Duration,
    pub sync_timeout: Duration,
    pub reconnect_initial_backoff: Duration,
    pub reconnect_max_backoff: Duration,
    pub reconnect_attempts: usize,
}

impl Default for NatsClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CLIENT_CONNECT_TIMEOUT,
            sync_timeout: DEFAULT_CLIENT_SYNC_TIMEOUT,
            reconnect_initial_backoff: DEFAULT_CLIENT_RECONNECT_INITIAL_BACKOFF,
            reconnect_max_backoff: DEFAULT_CLIENT_RECONNECT_MAX_BACKOFF,
            reconnect_attempts: DEFAULT_CLIENT_RECONNECT_ATTEMPTS,
        }
    }
}

struct NatsConnection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

struct NatsClientState {
    connection: Option<NatsConnection>,
    response_subscriptions: HashMap<String, u32>,
    has_connected_once: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NatsClientMetricsSnapshot {
    pub timeouts: u64,
    pub reconnects: u64,
    pub in_flight: u64,
    pub last_error: Option<String>,
}

struct NatsClientMetrics {
    timeouts: AtomicU64,
    reconnects: AtomicU64,
    in_flight: AtomicU64,
    last_error: StdMutex<Option<String>>,
}

impl NatsClientMetrics {
    fn snapshot(&self) -> NatsClientMetricsSnapshot {
        let last_error = self
            .last_error
            .lock()
            .map(|value| value.clone())
            .unwrap_or_else(|_| Some("metrics.last_error lock poisoned".to_string()));
        NatsClientMetricsSnapshot {
            timeouts: self.timeouts.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            last_error,
        }
    }
}

struct NatsInFlightGuard {
    metrics: Arc<NatsClientMetrics>,
}

impl NatsInFlightGuard {
    fn new(metrics: Arc<NatsClientMetrics>) -> Self {
        metrics.in_flight.fetch_add(1, Ordering::Relaxed);
        Self { metrics }
    }
}

impl Drop for NatsInFlightGuard {
    fn drop(&mut self) {
        self.metrics.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct NatsClient {
    endpoint: String,
    options: NatsClientOptions,
    session_inbox_prefix: String,
    session_inbox_subscription: String,
    session_inbox_sid: u32,
    metrics: Arc<NatsClientMetrics>,
    state: Mutex<NatsClientState>,
}

impl NatsClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_options(endpoint, NatsClientOptions::default())
    }

    pub fn with_options(endpoint: impl Into<String>, options: NatsClientOptions) -> Self {
        let session = Uuid::new_v4().simple().to_string();
        let session_inbox_prefix = format!("{SESSION_INBOX_PREFIX}.{session}");
        let session_inbox_subscription = format!("{session_inbox_prefix}.*");
        let session_inbox_sid = SESSION_INBOX_SID_BASE
            + (fnv1a64(session_inbox_prefix.as_bytes()) % SESSION_INBOX_SID_RANGE) as u32;
        Self {
            endpoint: endpoint.into(),
            options,
            session_inbox_prefix,
            session_inbox_subscription,
            session_inbox_sid,
            metrics: Arc::new(NatsClientMetrics {
                timeouts: AtomicU64::new(0),
                reconnects: AtomicU64::new(0),
                in_flight: AtomicU64::new(0),
                last_error: StdMutex::new(None),
            }),
            state: Mutex::new(NatsClientState {
                connection: None,
                response_subscriptions: HashMap::new(),
                has_connected_once: false,
            }),
        }
    }

    pub fn inbox_reply_subject(&self, correlation_id: &str) -> String {
        let token = sanitize_inbox_token(correlation_id);
        format!("{}.{}", self.session_inbox_prefix, token)
    }

    pub fn metrics_snapshot(&self) -> NatsClientMetricsSnapshot {
        self.metrics.snapshot()
    }

    fn record_error(&self, err: &NatsError) {
        if matches!(err, NatsError::Timeout(_)) {
            self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        if let Ok(mut last_error) = self.metrics.last_error.lock() {
            *last_error = Some(err.to_string());
        }
    }

    fn on_connect_success(&self, state: &mut NatsClientState) {
        if state.has_connected_once {
            self.metrics.reconnects.fetch_add(1, Ordering::Relaxed);
        }
        state.has_connected_once = true;
    }

    pub async fn request_with_session_inbox(
        &self,
        request_subject: &str,
        request_payload: &[u8],
        response_subject: &str,
        timeout_duration: Duration,
    ) -> Result<Vec<u8>, NatsError> {
        self.request_multiplexed(
            request_subject,
            request_payload,
            response_subject,
            &self.session_inbox_subscription,
            self.session_inbox_sid,
            timeout_duration,
        )
        .await
    }

    pub async fn publish(&self, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
        let attempts = self.options.reconnect_attempts.max(1);
        let mut backoff = self.options.reconnect_initial_backoff;
        let mut last_err = None;

        for attempt in 1..=attempts {
            let mut state = self.state.lock().await;
            if state.connection.is_none() {
                drop(state);
                match self.connect_with_retry().await {
                    Ok(conn) => {
                        state = self.state.lock().await;
                        if state.connection.is_none() {
                            state.connection = Some(conn);
                            state.response_subscriptions.clear();
                            self.on_connect_success(&mut state);
                        }
                    }
                    Err(err) => {
                        self.record_error(&err);
                        last_err = Some(err);
                        if attempt == attempts {
                            break;
                        }
                        sleep(backoff).await;
                        backoff = std::cmp::min(
                            backoff.saturating_mul(2),
                            self.options.reconnect_max_backoff,
                        );
                        continue;
                    }
                }
            }

            let result = match state.connection.as_mut() {
                Some(conn) => match publish_on_connection(conn, subject, payload).await {
                    Ok(()) => sync_connection(conn, self.options.sync_timeout, "publish")
                        .await
                        .map_err(|err| match err {
                            NatsError::Timeout(_) => NatsError::Timeout(format!(
                                "publish timeout waiting server ack on subject {subject}"
                            )),
                            other => other,
                        }),
                    Err(err) => Err(err),
                },
                None => Err(NatsError::Protocol("missing nats connection".to_string())),
            };
            match result {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let retryable = is_retryable_nats_error(&err);
                    state.connection = None;
                    state.response_subscriptions.clear();
                    self.record_error(&err);
                    last_err = Some(err);
                    if attempt == attempts || !retryable {
                        break;
                    }
                }
            }
            drop(state);
            sleep(backoff).await;
            backoff = std::cmp::min(
                backoff.saturating_mul(2),
                self.options.reconnect_max_backoff,
            );
        }

        Err(last_err.unwrap_or_else(|| {
            NatsError::Protocol("nats client publish failed".to_string())
        }))
    }

    pub async fn request(
        &self,
        request_subject: &str,
        request_payload: &[u8],
        response_subject: &str,
        sid: u32,
        timeout_duration: Duration,
    ) -> Result<Vec<u8>, NatsError> {
        let _in_flight = NatsInFlightGuard::new(Arc::clone(&self.metrics));
        let attempts = self.options.reconnect_attempts.max(1);
        let mut backoff = self.options.reconnect_initial_backoff;
        let mut last_err = None;

        for attempt in 1..=attempts {
            let mut state = self.state.lock().await;
            if state.connection.is_none() {
                drop(state);
                match self.connect_with_retry().await {
                    Ok(conn) => {
                        state = self.state.lock().await;
                        if state.connection.is_none() {
                            state.connection = Some(conn);
                            state.response_subscriptions.clear();
                            self.on_connect_success(&mut state);
                        }
                    }
                    Err(err) => {
                        self.record_error(&err);
                        last_err = Some(err);
                        if attempt == attempts {
                            break;
                        }
                        sleep(backoff).await;
                        backoff = std::cmp::min(backoff.saturating_mul(2), self.options.reconnect_max_backoff);
                        continue;
                    }
                }
            }

            let result = match state.connection.as_mut() {
                Some(conn) => {
                    request_on_connection(
                        conn,
                        request_subject,
                        request_payload,
                        response_subject,
                        sid,
                        timeout_duration,
                        self.options.sync_timeout,
                    )
                    .await
                }
                None => Err(NatsError::Protocol("missing nats connection".to_string())),
            };
            match result {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let retryable = is_retryable_nats_error(&err);
                    state.connection = None;
                    state.response_subscriptions.clear();
                    self.record_error(&err);
                    last_err = Some(err);
                    if attempt == attempts || !retryable {
                        break;
                    }
                }
            }
            drop(state);
            sleep(backoff).await;
            backoff = std::cmp::min(
                backoff.saturating_mul(2),
                self.options.reconnect_max_backoff,
            );
        }

        Err(last_err
            .unwrap_or_else(|| NatsError::Protocol("nats client request failed".to_string())))
    }

    pub async fn request_multiplexed(
        &self,
        request_subject: &str,
        request_payload: &[u8],
        response_subject: &str,
        response_subscription: &str,
        sid: u32,
        timeout_duration: Duration,
    ) -> Result<Vec<u8>, NatsError> {
        let _in_flight = NatsInFlightGuard::new(Arc::clone(&self.metrics));
        let attempts = self.options.reconnect_attempts.max(1);
        let mut backoff = self.options.reconnect_initial_backoff;
        let mut last_err = None;

        for attempt in 1..=attempts {
            let mut state = self.state.lock().await;
            if state.connection.is_none() {
                drop(state);
                match self.connect_with_retry().await {
                    Ok(conn) => {
                        state = self.state.lock().await;
                        if state.connection.is_none() {
                            state.connection = Some(conn);
                            state.response_subscriptions.clear();
                            self.on_connect_success(&mut state);
                        }
                    }
                    Err(err) => {
                        self.record_error(&err);
                        last_err = Some(err);
                        if attempt == attempts {
                            break;
                        }
                        sleep(backoff).await;
                        backoff = std::cmp::min(
                            backoff.saturating_mul(2),
                            self.options.reconnect_max_backoff,
                        );
                        continue;
                    }
                }
            }

            let NatsClientState {
                connection,
                response_subscriptions,
                ..
            } = &mut *state;
            let result = match connection.as_mut() {
                Some(conn) => {
                    request_multiplexed_on_connection(
                        conn,
                        response_subscriptions,
                        request_subject,
                        request_payload,
                        response_subject,
                        response_subscription,
                        sid,
                        timeout_duration,
                        self.options.sync_timeout,
                    )
                    .await
                }
                None => Err(NatsError::Protocol("missing nats connection".to_string())),
            };
            match result {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let retryable = is_retryable_nats_error(&err);
                    state.connection = None;
                    state.response_subscriptions.clear();
                    self.record_error(&err);
                    last_err = Some(err);
                    if attempt == attempts || !retryable {
                        break;
                    }
                }
            }
            drop(state);
            sleep(backoff).await;
            backoff = std::cmp::min(
                backoff.saturating_mul(2),
                self.options.reconnect_max_backoff,
            );
        }

        Err(last_err.unwrap_or_else(|| {
            NatsError::Protocol("nats client multiplexed request failed".to_string())
        }))
    }

    async fn connect_with_retry(&self) -> Result<NatsConnection, NatsError> {
        let attempts = self.options.reconnect_attempts.max(1);
        let mut backoff = self.options.reconnect_initial_backoff;
        let mut last_err = None;

        for attempt in 1..=attempts {
            match connect_nats_connection(
                &self.endpoint,
                self.options.connect_timeout,
                self.options.sync_timeout,
            )
            .await
            {
                Ok(conn) => return Ok(conn),
                Err(err) => {
                    let retryable = is_retryable_nats_error(&err);
                    last_err = Some(err);
                    if attempt == attempts || !retryable {
                        break;
                    }
                    sleep(backoff).await;
                    backoff =
                        std::cmp::min(backoff.saturating_mul(2), self.options.reconnect_max_backoff);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| NatsError::Protocol("nats connect failed".to_string())))
    }
}

fn is_retryable_nats_error(err: &NatsError) -> bool {
    matches!(err, NatsError::Io(_) | NatsError::Timeout(_))
}

async fn connect_nats_connection(
    endpoint: &str,
    connect_timeout: Duration,
    sync_timeout: Duration,
) -> Result<NatsConnection, NatsError> {
    let addr = endpoint_to_addr(endpoint)?;
    let stream = timeout(connect_timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| NatsError::Timeout(format!("connect timeout to {endpoint}")))??;
    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
    writer_half.flush().await?;
    let mut conn = NatsConnection {
        reader: BufReader::new(reader_half),
        writer: writer_half,
    };
    sync_connection(&mut conn, sync_timeout, "connect").await?;
    Ok(conn)
}

async fn sync_connection(
    conn: &mut NatsConnection,
    timeout_duration: Duration,
    context: &str,
) -> Result<(), NatsError> {
    conn.writer.write_all(b"PING\r\n").await?;
    conn.writer.flush().await?;
    loop {
        let mut line = String::new();
        let n = timeout(timeout_duration, conn.reader.read_line(&mut line))
            .await
            .map_err(|_| {
                NatsError::Timeout(format!(
                    "{context} timeout waiting server sync (PONG)"
                ))
            })??;
        if n == 0 {
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            continue;
        }
        if line == "PING" {
            conn.writer.write_all(b"PONG\r\n").await?;
            conn.writer.flush().await?;
            continue;
        }
        if line == "PONG" {
            return Ok(());
        }
        if line.starts_with("-ERR") {
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(timeout_duration, conn.reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    NatsError::Timeout(format!(
                        "{context} timeout draining message payload during sync"
                    ))
                })??;
            continue;
        }
    }
}

async fn publish_on_connection(
    conn: &mut NatsConnection,
    subject: &str,
    payload: &[u8],
) -> Result<(), NatsError> {
    conn.writer
        .write_all(format!("PUB {subject} {}\r\n", payload.len()).as_bytes())
        .await?;
    conn.writer.write_all(payload).await?;
    conn.writer.write_all(b"\r\n").await?;
    conn.writer.flush().await?;
    Ok(())
}

async fn request_on_connection(
    conn: &mut NatsConnection,
    request_subject: &str,
    request_payload: &[u8],
    response_subject: &str,
    sid: u32,
    timeout_duration: Duration,
    sync_timeout: Duration,
) -> Result<Vec<u8>, NatsError> {
    conn.writer
        .write_all(format!("UNSUB {sid}\r\n").as_bytes())
        .await?;
    conn.writer
        .write_all(format!("SUB {response_subject} {sid}\r\n").as_bytes())
        .await?;
    conn.writer.flush().await?;
    sync_connection(conn, sync_timeout, "request subscription sync").await?;

    publish_on_connection(conn, request_subject, request_payload).await?;

    let deadline = tokio::time::Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(NatsError::Timeout(format!(
                "request timeout waiting response header on {response_subject}"
            )));
        }
        let mut line = String::new();
        let n = timeout(remaining, conn.reader.read_line(&mut line))
            .await
            .map_err(|_| {
                NatsError::Timeout(format!(
                    "request timeout waiting response header on {response_subject}"
                ))
            })??;
        if n == 0 {
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            continue;
        }
        if line == "PING" {
            conn.writer.write_all(b"PONG\r\n").await?;
            conn.writer.flush().await?;
            continue;
        }
        if line == "PONG" {
            continue;
        }
        if line.starts_with("-ERR") {
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(remaining, conn.reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    NatsError::Timeout(format!(
                        "request timeout reading response payload on {response_subject}"
                    ))
                })??;
            payload.truncate(msg.payload_len);
            if msg.subject == response_subject {
                conn.writer
                    .write_all(format!("UNSUB {sid}\r\n").as_bytes())
                    .await?;
                conn.writer.flush().await?;
                return Ok(payload);
            }
        }
    }
}

async fn ensure_response_subscription(
    conn: &mut NatsConnection,
    subscriptions: &mut HashMap<String, u32>,
    response_subscription: &str,
    sid: u32,
    sync_timeout: Duration,
) -> Result<(), NatsError> {
    if subscriptions.contains_key(response_subscription) {
        return Ok(());
    }
    conn.writer
        .write_all(format!("SUB {response_subscription} {sid}\r\n").as_bytes())
        .await?;
    conn.writer.flush().await?;
    sync_connection(conn, sync_timeout, "request multiplexed subscription sync").await?;
    subscriptions.insert(response_subscription.to_string(), sid);
    Ok(())
}

async fn request_multiplexed_on_connection(
    conn: &mut NatsConnection,
    subscriptions: &mut HashMap<String, u32>,
    request_subject: &str,
    request_payload: &[u8],
    response_subject: &str,
    response_subscription: &str,
    sid: u32,
    timeout_duration: Duration,
    sync_timeout: Duration,
) -> Result<Vec<u8>, NatsError> {
    ensure_response_subscription(
        conn,
        subscriptions,
        response_subscription,
        sid,
        sync_timeout,
    )
    .await?;

    publish_on_connection(conn, request_subject, request_payload).await?;

    let deadline = tokio::time::Instant::now() + timeout_duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(NatsError::Timeout(format!(
                "request timeout waiting response header on {response_subject}"
            )));
        }
        let mut line = String::new();
        let n = timeout(remaining, conn.reader.read_line(&mut line))
            .await
            .map_err(|_| {
                NatsError::Timeout(format!(
                    "request timeout waiting response header on {response_subject}"
                ))
            })??;
        if n == 0 {
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            continue;
        }
        if line == "PING" {
            conn.writer.write_all(b"PONG\r\n").await?;
            conn.writer.flush().await?;
            continue;
        }
        if line == "PONG" {
            continue;
        }
        if line.starts_with("-ERR") {
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(remaining, conn.reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    NatsError::Timeout(format!(
                        "request timeout reading response payload on {response_subject}"
                    ))
                })??;
            payload.truncate(msg.payload_len);
            if msg.subject == response_subject {
                return Ok(payload);
            }
        }
    }
}

pub async fn publish(endpoint: &str, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
    let addr = endpoint_to_addr(endpoint)?;
    let publish_started = Instant::now();
    let stream = timeout(PUBLISH_SYNC_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| NatsError::Timeout(format!("publish connect timeout to {endpoint}")))??;
    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
    writer_half
        .write_all(format!("PUB {subject} {}\r\n", payload.len()).as_bytes())
        .await?;
    writer_half.write_all(payload).await?;
    writer_half.write_all(b"\r\n").await?;
    writer_half.write_all(b"PING\r\n").await?;
    writer_half.flush().await?;

    let mut reader = BufReader::new(reader_half);
    let sync_started = Instant::now();
    loop {
        let mut line = String::new();
        let n = timeout(PUBLISH_SYNC_TIMEOUT, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                NatsError::Timeout(format!("publish timeout waiting server ack on subject {subject}"))
            })??;
        if n == 0 {
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            continue;
        }
        if line == "PING" {
            writer_half.write_all(b"PONG\r\n").await?;
            writer_half.flush().await?;
            continue;
        }
        if line == "PONG" {
            tracing::debug!(
                endpoint = %endpoint,
                subject = %subject,
                payload_bytes = payload.len(),
                sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                elapsed_ms = publish_started.elapsed().as_millis() as u64,
                "nats publish completed with server ack"
            );
            break;
        }
        if line.starts_with("-ERR") {
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
    }
    Ok(())
}

pub async fn publish_local(
    config_dir: impl AsRef<Path>,
    subject: &str,
    payload: &[u8],
) -> Result<(), NatsError> {
    let endpoint = resolve_local_nats_endpoint(config_dir)?;
    publish(&endpoint, subject, payload).await
}

pub async fn publish_with_client_config(
    config: &ClientConfig,
    subject: &str,
    payload: &[u8],
) -> Result<(), NatsError> {
    let endpoint = config.resolved_nats_endpoint()?;
    publish(&endpoint, subject, payload).await
}

pub async fn request(
    endpoint: &str,
    request_subject: &str,
    request_payload: &[u8],
    response_subject: &str,
    sid: u32,
    timeout_duration: Duration,
) -> Result<Vec<u8>, NatsError> {
    let request_started = Instant::now();
    tracing::debug!(
        endpoint = %endpoint,
        request_subject = %request_subject,
        response_subject = %response_subject,
        sid = sid,
        timeout_ms = timeout_duration.as_millis() as u64,
        request_bytes = request_payload.len(),
        "nats request start"
    );

    let addr = endpoint_to_addr(endpoint)?;
    let connect_started = Instant::now();
    let stream = timeout(timeout_duration, TcpStream::connect(&addr))
        .await
        .map_err(|_| {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                timeout_ms = timeout_duration.as_millis() as u64,
                elapsed_ms = connect_started.elapsed().as_millis() as u64,
                "nats request connect timeout"
            );
            NatsError::Timeout(format!("connect timeout to {endpoint}"))
        })??;
    tracing::debug!(
        endpoint = %endpoint,
        request_subject = %request_subject,
        response_subject = %response_subject,
        sid = sid,
        elapsed_ms = connect_started.elapsed().as_millis() as u64,
        "nats request tcp connected"
    );

    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
    writer_half
        .write_all(format!("SUB {response_subject} {sid}\r\n").as_bytes())
        .await?;
    writer_half.write_all(b"PING\r\n").await?;
    writer_half.flush().await?;
    tracing::debug!(
        endpoint = %endpoint,
        request_subject = %request_subject,
        response_subject = %response_subject,
        sid = sid,
        elapsed_ms = request_started.elapsed().as_millis() as u64,
        "nats request sent CONNECT/SUB/PING"
    );

    let mut reader = BufReader::new(reader_half);
    let deadline = tokio::time::Instant::now() + timeout_duration;
    let sync_started = Instant::now();
    loop {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                "nats request timeout waiting subscription ack"
            );
            return Err(NatsError::Timeout(format!(
                "request timeout waiting subscription ack on {response_subject}"
            )));
        }

        let mut line = String::new();
        let n = timeout(remaining, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                tracing::warn!(
                    endpoint = %endpoint,
                    request_subject = %request_subject,
                    response_subject = %response_subject,
                    sid = sid,
                    elapsed_ms = request_started.elapsed().as_millis() as u64,
                    sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                    "nats request timeout while reading subscription ack line"
                );
                NatsError::Timeout(format!(
                    "request timeout waiting subscription ack on {response_subject}"
                ))
            })??;
        if n == 0 {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request socket closed while waiting subscription ack"
            );
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                line = %summarize_nats_line(line),
                "nats request sync loop ignored server line"
            );
            continue;
        }
        if line == "PING" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                "nats request sync loop received PING; sending PONG"
            );
            writer_half.write_all(b"PONG\r\n").await?;
            writer_half.flush().await?;
            continue;
        }
        if line == "PONG" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request subscription sync completed"
            );
            break;
        }
        if line.starts_with("-ERR") {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                error_line = %summarize_nats_line(line),
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request server returned error during sync"
            );
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(remaining, reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    tracing::warn!(
                        endpoint = %endpoint,
                        request_subject = %request_subject,
                        response_subject = %response_subject,
                        sid = sid,
                        message_subject = %msg.subject,
                        payload_len = msg.payload_len,
                        elapsed_ms = request_started.elapsed().as_millis() as u64,
                        "nats request timeout draining pre-ack payload"
                    );
                    NatsError::Timeout(format!(
                        "request timeout draining pre-ack payload on {response_subject}"
                    ))
                })??;
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                message_subject = %msg.subject,
                payload_len = msg.payload_len,
                "nats request drained pre-ack message frame"
            );
            continue;
        }
    }

    let publish_started = Instant::now();
    let publish_result = async {
        writer_half
            .write_all(format!("PUB {request_subject} {}\r\n", request_payload.len()).as_bytes())
            .await?;
        writer_half.write_all(request_payload).await?;
        writer_half.write_all(b"\r\n").await?;
        writer_half.flush().await?;
        Ok::<(), io::Error>(())
    }
    .await;
    match publish_result {
        Ok(()) => {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                publish_elapsed_ms = publish_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                request_bytes = request_payload.len(),
                "nats request payload published"
            );
        }
        Err(err) => {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                publish_elapsed_ms = publish_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                error = %err,
                "nats request publish failed"
            );
            return Err(NatsError::Io(err));
        }
    }

    let wait_response_started = Instant::now();
    loop {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request timeout waiting response message"
            );
            return Err(NatsError::Timeout(format!(
                "request timeout waiting response on {response_subject}"
            )));
        }

        let mut line = String::new();
        let n = timeout(remaining, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                tracing::warn!(
                    endpoint = %endpoint,
                    request_subject = %request_subject,
                    response_subject = %response_subject,
                    sid = sid,
                    wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                    elapsed_ms = request_started.elapsed().as_millis() as u64,
                    "nats request timeout waiting response header"
                );
                NatsError::Timeout(format!(
                    "request timeout waiting response header on {response_subject}"
                ))
            })??;
        if n == 0 {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request socket closed while waiting response"
            );
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                line = %summarize_nats_line(line),
                "nats request response loop ignored server line"
            );
            continue;
        }
        if line == "PING" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                "nats request response loop received PING; sending PONG"
            );
            writer_half.write_all(b"PONG\r\n").await?;
            writer_half.flush().await?;
            continue;
        }
        if line == "PONG" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                "nats request response loop received PONG"
            );
            continue;
        }
        if line.starts_with("-ERR") {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                error_line = %summarize_nats_line(line),
                wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request server returned error while waiting response"
            );
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(remaining, reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    tracing::warn!(
                        endpoint = %endpoint,
                        request_subject = %request_subject,
                        response_subject = %response_subject,
                        sid = sid,
                        message_subject = %msg.subject,
                        payload_len = msg.payload_len,
                        wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                        elapsed_ms = request_started.elapsed().as_millis() as u64,
                        "nats request timeout reading response payload"
                    );
                    NatsError::Timeout(format!(
                        "request timeout reading response payload on {response_subject}"
                    ))
                })??;
            payload.truncate(msg.payload_len);
            if msg.subject == response_subject {
                tracing::debug!(
                    endpoint = %endpoint,
                    request_subject = %request_subject,
                    response_subject = %response_subject,
                    sid = sid,
                    payload_bytes = payload.len(),
                    wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                    elapsed_ms = request_started.elapsed().as_millis() as u64,
                    "nats request response matched reply subject"
                );
                return Ok(payload);
            }
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                message_subject = %msg.subject,
                payload_len = msg.payload_len,
                "nats request response loop ignored unrelated message"
            );
        }
    }
}

pub async fn request_local(
    config_dir: impl AsRef<Path>,
    request_subject: &str,
    request_payload: &[u8],
    response_subject: &str,
    sid: u32,
    timeout_duration: Duration,
) -> Result<Vec<u8>, NatsError> {
    let endpoint = resolve_local_nats_endpoint(config_dir)?;
    request(
        &endpoint,
        request_subject,
        request_payload,
        response_subject,
        sid,
        timeout_duration,
    )
    .await
}

pub async fn request_with_client_config(
    config: &ClientConfig,
    request_subject: &str,
    request_payload: &[u8],
    response_subject: &str,
    sid: u32,
    timeout_duration: Duration,
) -> Result<Vec<u8>, NatsError> {
    let endpoint = config.resolved_nats_endpoint()?;
    request(
        &endpoint,
        request_subject,
        request_payload,
        response_subject,
        sid,
        timeout_duration,
    )
    .await
}

pub struct NatsSubscriber {
    endpoint: String,
    subject: String,
    sid: u32,
    queue: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NatsSubscriberReconnectOptions {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for NatsSubscriberReconnectOptions {
    fn default() -> Self {
        Self {
            initial_backoff: DEFAULT_CLIENT_RECONNECT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_CLIENT_RECONNECT_MAX_BACKOFF,
        }
    }
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

    pub async fn run_with_reconnect<F, Fut>(&self, mut handler: F) -> Result<(), NatsError>
    where
        F: FnMut(NatsMessage) -> Fut,
        Fut: std::future::Future<Output = Result<(), NatsError>>,
    {
        self.run_with_reconnect_options(&mut handler, NatsSubscriberReconnectOptions::default())
            .await
    }

    pub async fn run_with_reconnect_options<F, Fut>(
        &self,
        mut handler: F,
        options: NatsSubscriberReconnectOptions,
    ) -> Result<(), NatsError>
    where
        F: FnMut(NatsMessage) -> Fut,
        Fut: std::future::Future<Output = Result<(), NatsError>>,
    {
        let mut backoff = options.initial_backoff;
        loop {
            match self.run_once(&mut handler).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    if !is_retryable_nats_error(&err) {
                        return Err(err);
                    }
                    tracing::warn!(
                        endpoint = %self.endpoint,
                        subject = %self.subject,
                        sid = self.sid,
                        retry_in_ms = backoff.as_millis() as u64,
                        error = %err,
                        "nats subscriber disconnected; reconnecting"
                    );
                    sleep(backoff).await;
                    backoff = std::cmp::min(backoff.saturating_mul(2), options.max_backoff);
                }
            }
        }
    }

    pub async fn run<F, Fut>(&self, mut handler: F) -> Result<(), NatsError>
    where
        F: FnMut(NatsMessage) -> Fut,
        Fut: std::future::Future<Output = Result<(), NatsError>>,
    {
        self.run_once(&mut handler).await
    }

    async fn run_once<F, Fut>(&self, handler: &mut F) -> Result<(), NatsError>
    where
        F: FnMut(NatsMessage) -> Fut,
        Fut: std::future::Future<Output = Result<(), NatsError>>,
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
                return Err(NatsError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "nats socket closed",
                )));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
                continue;
            }
            if line == "PING" {
                writer_half.write_all(b"PONG\r\n").await?;
                writer_half.flush().await?;
                continue;
            }
            if line == "PONG" {
                continue;
            }
            if line.starts_with("-ERR") {
                return Err(NatsError::Protocol(format!("nats error: {line}")));
            }
            if line.starts_with("MSG ") {
                let header = parse_msg_line(line)?;
                let mut payload = vec![0u8; header.payload_len + 2];
                reader.read_exact(&mut payload).await?;
                payload.truncate(header.payload_len);
                let msg = NatsMessage {
                    subject: header.subject,
                    sid: header.sid,
                    reply_to: header.reply_to,
                    payload,
                };
                handler(msg.clone()).await?;
                if let Some(reply_to) = msg.reply_to {
                    writer_half
                        .write_all(format!("PUB {reply_to} 0\r\n\r\n").as_bytes())
                        .await?;
                    writer_half.flush().await?;
                }
            }
        }
    }
}

pub fn subscribe_local(
    config_dir: impl AsRef<Path>,
    subject: String,
    sid: u32,
) -> Result<NatsSubscriber, NatsError> {
    let endpoint = resolve_local_nats_endpoint(config_dir)?;
    Ok(NatsSubscriber::new(endpoint, subject, sid))
}

pub fn subscribe_with_client_config(
    config: &ClientConfig,
    subject: String,
    sid: u32,
) -> Result<NatsSubscriber, NatsError> {
    let endpoint = config.resolved_nats_endpoint()?;
    Ok(NatsSubscriber::new(endpoint, subject, sid))
}

#[derive(Debug, Deserialize)]
struct LocalHiveFile {
    nats: Option<LocalNatsSection>,
}

#[derive(Debug, Deserialize)]
struct LocalNatsSection {
    mode: Option<String>,
    port: Option<u16>,
    url: Option<String>,
}

pub fn resolve_local_nats_endpoint(config_dir: impl AsRef<Path>) -> Result<String, NatsError> {
    let config_dir = config_dir.as_ref();
    let hive_path = config_dir.join("hive.yaml");
    let data = std::fs::read_to_string(&hive_path).map_err(|err| {
        NatsError::Protocol(format!(
            "failed reading {}: {err}",
            hive_path.display()
        ))
    })?;
    let hive: LocalHiveFile = serde_yaml::from_str(&data)
        .map_err(|err| NatsError::Protocol(format!("invalid hive.yaml: {err}")))?;

    let endpoint = match hive.nats {
        Some(nats) => {
            if let Some(url) = nats.url {
                let url = url.trim();
                if !url.is_empty() {
                    url.to_string()
                } else {
                    default_local_endpoint(nats.mode.as_deref(), nats.port)
                }
            } else {
                default_local_endpoint(nats.mode.as_deref(), nats.port)
            }
        }
        None => DEFAULT_LOCAL_NATS_ENDPOINT.to_string(),
    };
    validate_local_endpoint(&endpoint)?;
    Ok(endpoint)
}

fn endpoint_to_addr(endpoint: &str) -> Result<String, NatsError> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(NatsError::Protocol("empty nats endpoint".to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("nats://") {
        if rest.is_empty() {
            return Err(NatsError::Protocol("invalid nats endpoint".to_string()));
        }
        return Ok(rest.to_string());
    }
    Ok(trimmed.to_string())
}

fn default_local_endpoint(mode: Option<&str>, port: Option<u16>) -> String {
    let mode = mode
        .unwrap_or("embedded")
        .trim()
        .to_ascii_lowercase();
    let port = port.unwrap_or(DEFAULT_LOCAL_NATS_PORT);
    if mode == "embedded" || mode == "client" {
        format!("nats://127.0.0.1:{port}")
    } else {
        DEFAULT_LOCAL_NATS_ENDPOINT.to_string()
    }
}

pub fn validate_local_endpoint(endpoint: &str) -> Result<(), NatsError> {
    let addr = endpoint_to_addr(endpoint)?;
    let host = extract_host(&addr)?;
    let normalized = host.trim_matches(['[', ']']);
    if normalized.eq_ignore_ascii_case("localhost")
        || normalized == "127.0.0.1"
        || normalized == "::1"
    {
        return Ok(());
    }
    Err(NatsError::Protocol(format!(
        "non-local nats endpoint rejected in strict mode: {endpoint}"
    )))
}

fn extract_host(addr: &str) -> Result<&str, NatsError> {
    if addr.is_empty() {
        return Err(NatsError::Protocol("invalid nats endpoint".to_string()));
    }
    if let Some(rest) = addr.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            if tail.starts_with(':') && !host.trim().is_empty() {
                return Ok(host);
            }
        }
        return Err(NatsError::Protocol(format!(
            "invalid nats endpoint address: {addr}"
        )));
    }
    if let Some((host, _port)) = addr.rsplit_once(':') {
        if host.trim().is_empty() {
            return Err(NatsError::Protocol(format!(
                "invalid nats endpoint address: {addr}"
            )));
        }
        return Ok(host);
    }
    Err(NatsError::Protocol(format!(
        "invalid nats endpoint address: {addr}"
    )))
}

fn sanitize_inbox_token(token: &str) -> String {
    let mut out = String::with_capacity(token.len().max(1));
    for ch in token.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn summarize_nats_line(line: &str) -> String {
    if line.len() <= NATS_LOG_LINE_MAX_LEN {
        return line.to_string();
    }
    let mut out = line[..NATS_LOG_LINE_MAX_LEN].to_string();
    out.push_str("...");
    out
}

#[derive(Debug)]
struct MsgFrameHeader {
    subject: String,
    sid: String,
    reply_to: Option<String>,
    payload_len: usize,
}

fn parse_msg_line(msg_line: &str) -> Result<MsgFrameHeader, NatsError> {
    let parts: Vec<&str> = msg_line.split_whitespace().collect();
    if parts.len() != 4 && parts.len() != 5 {
        return Err(NatsError::Protocol(format!("invalid MSG line: {msg_line}")));
    }
    if parts[0] != "MSG" {
        return Err(NatsError::Protocol(format!("invalid MSG line: {msg_line}")));
    }
    let subject = parts[1].trim();
    let sid = parts[2].trim();
    if subject.is_empty() || sid.is_empty() {
        return Err(NatsError::Protocol(format!("invalid MSG line: {msg_line}")));
    }
    let (reply_to, len_raw) = if parts.len() == 4 {
        (None, parts[3])
    } else {
        (Some(parts[3]), parts[4])
    };
    let payload_len = len_raw
        .parse::<usize>()
        .map_err(|err| NatsError::Protocol(format!("invalid MSG length '{len_raw}': {err}")))?;
    Ok(MsgFrameHeader {
        subject: subject.to_string(),
        sid: sid.to_string(),
        reply_to: reply_to.map(|v| v.to_string()),
        payload_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_config_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jsr-client-nats-test-{nanos}-{seq}"));
        fs::create_dir_all(&dir).expect("create temp config dir");
        dir
    }

    #[test]
    fn strict_accepts_local_loopback_endpoint() {
        validate_local_endpoint("nats://127.0.0.1:4222").expect("local endpoint should pass");
        validate_local_endpoint("nats://localhost:4222").expect("localhost endpoint should pass");
        validate_local_endpoint("nats://[::1]:4222").expect("ipv6 loopback should pass");
    }

    #[test]
    fn strict_rejects_non_local_endpoint() {
        let err = validate_local_endpoint("nats://192.168.8.220:4222")
            .expect_err("non-local endpoint should be rejected");
        assert!(
            err.to_string().contains("non-local nats endpoint rejected"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_local_nats_endpoint_uses_hive_and_rejects_external_url() {
        let dir = temp_config_dir();
        let hive = r#"
hive_id: sandbox
nats:
  mode: client
  url: "nats://10.0.0.5:4222"
"#;
        fs::write(dir.join("hive.yaml"), hive).expect("write hive.yaml");

        let err =
            resolve_local_nats_endpoint(&dir).expect_err("external endpoint should be rejected");
        assert!(
            err.to_string().contains("non-local nats endpoint rejected"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_local_nats_endpoint_defaults_to_embedded_loopback() {
        let dir = temp_config_dir();
        let hive = r#"
hive_id: sandbox
nats:
  mode: embedded
  port: 4333
"#;
        fs::write(dir.join("hive.yaml"), hive).expect("write hive.yaml");

        let endpoint =
            resolve_local_nats_endpoint(&dir).expect("embedded loopback endpoint should resolve");
        assert_eq!(endpoint, "nats://127.0.0.1:4333");
    }

    #[tokio::test]
    async fn request_uses_single_connection_for_publish_and_reply() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test nats listener");
        let addr = listener.local_addr().expect("listener local addr");
        let endpoint = format!("nats://{}", addr);
        let request_subject = "storage.metrics.get";
        let response_subject = "storage.metrics.reply.test";
        let sid = 77;
        let request_payload = br#"{"trace_id":"abc"}"#.to_vec();
        let response_payload = br#"{"status":"ok"}"#.to_vec();
        let response_payload_for_server = response_payload.clone();
        let request_payload_for_server = request_payload.clone();
        let response_subject_for_server = response_subject.to_string();
        let request_subject_for_server = request_subject.to_string();

        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("first connection accepted");
            let mut reader = BufReader::new(socket);

            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .expect("read CONNECT line");
            assert!(
                line.starts_with("CONNECT "),
                "expected CONNECT line, got {:?}",
                line.trim_end()
            );

            line.clear();
            reader
                .read_line(&mut line)
                .await
                .expect("read SUB line");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("SUB {response_subject_for_server} {sid}")
            );

            line.clear();
            reader
                .read_line(&mut line)
                .await
                .expect("read PING line");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");

            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write PONG");
            reader.get_mut().flush().await.expect("flush PONG");

            line.clear();
            reader
                .read_line(&mut line)
                .await
                .expect("read PUB line");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!(
                    "PUB {} {}",
                    request_subject_for_server,
                    request_payload_for_server.len()
                )
            );

            let mut published = vec![0u8; request_payload_for_server.len() + 2];
            reader
                .read_exact(&mut published)
                .await
                .expect("read PUB payload");
            published.truncate(request_payload_for_server.len());
            assert_eq!(published, request_payload_for_server);

            reader
                .get_mut()
                .write_all(
                    format!(
                        "MSG {response_subject_for_server} {sid} {}\r\n",
                        response_payload_for_server.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write MSG header");
            reader
                .get_mut()
                .write_all(&response_payload_for_server)
                .await
                .expect("write MSG payload");
            reader
                .get_mut()
                .write_all(b"\r\n")
                .await
                .expect("write MSG terminator");
            reader.get_mut().flush().await.expect("flush MSG response");

            let second_accept = tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await;
            assert!(
                second_accept.is_err(),
                "unexpected second connection from request path"
            );
        });

        let response = request(
            &endpoint,
            request_subject,
            &request_payload,
            response_subject,
            sid,
            Duration::from_secs(2),
        )
        .await
        .expect("request should succeed");
        assert_eq!(response, response_payload);

        server_task.await.expect("server task should finish");
    }

    #[tokio::test]
    async fn persistent_client_reconnects_after_publish_sync_drop() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test nats listener");
        let endpoint = format!("nats://{}", listener.local_addr().expect("listener local addr"));
        let subject = "storage.metrics.get";
        let payload = br#"{"trace_id":"reconnect"}"#.to_vec();
        let payload_for_server = payload.clone();
        let subject_for_server = subject.to_string();

        let server_task = tokio::spawn(async move {
            let (first_socket, _) = listener.accept().await.expect("first connection accepted");
            let mut first_reader = BufReader::new(first_socket);
            let mut line = String::new();

            first_reader
                .read_line(&mut line)
                .await
                .expect("read first CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT in first connection");
            line.clear();

            first_reader
                .read_line(&mut line)
                .await
                .expect("read first PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            first_reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write first PONG");
            first_reader
                .get_mut()
                .flush()
                .await
                .expect("flush first PONG");
            line.clear();

            first_reader
                .read_line(&mut line)
                .await
                .expect("read first PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("PUB {} {}", subject_for_server, payload_for_server.len())
            );
            let mut first_payload = vec![0u8; payload_for_server.len() + 2];
            first_reader
                .read_exact(&mut first_payload)
                .await
                .expect("read first payload");

            // Drop first socket before publish sync PONG to force reconnect path.
            drop(first_reader);

            let (second_socket, _) = listener.accept().await.expect("second connection accepted");
            let mut second_reader = BufReader::new(second_socket);
            let mut line = String::new();

            second_reader
                .read_line(&mut line)
                .await
                .expect("read second CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT in second connection");
            line.clear();

            second_reader
                .read_line(&mut line)
                .await
                .expect("read second PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            second_reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write second handshake PONG");
            second_reader
                .get_mut()
                .flush()
                .await
                .expect("flush second handshake PONG");
            line.clear();

            second_reader
                .read_line(&mut line)
                .await
                .expect("read second PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("PUB {} {}", subject_for_server, payload_for_server.len())
            );
            let mut second_payload = vec![0u8; payload_for_server.len() + 2];
            second_reader
                .read_exact(&mut second_payload)
                .await
                .expect("read second payload");
            second_payload.truncate(payload_for_server.len());
            assert_eq!(second_payload, payload_for_server);
            line.clear();

            second_reader
                .read_line(&mut line)
                .await
                .expect("read second publish sync PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            second_reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write second publish sync PONG");
            second_reader
                .get_mut()
                .flush()
                .await
                .expect("flush second publish sync PONG");
        });

        let client = NatsClient::new(endpoint);
        client
            .publish(subject, &payload)
            .await
            .expect("persistent client publish should recover after reconnect");
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.reconnects, 1, "expected exactly one reconnect");
        assert_eq!(metrics.in_flight, 0, "publish should not leave in-flight requests");
        server_task.await.expect("server task should finish");
    }

    #[tokio::test]
    async fn request_timeout_records_metrics_snapshot() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test nats listener");
        let endpoint = format!("nats://{}", listener.local_addr().expect("listener local addr"));
        let request_subject = "storage.metrics.get";
        let response_subject = "storage.metrics.reply.timeout";
        let sid = 4242;
        let request_payload = br#"{"trace_id":"timeout"}"#.to_vec();
        let request_payload_for_server = request_payload.clone();
        let request_subject_for_server = request_subject.to_string();
        let response_subject_for_server = response_subject.to_string();

        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("connection accepted");
            let mut reader = BufReader::new(socket);
            let mut line = String::new();

            reader.read_line(&mut line).await.expect("read CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT");
            line.clear();

            reader.read_line(&mut line).await.expect("read handshake PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write handshake PONG");
            reader.get_mut().flush().await.expect("flush handshake PONG");
            line.clear();

            reader.read_line(&mut line).await.expect("read UNSUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("UNSUB {sid}")
            );
            line.clear();

            reader.read_line(&mut line).await.expect("read SUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("SUB {response_subject_for_server} {sid}")
            );
            line.clear();

            reader.read_line(&mut line).await.expect("read sync PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write sync PONG");
            reader.get_mut().flush().await.expect("flush sync PONG");
            line.clear();

            reader.read_line(&mut line).await.expect("read PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!(
                    "PUB {} {}",
                    request_subject_for_server,
                    request_payload_for_server.len()
                )
            );
            let mut published = vec![0u8; request_payload_for_server.len() + 2];
            reader
                .read_exact(&mut published)
                .await
                .expect("read request payload");
            tokio::time::sleep(Duration::from_millis(200)).await;
        });

        let client = NatsClient::with_options(
            endpoint,
            NatsClientOptions {
                reconnect_attempts: 1,
                ..NatsClientOptions::default()
            },
        );
        let err = client
            .request(
                request_subject,
                &request_payload,
                response_subject,
                sid,
                Duration::from_millis(100),
            )
            .await
            .expect_err("request must timeout");
        assert!(
            err.to_string().contains("timeout"),
            "expected timeout error, got {err}"
        );
        let metrics = client.metrics_snapshot();
        assert_eq!(metrics.timeouts, 1, "expected timeout counter increment");
        assert_eq!(metrics.in_flight, 0, "in-flight must be released after timeout");
        let last_error = metrics.last_error.unwrap_or_default();
        assert!(
            last_error.contains("timeout"),
            "expected last_error with timeout, got {last_error}"
        );

        server_task.await.expect("server task should finish");
    }

    #[tokio::test]
    async fn multiplexed_request_reuses_wildcard_subscription() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test nats listener");
        let endpoint = format!("nats://{}", listener.local_addr().expect("listener local addr"));
        let request_subject = "storage.metrics.get";
        let response_subscription = "storage.metrics.reply.*";
        let response_subject_1 = "storage.metrics.reply.req-1";
        let response_subject_2 = "storage.metrics.reply.req-2";
        let sid = 9001;
        let request_payload_1 = br#"{"trace_id":"req-1"}"#.to_vec();
        let request_payload_2 = br#"{"trace_id":"req-2"}"#.to_vec();
        let response_payload_1 = br#"{"status":"ok","trace_id":"req-1"}"#.to_vec();
        let response_payload_2 = br#"{"status":"ok","trace_id":"req-2"}"#.to_vec();
        let request_payload_1_for_server = request_payload_1.clone();
        let request_payload_2_for_server = request_payload_2.clone();
        let response_payload_1_for_server = response_payload_1.clone();
        let response_payload_2_for_server = response_payload_2.clone();

        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("connection accepted");
            let mut reader = BufReader::new(socket);
            let mut line = String::new();

            reader.read_line(&mut line).await.expect("read CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT");
            line.clear();

            reader.read_line(&mut line).await.expect("read initial PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write initial PONG");
            reader.get_mut().flush().await.expect("flush initial PONG");
            line.clear();

            // First request installs wildcard subscription once.
            reader.read_line(&mut line).await.expect("read SUB wildcard");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("SUB {response_subscription} {sid}")
            );
            line.clear();

            reader
                .read_line(&mut line)
                .await
                .expect("read sync PING after SUB");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write sync PONG after SUB");
            reader
                .get_mut()
                .flush()
                .await
                .expect("flush sync PONG after SUB");
            line.clear();

            reader.read_line(&mut line).await.expect("read first PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("PUB {request_subject} {}", request_payload_1_for_server.len())
            );
            let mut payload_1 = vec![0u8; request_payload_1_for_server.len() + 2];
            reader
                .read_exact(&mut payload_1)
                .await
                .expect("read first payload");
            payload_1.truncate(request_payload_1_for_server.len());
            assert_eq!(payload_1, request_payload_1_for_server);
            line.clear();

            reader
                .get_mut()
                .write_all(
                    format!(
                        "MSG {response_subject_1} {sid} {}\r\n",
                        response_payload_1_for_server.len()
                    )
                        .as_bytes(),
                )
                .await
                .expect("write first MSG header");
            reader
                .get_mut()
                .write_all(&response_payload_1_for_server)
                .await
                .expect("write first MSG payload");
            reader
                .get_mut()
                .write_all(b"\r\n")
                .await
                .expect("write first MSG terminator");
            reader.get_mut().flush().await.expect("flush first MSG");

            // Second request should not send SUB again.
            reader.read_line(&mut line).await.expect("read second PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("PUB {request_subject} {}", request_payload_2_for_server.len())
            );
            let mut payload_2 = vec![0u8; request_payload_2_for_server.len() + 2];
            reader
                .read_exact(&mut payload_2)
                .await
                .expect("read second payload");
            payload_2.truncate(request_payload_2_for_server.len());
            assert_eq!(payload_2, request_payload_2_for_server);
            line.clear();

            reader
                .get_mut()
                .write_all(
                    format!(
                        "MSG {response_subject_2} {sid} {}\r\n",
                        response_payload_2_for_server.len()
                    )
                        .as_bytes(),
                )
                .await
                .expect("write second MSG header");
            reader
                .get_mut()
                .write_all(&response_payload_2_for_server)
                .await
                .expect("write second MSG payload");
            reader
                .get_mut()
                .write_all(b"\r\n")
                .await
                .expect("write second MSG terminator");
            reader.get_mut().flush().await.expect("flush second MSG");
        });

        let client = NatsClient::new(endpoint);
        let first = client
            .request_multiplexed(
                request_subject,
                &request_payload_1,
                response_subject_1,
                response_subscription,
                sid,
                Duration::from_secs(2),
            )
            .await
            .expect("first multiplexed request should succeed");
        assert_eq!(first, response_payload_1);

        let second = client
            .request_multiplexed(
                request_subject,
                &request_payload_2,
                response_subject_2,
                response_subscription,
                sid,
                Duration::from_secs(2),
            )
            .await
            .expect("second multiplexed request should succeed");
        assert_eq!(second, response_payload_2);

        server_task.await.expect("server task should finish");
    }

    #[tokio::test]
    async fn subscriber_run_with_reconnect_recovers_after_socket_drop() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test nats listener");
        let endpoint = format!("nats://{}", listener.local_addr().expect("listener local addr"));
        let subject = "storage.metrics.get".to_string();
        let sid = 222;
        let payload = br#"{"probe":"ok"}"#.to_vec();
        let payload_for_server = payload.clone();
        let subject_for_server = subject.clone();
        let (delivered_tx, delivered_rx) = oneshot::channel::<()>();
        let delivered = Arc::new(std::sync::Mutex::new(Some(delivered_tx)));

        let server_task = tokio::spawn(async move {
            // First connection: handshake then immediate drop.
            let (first_socket, _) = listener
                .accept()
                .await
                .expect("first connection accepted");
            let mut first_reader = BufReader::new(first_socket);
            let mut line = String::new();
            first_reader
                .read_line(&mut line)
                .await
                .expect("read first CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT in first connection");
            line.clear();
            first_reader
                .read_line(&mut line)
                .await
                .expect("read first SUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("SUB {subject_for_server} {sid}")
            );
            line.clear();
            first_reader
                .read_line(&mut line)
                .await
                .expect("read first PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            first_reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write first PONG");
            first_reader
                .get_mut()
                .flush()
                .await
                .expect("flush first PONG");
            drop(first_reader);

            // Second connection: handshake + deliver a message.
            let (second_socket, _) = listener
                .accept()
                .await
                .expect("second connection accepted");
            let mut second_reader = BufReader::new(second_socket);
            let mut line = String::new();
            second_reader
                .read_line(&mut line)
                .await
                .expect("read second CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT in second connection");
            line.clear();
            second_reader
                .read_line(&mut line)
                .await
                .expect("read second SUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("SUB {subject_for_server} {sid}")
            );
            line.clear();
            second_reader
                .read_line(&mut line)
                .await
                .expect("read second PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            second_reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write second PONG");
            second_reader
                .get_mut()
                .flush()
                .await
                .expect("flush second PONG");

            second_reader
                .get_mut()
                .write_all(
                    format!("MSG {subject_for_server} {sid} {}\r\n", payload_for_server.len())
                        .as_bytes(),
                )
                .await
                .expect("write MSG header");
            second_reader
                .get_mut()
                .write_all(&payload_for_server)
                .await
                .expect("write MSG payload");
            second_reader
                .get_mut()
                .write_all(b"\r\n")
                .await
                .expect("write MSG terminator");
            second_reader
                .get_mut()
                .flush()
                .await
                .expect("flush MSG");
        });

        let subscriber = NatsSubscriber::new(endpoint, subject, sid);
        let delivered_for_task = Arc::clone(&delivered);
        let expected_payload = payload.clone();
        let subscriber_task = tokio::spawn(async move {
            let _ = subscriber
                .run_with_reconnect_options(
                    move |msg| {
                        let delivered_for_handler = Arc::clone(&delivered_for_task);
                        let expected_payload = expected_payload.clone();
                        async move {
                            if msg.payload == expected_payload {
                                if let Some(tx) = delivered_for_handler.lock().expect("lock").take()
                                {
                                    let _ = tx.send(());
                                }
                            }
                            Ok(())
                        }
                    },
                    NatsSubscriberReconnectOptions {
                        initial_backoff: Duration::from_millis(25),
                        max_backoff: Duration::from_millis(100),
                    },
                )
                .await;
        });

        tokio::time::timeout(Duration::from_secs(2), delivered_rx)
            .await
            .expect("delivery signal timeout")
            .expect("delivery signal channel");

        subscriber_task.abort();
        let _ = subscriber_task.await;
        server_task.await.expect("server task should finish");
    }

    #[test]
    fn session_inbox_subject_sanitizes_trace_token() {
        let client = NatsClient::new("nats://127.0.0.1:4222");
        let subject = client.inbox_reply_subject("trace.id/with space");
        assert!(
            subject.starts_with("_INBOX.JSR."),
            "expected session inbox prefix, got {subject}"
        );
        assert!(
            subject.ends_with(".trace_id_with_space"),
            "expected sanitized suffix, got {subject}"
        );
    }

    #[tokio::test]
    async fn request_with_session_inbox_reuses_session_subscription() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test nats listener");
        let endpoint = format!("nats://{}", listener.local_addr().expect("listener local addr"));
        let request_subject = "storage.metrics.get";
        let request_payload_1 = br#"{"trace_id":"t1"}"#.to_vec();
        let request_payload_2 = br#"{"trace_id":"t2"}"#.to_vec();
        let response_payload_1 = br#"{"status":"ok","trace_id":"t1"}"#.to_vec();
        let response_payload_2 = br#"{"status":"ok","trace_id":"t2"}"#.to_vec();
        let request_payload_1_for_server = request_payload_1.clone();
        let request_payload_2_for_server = request_payload_2.clone();
        let response_payload_1_for_server = response_payload_1.clone();
        let response_payload_2_for_server = response_payload_2.clone();

        let client = NatsClient::new(endpoint.clone());
        let response_subject_1 = client.inbox_reply_subject("t1");
        let response_subject_2 = client.inbox_reply_subject("t2");
        let response_subject_1_for_server = response_subject_1.clone();
        let response_subject_2_for_server = response_subject_2.clone();

        let server_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("connection accepted");
            let mut reader = BufReader::new(socket);
            let mut line = String::new();

            reader.read_line(&mut line).await.expect("read CONNECT");
            assert!(line.starts_with("CONNECT "), "expected CONNECT");
            line.clear();

            reader.read_line(&mut line).await.expect("read initial PING");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write initial PONG");
            reader.get_mut().flush().await.expect("flush initial PONG");
            line.clear();

            reader.read_line(&mut line).await.expect("read SUB wildcard");
            let sub_line = line.trim_end_matches(['\r', '\n']).to_string();
            let parts: Vec<&str> = sub_line.split_whitespace().collect();
            assert_eq!(parts.len(), 3, "expected SUB <subject> <sid>, got {sub_line}");
            assert_eq!(parts[0], "SUB");
            assert!(
                parts[1].starts_with("_INBOX.JSR."),
                "expected inbox wildcard subject, got {}",
                parts[1]
            );
            assert!(parts[1].ends_with(".*"), "expected wildcard suffix, got {}", parts[1]);
            let sid = parts[2];
            line.clear();

            reader
                .read_line(&mut line)
                .await
                .expect("read sync PING after SUB");
            assert_eq!(line.trim_end_matches(['\r', '\n']), "PING");
            reader
                .get_mut()
                .write_all(b"PONG\r\n")
                .await
                .expect("write sync PONG after SUB");
            reader
                .get_mut()
                .flush()
                .await
                .expect("flush sync PONG after SUB");
            line.clear();

            reader.read_line(&mut line).await.expect("read first PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("PUB {request_subject} {}", request_payload_1_for_server.len())
            );
            let mut payload_1 = vec![0u8; request_payload_1_for_server.len() + 2];
            reader
                .read_exact(&mut payload_1)
                .await
                .expect("read first payload");
            payload_1.truncate(request_payload_1_for_server.len());
            assert_eq!(payload_1, request_payload_1_for_server);
            line.clear();

            reader
                .get_mut()
                .write_all(
                    format!(
                        "MSG {response_subject_1_for_server} {sid} {}\r\n",
                        response_payload_1_for_server.len()
                    )
                        .as_bytes(),
                )
                .await
                .expect("write first MSG header");
            reader
                .get_mut()
                .write_all(&response_payload_1_for_server)
                .await
                .expect("write first MSG payload");
            reader
                .get_mut()
                .write_all(b"\r\n")
                .await
                .expect("write first MSG terminator");
            reader.get_mut().flush().await.expect("flush first MSG");

            // Second request should not emit SUB again.
            reader.read_line(&mut line).await.expect("read second PUB");
            assert_eq!(
                line.trim_end_matches(['\r', '\n']),
                format!("PUB {request_subject} {}", request_payload_2_for_server.len())
            );
            let mut payload_2 = vec![0u8; request_payload_2_for_server.len() + 2];
            reader
                .read_exact(&mut payload_2)
                .await
                .expect("read second payload");
            payload_2.truncate(request_payload_2_for_server.len());
            assert_eq!(payload_2, request_payload_2_for_server);
            line.clear();

            reader
                .get_mut()
                .write_all(
                    format!(
                        "MSG {response_subject_2_for_server} {sid} {}\r\n",
                        response_payload_2_for_server.len()
                    )
                        .as_bytes(),
                )
                .await
                .expect("write second MSG header");
            reader
                .get_mut()
                .write_all(&response_payload_2_for_server)
                .await
                .expect("write second MSG payload");
            reader
                .get_mut()
                .write_all(b"\r\n")
                .await
                .expect("write second MSG terminator");
            reader.get_mut().flush().await.expect("flush second MSG");
        });

        let first = client
            .request_with_session_inbox(
                request_subject,
                &request_payload_1,
                &response_subject_1,
                Duration::from_secs(2),
            )
            .await
            .expect("first session-inbox request should succeed");
        assert_eq!(first, response_payload_1);

        let second = client
            .request_with_session_inbox(
                request_subject,
                &request_payload_2,
                &response_subject_2,
                Duration::from_secs(2),
            )
            .await
            .expect("second session-inbox request should succeed");
        assert_eq!(second, response_payload_2);

        server_task.await.expect("server task should finish");
    }
}
