use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::node_client::NodeError;
use fluxbee_sdk::protocol::is_system_kind;
use fluxbee_sdk::{NodeSender, RouterDispatcher, RpcCommandReceiver, RpcError};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio::time::timeout;

use crate::errors::AiSdkError;
use crate::errors::Result;
use crate::function_calling::DEFAULT_MAX_TURNS;
use crate::llm::MODEL_CALL_TIMEOUT;
use crate::message::{build_reply_message_runtime_src, Message};
use crate::node_trait::AiNode;

/// Command channel the AI runtime drains. Binary callers must declare a
/// matching `command_channel` + `post_pending_rule` in the dispatcher
/// profile they hand to the runtime.
pub const AI_RUNTIME_CHANNEL: &str = "incoming";

/// Room for the tools a message's tool loop runs between its model turns: a `publish_html_page`
/// waits up to 2 min on SY.admin.
const TOOL_TIME_ALLOWANCE: Duration = Duration::from_secs(120);

/// How long a message's handler may run by default: a whole tool loop, that is
/// `DEFAULT_MAX_TURNS` model turns plus the plain call ai.generic and SY.frontdesk.gov make when the
/// loop ends without text, each bounded by `MODEL_CALL_TIMEOUT`, and the tools between them. It is a
/// backstop that never cuts a loop still making progress: a stuck model call fails on its own bound
/// first, as a provider error the node answers.
pub const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_secs(
    MODEL_CALL_TIMEOUT.as_secs() * (DEFAULT_MAX_TURNS as u64 + 1) + TOOL_TIME_ALLOWANCE.as_secs(),
);

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub read_timeout: Duration,
    /// How long one message's handler may run. The handler runs once: when it fails or runs out of
    /// time, the sender gets an error and the message is never run again.
    pub handler_timeout: Duration,
    pub write_timeout: Duration,
    pub queue_capacity: usize,
    pub worker_pool_size: usize,
    /// Retries of the write of a reply, the only step the runtime retries.
    pub write_retry: RetryPolicy,
    pub metrics_log_interval: Duration,
    /// The agent's own ILK, stamped as the sender of its replies (docs/router-stream-v1.md §6.2:
    /// cognition reads who says what). None leaves a reply without `src_ilk`, never the human's.
    pub self_ilk_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(30),
            handler_timeout: DEFAULT_HANDLER_TIMEOUT,
            write_timeout: Duration::from_secs(10),
            queue_capacity: 128,
            worker_pool_size: 4,
            write_retry: RetryPolicy::default(),
            metrics_log_interval: Duration::from_secs(30),
            self_ilk_id: fluxbee_sdk::read_self_ilk_from_env(),
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(2),
        }
    }
}

pub struct NodeRuntime<N: AiNode> {
    dispatcher: Arc<RouterDispatcher>,
    node: N,
}

