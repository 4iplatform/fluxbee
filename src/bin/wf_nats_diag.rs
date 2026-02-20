use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsr_client::nats::{
    publish, NatsClient, NatsRequestEnvelope, NatsResponseEnvelope, NatsSubscriber,
};
use serde_json::{json, Value};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn run_server(endpoint: String, subject: String, sid: u32) -> Result<(), Box<dyn Error>> {
    tracing::info!(
        mode = "server",
        endpoint = %endpoint,
        subject = %subject,
        sid = sid,
        "wf nats diag server starting"
    );
    let subscriber = NatsSubscriber::new(endpoint.clone(), subject.clone(), sid);
    subscriber
        .run_with_reconnect(move |msg| {
            let endpoint = endpoint.clone();
            let subject = subject.clone();
            async move {
                let handler_started = std::time::Instant::now();
                tracing::info!(
                    mode = "server",
                    subject = %msg.subject,
                    sid = %msg.sid,
                    reply_to = ?msg.reply_to,
                    payload_bytes = msg.payload.len(),
                    "wf nats diag server message received"
                );

                let (trace_id, reply_subject, payload) =
                    match serde_json::from_slice::<NatsRequestEnvelope<Value>>(&msg.payload) {
                        Ok(req) => (
                            req.trace_id,
                            req.reply_subject,
                            req.payload.unwrap_or(Value::Null),
                        ),
                        Err(err) => {
                            tracing::warn!(
                                mode = "server",
                                error = %err,
                                "wf nats diag server failed to parse request envelope"
                            );
                            let fallback_trace = Uuid::new_v4().to_string();
                            let fallback_reply =
                                msg.reply_to.clone().unwrap_or_else(|| "".to_string());
                            (fallback_trace, fallback_reply, Value::Null)
                        }
                    };

                if reply_subject.trim().is_empty() {
                    tracing::warn!(
                        mode = "server",
                        trace_id = %trace_id,
                        "wf nats diag server missing reply subject, skipping response"
                    );
                    return Ok(());
                }

                let response_payload = json!({
                    "echo_subject": msg.subject,
                    "echo_payload": payload,
                    "server_epoch_ms": now_epoch_ms(),
                });
                let response = NatsResponseEnvelope::ok(subject.clone(), trace_id.clone(), response_payload);
                let body = serde_json::to_vec(&response)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

                tracing::info!(
                    mode = "server",
                    trace_id = %trace_id,
                    reply_subject = %reply_subject,
                    response_bytes = body.len(),
                    handler_elapsed_ms = handler_started.elapsed().as_millis() as u64,
                    "wf nats diag server response publish start"
                );
                publish(&endpoint, &reply_subject, &body)
                    .await
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
                tracing::info!(
                    mode = "server",
                    trace_id = %trace_id,
                    reply_subject = %reply_subject,
                    response_bytes = body.len(),
                    total_elapsed_ms = handler_started.elapsed().as_millis() as u64,
                    "wf nats diag server response published"
                );
                Ok(())
            }
        })
        .await
        .map_err(|e| -> Box<dyn Error> { Box::new(e) })
}

async fn run_client(
    endpoint: String,
    subject: String,
    timeout_secs: u64,
    loops: u64,
    interval_ms: u64,
    trace_prefix: String,
) -> Result<(), Box<dyn Error>> {
    tracing::info!(
        mode = "client",
        endpoint = %endpoint,
        subject = %subject,
        timeout_secs = timeout_secs,
        loops = loops,
        interval_ms = interval_ms,
        trace_prefix = %trace_prefix,
        "wf nats diag client starting"
    );

    let timeout = Duration::from_secs(timeout_secs);
    let sleep_between = Duration::from_millis(interval_ms);
    let client = NatsClient::new(endpoint.clone());

    for i in 0..loops {
        let trace_id = format!("{trace_prefix}-{}-{}", now_epoch_ms(), Uuid::new_v4());
        let reply_subject = client.inbox_reply_subject(&trace_id);
        let payload = json!({
            "probe": "wf_nats_diag",
            "seq": i,
            "client_epoch_ms": now_epoch_ms(),
        });
        let request = NatsRequestEnvelope::new(
            subject.clone(),
            trace_id.clone(),
            reply_subject.clone(),
            Some(payload),
        );
        let body = serde_json::to_vec(&request)?;

        tracing::info!(
            mode = "client",
            trace_id = %trace_id,
            request_subject = %subject,
            reply_subject = %reply_subject,
            request_bytes = body.len(),
            "wf nats diag client request send"
        );

        let started = std::time::Instant::now();
        match client
            .request_with_session_inbox(&subject, &body, &reply_subject, timeout)
            .await
        {
            Ok(reply) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                tracing::info!(
                    mode = "client",
                    trace_id = %trace_id,
                    reply_subject = %reply_subject,
                    elapsed_ms = elapsed_ms,
                    response_bytes = reply.len(),
                    "wf nats diag client response received"
                );
                match serde_json::from_slice::<NatsResponseEnvelope<Value>>(&reply) {
                    Ok(decoded) => {
                        tracing::info!(
                            mode = "client",
                            trace_id = %trace_id,
                            response_status = %decoded.status,
                            response_action = %decoded.action,
                            response_trace = %decoded.trace_id,
                            has_payload = decoded.payload.is_some(),
                            error_code = ?decoded.error_code,
                            error_detail = ?decoded.error_detail,
                            "wf nats diag client response decoded"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            mode = "client",
                            trace_id = %trace_id,
                            error = %err,
                            "wf nats diag client failed to decode response envelope"
                        );
                    }
                }
            }
            Err(err) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let metrics = client.metrics_snapshot();
                tracing::warn!(
                    mode = "client",
                    trace_id = %trace_id,
                    request_subject = %subject,
                    reply_subject = %reply_subject,
                    elapsed_ms = elapsed_ms,
                    error = %err,
                    nats_timeouts = metrics.timeouts,
                    nats_reconnects = metrics.reconnects,
                    nats_in_flight = metrics.in_flight,
                    nats_last_error = ?metrics.last_error,
                    "wf nats diag client request failed"
                );
            }
        }

        if i + 1 < loops {
            tokio::time::sleep(sleep_between).await;
        }
    }

    let metrics = client.metrics_snapshot();
    tracing::info!(
        mode = "client",
        nats_timeouts = metrics.timeouts,
        nats_reconnects = metrics.reconnects,
        nats_in_flight = metrics.in_flight,
        nats_last_error = ?metrics.last_error,
        "wf nats diag client completed"
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let log_level = env_or("JSR_LOG_LEVEL", "debug");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let mode = env_or("WF_DIAG_MODE", "server").to_ascii_lowercase();
    let endpoint = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let subject = env_or("WF_DIAG_SUBJECT", "wf.diag.echo");
    let sid = env_u32("WF_DIAG_SID", 51001);
    let timeout_secs = env_u64("WF_DIAG_TIMEOUT_SECS", 8);
    let loops = env_u64("WF_DIAG_LOOPS", 1);
    let interval_ms = env_u64("WF_DIAG_INTERVAL_MS", 500);
    let trace_prefix = env_or("WF_DIAG_TRACE_PREFIX", "wf-diag");

    match mode.as_str() {
        "server" => run_server(endpoint, subject, sid).await,
        "client" => {
            run_client(
                endpoint,
                subject,
                timeout_secs,
                loops,
                interval_ms,
                trace_prefix,
            )
            .await
        }
        other => Err(format!(
            "invalid WF_DIAG_MODE='{other}'. expected 'server' or 'client'"
        )
        .into()),
    }
}
