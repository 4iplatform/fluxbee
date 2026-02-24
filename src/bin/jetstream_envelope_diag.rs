use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::nats::{
    NatsClient, NatsError as ClientNatsError, NatsSubscriber as ClientNatsSubscriber,
    NATS_ENVELOPE_SCHEMA_VERSION,
};
use json_router::nats::{publish as router_publish, NatsSubscriber as RouterNatsSubscriber};

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JetstreamEnvelope {
    schema_version: u16,
    trace_id: String,
    seq: u64,
    sent_at_ms: u64,
    payload: Value,
}

#[derive(Debug, Clone, Copy)]
enum DiagStack {
    RouterNats,
    JsrClient,
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let mode = env_or("JETSTREAM_DIAG_MODE", "client");
    let endpoint = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let subject = env_or("JETSTREAM_DIAG_SUBJECT", "jetstream.diag.envelope");
    let stack = parse_stack(&env_or("JETSTREAM_DIAG_STACK", "router_nats"))?;

    match mode.as_str() {
        "server" => {
            let sid = env_u32("JETSTREAM_DIAG_SID", 52_001);
            let queue = env_or("JETSTREAM_DIAG_QUEUE", "durable.jetstream.diag.envelope");
            let fail_first_n = env_u64("JETSTREAM_DIAG_FAIL_FIRST_N", 0);
            run_server(stack, endpoint, subject, sid, queue, fail_first_n).await
        }
        "client" => {
            let loops = env_u64("JETSTREAM_DIAG_LOOPS", 10);
            let interval_ms = env_u64("JETSTREAM_DIAG_INTERVAL_MS", 100);
            let trace_prefix = env_or("JETSTREAM_DIAG_TRACE_PREFIX", "js-env");
            let seq_start = env_u64("JETSTREAM_DIAG_SEQ_START", 0);
            run_client(
                stack,
                endpoint,
                subject,
                loops,
                interval_ms,
                trace_prefix,
                seq_start,
            )
            .await
        }
        other => Err(format!("invalid JETSTREAM_DIAG_MODE={other}; expected server|client").into()),
    }
}

async fn run_server(
    stack: DiagStack,
    endpoint: String,
    subject: String,
    sid: u32,
    queue: String,
    fail_first_n: u64,
) -> Result<(), DynError> {
    tracing::info!(
        mode = "server",
        endpoint = %endpoint,
        subject = %subject,
        stack = %stack_label(stack),
        sid,
        queue = %queue,
        fail_first_n,
        "jetstream diag server starting"
    );

    let received = Arc::new(AtomicU64::new(0));
    match stack {
        DiagStack::RouterNats => {
            run_server_router_nats(endpoint, subject, sid, queue, fail_first_n, received).await
        }
        DiagStack::JsrClient => {
            run_server_jsr_client(endpoint, subject, sid, queue, fail_first_n, received).await
        }
    }
}