#[derive(Debug, Default)]
struct RuntimeMetrics {
    read_messages: AtomicU64,
    idle_read_timeouts: AtomicU64,
    reconnect_events: AtomicU64,
    reconnect_wait_cycles: AtomicU64,
    enqueued_messages: AtomicU64,
    processed_messages: AtomicU64,
    responses_sent: AtomicU64,
    handler_failures: AtomicU64,
    handler_timeouts: AtomicU64,
    recoverable_exhausted: AtomicU64,
    fatal_errors: AtomicU64,
    retry_attempts: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeMetricsSnapshot {
    read_messages: u64,
    idle_read_timeouts: u64,
    reconnect_events: u64,
    reconnect_wait_cycles: u64,
    enqueued_messages: u64,
    processed_messages: u64,
    responses_sent: u64,
    handler_failures: u64,
    handler_timeouts: u64,
    recoverable_exhausted: u64,
    fatal_errors: u64,
    retry_attempts: u64,
}

impl RuntimeMetrics {
    fn snapshot(&self) -> RuntimeMetricsSnapshot {
        RuntimeMetricsSnapshot {
            read_messages: self.read_messages.load(Ordering::Relaxed),
            idle_read_timeouts: self.idle_read_timeouts.load(Ordering::Relaxed),
            reconnect_events: self.reconnect_events.load(Ordering::Relaxed),
            reconnect_wait_cycles: self.reconnect_wait_cycles.load(Ordering::Relaxed),
            enqueued_messages: self.enqueued_messages.load(Ordering::Relaxed),
            processed_messages: self.processed_messages.load(Ordering::Relaxed),
            responses_sent: self.responses_sent.load(Ordering::Relaxed),
            handler_failures: self.handler_failures.load(Ordering::Relaxed),
            handler_timeouts: self.handler_timeouts.load(Ordering::Relaxed),
            recoverable_exhausted: self.recoverable_exhausted.load(Ordering::Relaxed),
            fatal_errors: self.fatal_errors.load(Ordering::Relaxed),
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
        }
    }
}

impl<N: AiNode + 'static> NodeRuntime<N> {
    pub fn new(dispatcher: Arc<RouterDispatcher>, node: N) -> Self {
        Self { dispatcher, node }
    }

    pub async fn run_forever(self) -> Result<()> {
        self.run_with_config(RuntimeConfig::default()).await
    }

