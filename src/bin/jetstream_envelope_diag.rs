use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use json_router::nats::{publish, NatsSubscriber};
use jsr_client::nats::NATS_ENVELOPE_SCHEMA_VERSION;

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JetstreamEnvelope {
    schema_version: u16,
    trace_id: String,
    seq: u64,
    sent_at_ms: u64,
    payload: Value,
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

    match mode.as_str() {
        "server" => {
            let sid = env_u32("JETSTREAM_DIAG_SID", 52_001);
            let queue = env_or(
                "JETSTREAM_DIAG_QUEUE",
                "durable.jetstream.diag.envelope",
            );
            let fail_first_n = env_u64("JETSTREAM_DIAG_FAIL_FIRST_N", 0);
            run_server(endpoint, subject, sid, queue, fail_first_n).await
        }
        "client" => {
            let loops = env_u64("JETSTREAM_DIAG_LOOPS", 10);
            let interval_ms = env_u64("JETSTREAM_DIAG_INTERVAL_MS", 100);
            let trace_prefix = env_or("JETSTREAM_DIAG_TRACE_PREFIX", "js-env");
            run_client(endpoint, subject, loops, interval_ms, trace_prefix).await
        }
        other => Err(format!(
            "invalid JETSTREAM_DIAG_MODE={other}; expected server|client"
        )
        .into()),
    }
}

async fn run_server(
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
        sid,
        queue = %queue,
        fail_first_n,
        "jetstream diag server starting"
    );

    let subscriber = NatsSubscriber::new(endpoint, subject, sid).with_queue(queue);
    let received = Arc::new(AtomicU64::new(0));

    subscriber
        .run({
            let received = Arc::clone(&received);
            move |payload| {
                let received = Arc::clone(&received);
                async move {
                    let recv_idx = received.fetch_add(1, Ordering::Relaxed);
                    let envelope: JetstreamEnvelope = serde_json::from_slice(&payload).map_err(|err| {
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
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
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
            }
        })
        .await
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;

    Ok(())
}

async fn run_client(
    endpoint: String,
    subject: String,
    loops: u64,
    interval_ms: u64,
    trace_prefix: String,
) -> Result<(), DynError> {
    tracing::info!(
        mode = "client",
        endpoint = %endpoint,
        subject = %subject,
        loops,
        interval_ms,
        trace_prefix = %trace_prefix,
        "jetstream diag client starting"
    );

    for seq in 0..loops {
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
        publish(&endpoint, &subject, &body).await?;
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

    tracing::info!(
        mode = "client",
        loops,
        "jetstream diag client completed"
    );

    Ok(())
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