async fn run_client(
    stack: DiagStack,
    endpoint: String,
    subject: String,
    loops: u64,
    interval_ms: u64,
    trace_prefix: String,
    seq_start: u64,
) -> Result<(), DynError> {
    tracing::info!(
        mode = "client",
        endpoint = %endpoint,
        subject = %subject,
        stack = %stack_label(stack),
        loops,
        interval_ms,
        seq_start,
        trace_prefix = %trace_prefix,
        "jetstream diag client starting"
    );

    let client = match stack {
        DiagStack::RouterNats => None,
        DiagStack::JsrClient => Some(NatsClient::new(endpoint.clone())),
    };

    for offset in 0..loops {
        let seq = seq_start.saturating_add(offset);
        let trace_id = format!("{}-{}-{}", trace_prefix, now_unix_ms(), Uuid::new_v4());
        let envelope = JetstreamEnvelope {
            schema_version: NATS_ENVELOPE_SCHEMA_VERSION,
            trace_id: trace_id.clone(),
            seq,
            sent_at_ms: now_unix_ms(),
            payload: json!({
                "opaque": true,
                "kind": "diag",
                "seq": seq,
                "blob": format!("blob-{}", Uuid::new_v4()),
                "nested": {
                    "a": 1,
                    "b": ["x", "y", "z"]
                }
            }),
        };
        let body = serde_json::to_vec(&envelope)?;
        match &client {
            Some(c) => c.publish(&subject, &body).await?,
            None => router_publish(&endpoint, &subject, &body).await?,
        }
        tracing::info!(
            mode = "client",
            seq,
            trace = %compact_trace(&trace_id),
            payload_bytes = body.len(),
            "jetstream diag client published"
        );

        if interval_ms > 0 {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }

    tracing::info!(mode = "client", loops, "jetstream diag client completed");

    Ok(())
}

async fn run_server_router_nats(
    endpoint: String,
    subject: String,
    sid: u32,
    queue: String,
    fail_first_n: u64,
    received: Arc<AtomicU64>,
) -> Result<(), DynError> {
    let subscriber = RouterNatsSubscriber::new(endpoint, subject, sid).with_queue(queue);
    let mut reconnect_backoff = Duration::from_millis(100);
    let reconnect_backoff_max = Duration::from_secs(2);
    loop {
        let result = subscriber
            .run({
                let received = Arc::clone(&received);
                move |payload| {
                    let received = Arc::clone(&received);
                    async move { handle_server_payload(&payload, &received, fail_first_n) }
                }
            })
            .await;
        match result {
            Ok(()) => {
                reconnect_backoff = Duration::from_millis(100);
            }
            Err(err) => {
                tracing::warn!(
                    mode = "server",
                    stack = "router_nats",
                    retry_in_ms = reconnect_backoff.as_millis() as u64,
                    error = %err,
                    "jetstream diag server subscriber disconnected; reconnecting"
                );
                tokio::time::sleep(reconnect_backoff).await;
                reconnect_backoff =
                    std::cmp::min(reconnect_backoff.saturating_mul(2), reconnect_backoff_max);
            }
        }
    }
}

async fn run_server_jsr_client(
    endpoint: String,
    subject: String,
    sid: u32,
    queue: String,
    fail_first_n: u64,
    received: Arc<AtomicU64>,
) -> Result<(), DynError> {
    let subscriber = ClientNatsSubscriber::new(endpoint, subject, sid).with_queue(queue);
    subscriber
        .run_with_reconnect(move |msg| {
            let received = Arc::clone(&received);
            async move {
                match handle_server_payload(&msg.payload, &received, fail_first_n) {
                    Ok(()) => Ok(()),
                    Err(err) => Err(ClientNatsError::Io(err)),
                }
            }
        })
        .await?;
    Ok(())
}

fn handle_server_payload(
    payload: &[u8],
    received: &Arc<AtomicU64>,
    fail_first_n: u64,
) -> Result<(), io::Error> {
    let recv_idx = received.fetch_add(1, Ordering::Relaxed);
    let envelope: JetstreamEnvelope = serde_json::from_slice(payload).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid envelope payload: {err}"),
        )
    })?;
    let trace = compact_trace(&envelope.trace_id);
    tracing::info!(
        mode = "server",
        seq = envelope.seq,
        recv_idx,
        trace = %trace,
        payload_bytes = payload.len(),
        "jetstream diag server received"
    );
    if recv_idx < fail_first_n {
        tracing::warn!(
            mode = "server",
            seq = envelope.seq,
            recv_idx,
            trace = %trace,
            "jetstream diag server intentionally not acking"
        );
        return Err(io::Error::other(
            "intentional no-ack for redelivery diagnostics",
        ));
    }
    tracing::info!(
        mode = "server",
        seq = envelope.seq,
        recv_idx,
        trace = %trace,
        "jetstream diag server acked"
    );
    Ok(())
}

fn parse_stack(raw: &str) -> Result<DiagStack, DynError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "router_nats" | "router" => Ok(DiagStack::RouterNats),
        "jsr_client" | "jsr" => Ok(DiagStack::JsrClient),
        other => Err(format!(
            "invalid JETSTREAM_DIAG_STACK={other}; expected router_nats|jsr_client"
        )
        .into()),
    }
}

fn stack_label(stack: DiagStack) -> &'static str {
    match stack {
        DiagStack::RouterNats => "router_nats",
        DiagStack::JsrClient => "jsr_client",
    }
}

fn compact_trace(trace_id: &str) -> String {
    let cleaned = trace_id.trim();
    if cleaned.len() <= 14 {
        return cleaned.to_string();
    }
    format!("{}..{}", &cleaned[..6], &cleaned[cleaned.len() - 6..])
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