    pub async fn run_with_config(self, config: RuntimeConfig) -> Result<()> {
        if config.worker_pool_size == 0 {
            return Err(AiSdkError::Protocol(
                "worker_pool_size must be >= 1".to_string(),
            ));
        }
        if config.metrics_log_interval.is_zero() {
            return Err(AiSdkError::Protocol(
                "metrics_log_interval must be > 0".to_string(),
            ));
        }

        tracing::info!(
            worker_pool_size = config.worker_pool_size,
            queue_capacity = config.queue_capacity,
            read_timeout_ms = config.read_timeout.as_millis() as u64,
            handler_timeout_ms = config.handler_timeout.as_millis() as u64,
            write_timeout_ms = config.write_timeout.as_millis() as u64,
            write_retry_max_attempts = config.write_retry.max_attempts,
            write_retry_initial_backoff_ms = config.write_retry.initial_backoff.as_millis() as u64,
            write_retry_max_backoff_ms = config.write_retry.max_backoff.as_millis() as u64,
            metrics_log_interval_s = config.metrics_log_interval.as_secs(),
            "ai runtime started"
        );

        let reader = self
            .dispatcher
            .take_command_receiver(AI_RUNTIME_CHANNEL)
            .await
            .map_err(map_rpc_err)?;
        let writer = self.dispatcher.sender_snapshot();
        let (tx, mut rx) = mpsc::channel(config.queue_capacity);
        let metrics = Arc::new(RuntimeMetrics::default());

        let read_timeout = config.read_timeout;
        let dispatcher_for_reader = self.dispatcher.clone();
        let mut reader_task = tokio::spawn(reader_loop(
            reader,
            dispatcher_for_reader,
            tx,
            read_timeout,
            metrics.clone(),
        ));
        let metrics_task = tokio::spawn(metrics_loop(metrics.clone(), config.metrics_log_interval));
        let node = Arc::new(self.node);
        let mut workers = JoinSet::new();

        loop {
            tokio::select! {
                read_result = &mut reader_task => {
                    match read_result {
                        Ok(Ok(())) => break,
                        Ok(Err(err)) => {
                            metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                            metrics_task.abort();
                            return Err(err);
                        },
                        Err(err) => {
                            metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                            metrics_task.abort();
                            return Err(AiSdkError::Protocol(format!("reader task join error: {err}")));
                        }
                    }
                }
                worker_result = workers.join_next(), if !workers.is_empty() => {
                    match worker_result {
                        Some(Ok(Ok(()))) => {}
                        Some(Ok(Err(err))) => {
                            if matches!(err, AiSdkError::RecoverableExhausted(_)) {
                                metrics.recoverable_exhausted.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                            metrics_task.abort();
                            return Err(err);
                        }
                        Some(Err(err)) => {
                            metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                            metrics_task.abort();
                            return Err(AiSdkError::Protocol(format!("worker task join error: {err}")));
                        }
                        None => {}
                    }
                }
                maybe_msg = rx.recv() => {
                    let Some(msg) = maybe_msg else {
                        break;
                    };

                    while workers.len() >= config.worker_pool_size {
                        match workers.join_next().await {
                            Some(Ok(Ok(()))) => {}
                            Some(Ok(Err(err))) => {
                                if matches!(err, AiSdkError::RecoverableExhausted(_)) {
                                    metrics.recoverable_exhausted.fetch_add(1, Ordering::Relaxed);
                                    continue;
                                }
                                metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                                metrics_task.abort();
                                return Err(err);
                            }
                            Some(Err(err)) => {
                                metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                                metrics_task.abort();
                                return Err(AiSdkError::Protocol(format!("worker task join error: {err}")));
                            }
                            None => break,
                        }
                    }

                    let node = node.clone();
                    let writer = writer.clone();
                    let config = config.clone();
                    let metrics = metrics.clone();
                    workers.spawn(async move { process_one(node, writer, msg, config, metrics).await });
                }
            }
        }

        while let Some(worker_result) = workers.join_next().await {
            match worker_result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    if matches!(err, AiSdkError::RecoverableExhausted(_)) {
                        metrics
                            .recoverable_exhausted
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                    metrics_task.abort();
                    return Err(err);
                }
                Err(err) => {
                    metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
                    metrics_task.abort();
                    return Err(AiSdkError::Protocol(format!(
                        "worker task join error: {err}"
                    )));
                }
            }
        }

        metrics_task.abort();
        let final_metrics = metrics.snapshot();
        tracing::info!(
            read_messages = final_metrics.read_messages,
            idle_read_timeouts = final_metrics.idle_read_timeouts,
            reconnect_events = final_metrics.reconnect_events,
            reconnect_wait_cycles = final_metrics.reconnect_wait_cycles,
            enqueued_messages = final_metrics.enqueued_messages,
            processed_messages = final_metrics.processed_messages,
            responses_sent = final_metrics.responses_sent,
            handler_failures = final_metrics.handler_failures,
            handler_timeouts = final_metrics.handler_timeouts,
            recoverable_exhausted = final_metrics.recoverable_exhausted,
            fatal_errors = final_metrics.fatal_errors,
            retry_attempts = final_metrics.retry_attempts,
            "ai runtime stopped"
        );

        Ok(())
    }
}

async fn reader_loop(
    mut reader: RpcCommandReceiver,
    dispatcher: Arc<RouterDispatcher>,
    tx: mpsc::Sender<Message>,
    read_timeout: Duration,
    metrics: Arc<RuntimeMetrics>,
) -> Result<()> {
    let reconnect_initial_backoff = Duration::from_millis(200);
    let reconnect_max_backoff = Duration::from_secs(5);

    loop {
        let recv = timeout(read_timeout, reader.recv()).await;
        let msg = match recv {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                // Channel closed (dispatcher dropped). Clean shutdown.
                tracing::warn!("ai runtime command channel closed");
                return Ok(());
            }
            Err(_) => {
                // Idle window elapsed: keep process alive and continue
                // waiting. If the underlying connection has dropped, the
                // dispatcher's sender_snapshot().is_connected() reports it
                // and we enter the reconnect wait.
                let sender = dispatcher.sender_snapshot();
                if !sender.is_connected() {
                    metrics.reconnect_events.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!("ai runtime disconnected from router; entering reconnect wait");
                    wait_for_reconnect(
                        &sender,
                        reconnect_initial_backoff,
                        reconnect_max_backoff,
                        metrics.clone(),
                    )
                    .await;
                    continue;
                }
                metrics.idle_read_timeouts.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    read_timeout_ms = read_timeout.as_millis() as u64,
                    "ai runtime read timeout (idle)"
                );
                continue;
            }
        };
        metrics.read_messages.fetch_add(1, Ordering::Relaxed);
        if tx.send(msg).await.is_err() {
            return Ok(());
        }
        metrics.enqueued_messages.fetch_add(1, Ordering::Relaxed);
    }
}

