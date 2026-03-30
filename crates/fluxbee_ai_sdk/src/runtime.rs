use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::node_client::NodeError;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio::time::timeout;

use crate::errors::AiSdkError;
use crate::errors::Result;
use crate::node_trait::AiNode;
use crate::router_client::{RouterClient, RouterReader, RouterWriter};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub read_timeout: Duration,
    pub handler_timeout: Duration,
    pub write_timeout: Duration,
    pub queue_capacity: usize,
    pub worker_pool_size: usize,
    pub retry_policy: RetryPolicy,
    pub metrics_log_interval: Duration,
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
            handler_timeout: Duration::from_secs(60),
            write_timeout: Duration::from_secs(10),
            queue_capacity: 128,
            worker_pool_size: 4,
            retry_policy: RetryPolicy::default(),
            metrics_log_interval: Duration::from_secs(30),
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
    client: RouterClient,
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
            recoverable_exhausted: self.recoverable_exhausted.load(Ordering::Relaxed),
            fatal_errors: self.fatal_errors.load(Ordering::Relaxed),
            retry_attempts: self.retry_attempts.load(Ordering::Relaxed),
        }
    }
}

impl<N: AiNode + 'static> NodeRuntime<N> {
    pub fn new(client: RouterClient, node: N) -> Self {
        Self { client, node }
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
            retry_max_attempts = config.retry_policy.max_attempts,
            retry_initial_backoff_ms = config.retry_policy.initial_backoff.as_millis() as u64,
            retry_max_backoff_ms = config.retry_policy.max_backoff.as_millis() as u64,
            metrics_log_interval_s = config.metrics_log_interval.as_secs(),
            "ai runtime started"
        );

        let (reader, writer) = self.client.split();
        let (tx, mut rx) = mpsc::channel(config.queue_capacity);
        let metrics = Arc::new(RuntimeMetrics::default());

        let read_timeout = config.read_timeout;
        let mut reader_task = tokio::spawn(reader_loop(reader, tx, read_timeout, metrics.clone()));
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
            recoverable_exhausted = final_metrics.recoverable_exhausted,
            fatal_errors = final_metrics.fatal_errors,
            retry_attempts = final_metrics.retry_attempts,
            "ai runtime stopped"
        );

        Ok(())
    }
}

async fn reader_loop(
    mut reader: RouterReader,
    tx: mpsc::Sender<crate::message::Message>,
    read_timeout: Duration,
    metrics: Arc<RuntimeMetrics>,
) -> Result<()> {
    let reconnect_initial_backoff = Duration::from_millis(200);
    let reconnect_max_backoff = Duration::from_secs(5);

    loop {
        let msg = match reader.read_timeout(read_timeout).await {
            Ok(msg) => msg,
            Err(AiSdkError::Node(NodeError::Timeout)) => {
                // Idle window elapsed: keep process alive and continue waiting.
                metrics.idle_read_timeouts.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    read_timeout_ms = read_timeout.as_millis() as u64,
                    "ai runtime read timeout (idle)"
                );
                continue;
            }
            Err(err) if is_transient_link_error(&err) => {
                metrics.reconnect_events.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    error = %err,
                    "ai runtime disconnected from router; entering reconnect wait"
                );
                let msg = wait_for_reconnect(
                    &mut reader,
                    reconnect_initial_backoff,
                    reconnect_max_backoff,
                    metrics.clone(),
                )
                .await?;
                msg
            }
            Err(err) => return Err(err),
        };
        metrics.read_messages.fetch_add(1, Ordering::Relaxed);
        if tx.send(msg).await.is_err() {
            return Ok(());
        }
        metrics.enqueued_messages.fetch_add(1, Ordering::Relaxed);
    }
}

fn is_transient_link_error(err: &AiSdkError) -> bool {
    matches!(
        err,
        AiSdkError::Node(NodeError::Io(_) | NodeError::Disconnected)
    )
}

async fn wait_for_reconnect(
    reader: &mut RouterReader,
    initial_backoff: Duration,
    max_backoff: Duration,
    metrics: Arc<RuntimeMetrics>,
) -> Result<crate::message::Message> {
    let mut attempt: u64 = 0;
    let mut backoff = initial_backoff;
    let poll_timeout = Duration::from_millis(250);

    loop {
        match reader.read_timeout(poll_timeout).await {
            Ok(msg) => {
                tracing::info!(attempt, "ai runtime reconnected to router");
                return Ok(msg);
            }
            Err(AiSdkError::Node(NodeError::Timeout)) => {}
            Err(err) if is_transient_link_error(&err) => {}
            Err(err) => return Err(err),
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
        sleep(wait_for).await;
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
    writer: RouterWriter,
    msg: crate::message::Message,
    config: RuntimeConfig,
    metrics: Arc<RuntimeMetrics>,
) -> Result<()> {
    let maybe_response = execute_with_retry(
        || async {
            timeout(config.handler_timeout, node.on_message(msg.clone()))
                .await
                .map_err(|_| AiSdkError::Timeout("node handler timeout".to_string()))?
        },
        &config.retry_policy,
        "handler",
        metrics.clone(),
    )
    .await?;

    if let Some(mut response) = maybe_response {
        response.routing.src = writer.uuid().to_string();
        execute_with_retry(
            || async {
                timeout(config.write_timeout, writer.write(response.clone()))
                    .await
                    .map_err(|_| AiSdkError::Timeout("router write timeout".to_string()))?
            },
            &config.retry_policy,
            "write",
            metrics.clone(),
        )
        .await?;
        metrics.responses_sent.fetch_add(1, Ordering::Relaxed);
    }

    metrics.processed_messages.fetch_add(1, Ordering::Relaxed);
    Ok(())
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
            recoverable_exhausted = snapshot.recoverable_exhausted,
            fatal_errors = snapshot.fatal_errors,
            retry_attempts = snapshot.retry_attempts,
            "ai runtime metrics"
        );
    }
}