async fn wait_for_reconnect(
    sender: &NodeSender,
    initial_backoff: Duration,
    max_backoff: Duration,
    metrics: Arc<RuntimeMetrics>,
) {
    let mut attempt: u64 = 0;
    let mut backoff = initial_backoff;

    loop {
        if sender.is_connected() {
            tracing::info!(attempt, "ai runtime reconnected to router");
            return;
        }

        attempt = attempt.saturating_add(1);
        let wait_for = with_jitter(backoff);
        metrics
            .reconnect_wait_cycles
            .fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            attempt,
            backoff_ms = backoff.as_millis() as u64,
            wait_ms = wait_for.as_millis() as u64,
            "ai runtime reconnecting to router"
        );

        let _ = timeout(wait_for, sender.wait_connected()).await;
        backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
    }
}

fn with_jitter(base: Duration) -> Duration {
    // Simple bounded jitter (+0%..+24%) to reduce synchronized reconnect waits.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_factor_percent = nanos % 25;
    let jitter = base
        .as_millis()
        .saturating_mul(jitter_factor_percent as u128)
        / 100;
    let total = base.as_millis().saturating_add(jitter);
    Duration::from_millis(total as u64)
}

async fn process_one<N: AiNode>(
    node: Arc<N>,
    writer: NodeSender,
    msg: Message,
    config: RuntimeConfig,
    metrics: Arc<RuntimeMetrics>,
) -> Result<()> {
    let maybe_response = run_handler(node.as_ref(), msg, config.handler_timeout, &metrics).await;

    if let Some(mut response) = maybe_response {
        response.routing.src = writer.uuid().to_string();
        if response.meta.src_ilk.is_none() {
            response.meta.src_ilk = config.self_ilk_id.clone();
        }
        // Sending a reply again is safe: a send that timed out or was refused never queued it (the
        // SDK's send is a cancel-safe channel send), so a reply cannot go out twice.
        execute_with_retry(
            || async {
                timeout(config.write_timeout, writer.send(response.clone()))
                    .await
                    .map_err(|_| AiSdkError::Timeout("router write timeout".to_string()))?
                    .map_err(AiSdkError::Node)
            },
            &config.write_retry,
            "write",
            metrics.clone(),
        )
        .await?;
        metrics.responses_sent.fetch_add(1, Ordering::Relaxed);
    }

    metrics.processed_messages.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Runs a message's handler once. When it fails or runs out of time it may already have called a
/// model (billed) or a tool that acts (`publish_html_page`), so it is never run again: the sender
/// gets an error instead, and the runtime goes on with the next message.
async fn run_handler<N: AiNode>(
    node: &N,
    msg: Message,
    handler_timeout: Duration,
    metrics: &RuntimeMetrics,
) -> Option<Message> {
    match timeout(handler_timeout, node.on_message(msg.clone())).await {
        Ok(Ok(response)) => response,
        Ok(Err(err)) => {
            metrics.handler_failures.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                error = %err,
                "handler failed; the sender gets an error and the message is not run again"
            );
            handler_failure_reply(
                &msg,
                "ai_runtime_error",
                "The AI runtime failed while processing this message.".to_string(),
            )
        }
        Err(_) => {
            metrics.handler_timeouts.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                handler_timeout_ms = handler_timeout.as_millis() as u64,
                "handler ran out of time; the sender gets an error and the message is not run again"
            );
            handler_failure_reply(
                &msg,
                "ai_runtime_timeout",
                format!(
                    "The AI runtime did not finish this message within {} s. It may have done part \
                     of its work before it stopped.",
                    handler_timeout.as_secs()
                ),
            )
        }
    }
}

/// The answer to a message whose handler failed or ran out of time, in the shape of every AI runtime
/// error (`type`, `code`, `message`, `retryable`). It is not `retryable`: the handler may have acted
/// before it stopped. A `system` or `admin` request gets none: its caller waits for a named answer
/// and reports its own timeout, and answering a router notice (`UNREACHABLE`) could bounce.
fn handler_failure_reply(msg: &Message, code: &str, message: String) -> Option<Message> {
    let kind = msg.meta.msg_type.as_str();
    if is_system_kind(kind) || kind.eq_ignore_ascii_case("admin") {
        return None;
    }
    Some(build_reply_message_runtime_src(
        msg,
        json!({
            "type": "error",
            "code": code,
            "message": message,
            "retryable": false
        }),
    ))
}

async fn execute_with_retry<T, F, Fut>(
    mut op: F,
    retry_policy: &RetryPolicy,
    stage: &str,
    metrics: Arc<RuntimeMetrics>,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let max_attempts = retry_policy.max_attempts.max(1);
    let mut backoff = retry_policy.initial_backoff;

    for attempt in 1..=max_attempts {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let is_last = attempt == max_attempts;
                if !err.is_recoverable() {
                    return Err(err);
                }
                if is_last {
                    tracing::warn!(
                        stage = stage,
                        attempts = max_attempts,
                        error = %err,
                        "recoverable error exhausted retry policy"
                    );
                    return Err(AiSdkError::RecoverableExhausted(format!(
                        "{stage} failed after {max_attempts} attempts: {err}"
                    )));
                }
                tracing::debug!(
                    stage = stage,
                    attempt = attempt,
                    next_backoff_ms = backoff.as_millis() as u64,
                    error = %err,
                    "recoverable error, retrying"
                );
                metrics.retry_attempts.fetch_add(1, Ordering::Relaxed);
                sleep(backoff).await;
                backoff = std::cmp::min(backoff.saturating_mul(2), retry_policy.max_backoff);
            }
        }
    }

    Err(AiSdkError::Protocol(
        "retry loop ended unexpectedly".to_string(),
    ))
}

async fn metrics_loop(metrics: Arc<RuntimeMetrics>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let snapshot = metrics.snapshot();
        tracing::info!(
            read_messages = snapshot.read_messages,
            idle_read_timeouts = snapshot.idle_read_timeouts,
            reconnect_events = snapshot.reconnect_events,
            reconnect_wait_cycles = snapshot.reconnect_wait_cycles,
            enqueued_messages = snapshot.enqueued_messages,
            processed_messages = snapshot.processed_messages,
            responses_sent = snapshot.responses_sent,
            handler_failures = snapshot.handler_failures,
            handler_timeouts = snapshot.handler_timeouts,
            recoverable_exhausted = snapshot.recoverable_exhausted,
            fatal_errors = snapshot.fatal_errors,
            retry_attempts = snapshot.retry_attempts,
            "ai runtime metrics"
        );
    }
}

fn map_rpc_err(err: RpcError) -> AiSdkError {
    match err {
        RpcError::Node(node) => AiSdkError::Node(node),
        RpcError::Disconnected => AiSdkError::Node(NodeError::Disconnected),
        other => AiSdkError::Protocol(format!("router dispatcher error: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use fluxbee_sdk::protocol::{Destination, Meta, Routing};
    use serde_json::Value;

    use super::*;

    enum Outcome {
        TooSlow,
        Fails,
        Answers,
    }

    struct CountingNode {
        outcome: Outcome,
        calls: AtomicUsize,
    }

    impl CountingNode {
        fn new(outcome: Outcome) -> Self {
            Self {
                outcome,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AiNode for CountingNode {
        async fn on_message(&self, msg: Message) -> Result<Option<Message>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.outcome {
                Outcome::TooSlow => {
                    sleep(Duration::from_secs(5)).await;
                    Ok(None)
                }
                // Recoverable: the runtime used to run the message again for it.
                Outcome::Fails => Err(AiSdkError::Node(NodeError::Disconnected)),
                Outcome::Answers => Ok(Some(build_reply_message_runtime_src(
                    &msg,
                    json!({"type": "text", "content": "done"}),
                ))),
            }
        }
    }

    fn message(msg_type: &str) -> Message {
        Message {
            routing: Routing {
                src: "IO.slack.main@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("AI.sales@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-1".to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                thread_id: Some("thread-1".to_string()),
                ..Meta::default()
            },
            payload: json!({"type": "text", "content": "publish my page"}),
        }
    }

    const SHORT: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_handler_out_of_time_is_answered_with_an_error_and_never_run_again() {
        let node = CountingNode::new(Outcome::TooSlow);
        let metrics = RuntimeMetrics::default();

        let reply = run_handler(&node, message("user"), SHORT, &metrics)
            .await
            .expect("the sender gets an answer");

        assert_eq!(node.calls(), 1);
        assert_eq!(metrics.handler_timeouts.load(Ordering::Relaxed), 1);
        assert_eq!(reply.payload["type"], json!("error"));
        assert_eq!(reply.payload["code"], json!("ai_runtime_timeout"));
        assert_eq!(reply.payload["retryable"], json!(false));
        assert!(matches!(
            reply.routing.dst,
            Destination::Unicast(ref dst) if dst == "IO.slack.main@motherbee"
        ));
        assert_eq!(reply.meta.thread_id.as_deref(), Some("thread-1"));
    }

    #[tokio::test]
    async fn a_failed_handler_is_answered_once_even_for_a_recoverable_error() {
        let node = CountingNode::new(Outcome::Fails);
        let metrics = RuntimeMetrics::default();

        let reply = run_handler(&node, message("user"), SHORT, &metrics)
            .await
            .expect("the sender gets an answer");

        assert_eq!(node.calls(), 1);
        assert_eq!(metrics.handler_failures.load(Ordering::Relaxed), 1);
        assert_eq!(reply.payload["code"], json!("ai_runtime_error"));
        assert_eq!(reply.payload["retryable"], json!(false));
    }

    #[tokio::test]
    async fn a_control_request_whose_handler_fails_gets_no_error_answer() {
        for kind in ["system", "admin"] {
            let node = CountingNode::new(Outcome::TooSlow);
            let metrics = RuntimeMetrics::default();
            assert!(run_handler(&node, message(kind), SHORT, &metrics)
                .await
                .is_none());
            assert_eq!(node.calls(), 1);
        }
    }

    #[tokio::test]
    async fn a_handler_answer_goes_out_as_it_is() {
        let node = CountingNode::new(Outcome::Answers);
        let metrics = RuntimeMetrics::default();

        let reply = run_handler(&node, message("user"), SHORT, &metrics)
            .await
            .expect("answer");

        assert_eq!(reply.payload["content"], Value::from("done"));
        assert_eq!(metrics.handler_failures.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.handler_timeouts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn the_default_handler_timeout_fits_a_whole_tool_loop() {
        let model_calls = DEFAULT_MAX_TURNS as u32 + 1;
        assert!(DEFAULT_HANDLER_TIMEOUT >= MODEL_CALL_TIMEOUT * model_calls + TOOL_TIME_ALLOWANCE);
        assert_eq!(
            RuntimeConfig::default().handler_timeout,
            DEFAULT_HANDLER_TIMEOUT
        );
        // The bounds the docs give: 5 min per model call, 47 min per message.
        assert_eq!(MODEL_CALL_TIMEOUT, Duration::from_secs(5 * 60));
        assert_eq!(DEFAULT_HANDLER_TIMEOUT, Duration::from_secs(47 * 60));
    }
}
