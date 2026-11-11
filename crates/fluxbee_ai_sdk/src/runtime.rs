use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fluxbee_sdk::message_trace::TraceNote;
use fluxbee_sdk::message_writer::{self, Writer};
use fluxbee_sdk::node_client::NodeError;
use fluxbee_sdk::protocol::{is_solution_kind, is_system_kind};
use fluxbee_sdk::{NodeSender, RouterDispatcher, RpcCommandReceiver, RpcError};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::sleep;
use tokio::time::timeout;
use tokio::time::Instant;

use crate::errors::AiSdkError;
use crate::errors::Result;
use crate::function_calling::DEFAULT_MAX_TURNS;
use crate::llm::MODEL_CALL_TIMEOUT;
use crate::message::{build_reply_message_runtime_src, take_reply_act, Message};
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

/// How many messages that run without the model (`runs_without_model`) run at once by default,
/// beside the workers: a status or config request takes milliseconds, a frontdesk handoff at most
/// its registration's 8 s.
pub const DEFAULT_DIRECT_CAPACITY: usize = 64;

/// The channel that hands what the reader takes to the dispatch loop. The loop never waits for a
/// worker (FINDINGS A-257), so it only holds what arrives between two of the loop's turns.
const DISPATCH_CHANNEL_CAPACITY: usize = 64;

/// A lane that turns messages away says so at once, then at most once in this time, with how many
/// it turned away since its last line, as the router does for a full link (FINDINGS A-222).
const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// What an IO node marks a frontdesk handoff with: its `meta.msg` and its payload's `type`
/// (`FRONTDESK_HANDOFF_MSG` and `FRONTDESK_HANDOFF_PAYLOAD_TYPE` of gov-common and io-common).
const FRONTDESK_HANDOFF: &str = "frontdesk_handoff";

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub read_timeout: Duration,
    /// How long one message's handler may run. The handler runs once: when it fails or runs out of
    /// time, the sender gets an error and the message is never run again.
    pub handler_timeout: Duration,
    pub write_timeout: Duration,
    /// How many LLM turns may wait for a worker, in the order they came (FINDINGS A-257). A turn
    /// that finds every worker busy and this queue full is answered `ai_runtime_busy` and never
    /// run.
    pub queue_capacity: usize,
    /// The model workers: how many LLM turns, the messages that may need the model, run at once.
    pub worker_pool_size: usize,
    /// How many messages that run without the model (`runs_without_model`), and answers
    /// `ai_runtime_busy`, run at once beside the workers. One that finds them all taken is dropped:
    /// nothing runs or answers it.
    pub direct_capacity: usize,
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
            direct_capacity: DEFAULT_DIRECT_CAPACITY,
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
    /// Tap copies the runtime took to no handler (`ignored_tap_copy`, FINDINGS A-248). Each one is
    /// a debug line, and the nodes log at info: this count, in the periodic metrics line, is where
    /// the journal shows them.
    tap_copies_ignored: AtomicU64,
    /// LLM turns that waited for a worker (FINDINGS A-257).
    turns_queued: AtomicU64,
    /// LLM turns waiting for a worker now.
    turns_waiting: AtomicU64,
    /// LLM turns answered `ai_runtime_busy` and never run: every worker busy and the queue full.
    turns_refused: AtomicU64,
    /// Messages nothing ran or answered: every place of the direct lane was taken.
    direct_dropped: AtomicU64,
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
    tap_copies_ignored: u64,
    turns_queued: u64,
    turns_waiting: u64,
    turns_refused: u64,
    direct_dropped: u64,
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
            tap_copies_ignored: self.tap_copies_ignored.load(Ordering::Relaxed),
            turns_queued: self.turns_queued.load(Ordering::Relaxed),
            turns_waiting: self.turns_waiting.load(Ordering::Relaxed),
            turns_refused: self.turns_refused.load(Ordering::Relaxed),
            direct_dropped: self.direct_dropped.load(Ordering::Relaxed),
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
        if config.direct_capacity == 0 {
            return Err(AiSdkError::Protocol(
                "direct_capacity must be >= 1".to_string(),
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
            direct_capacity = config.direct_capacity,
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
        let (tx, mut rx) = mpsc::channel(DISPATCH_CHANNEL_CAPACITY);
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
        let mut lanes = Lanes::new(Arc::new(self.node), writer, config, metrics.clone());

        let ended = match dispatch(&mut lanes, &mut rx, &mut reader_task).await {
            Ok(()) => lanes.finish().await,
            Err(err) => Err(err),
        };
        metrics_task.abort();
        if let Err(err) = ended {
            metrics.fatal_errors.fetch_add(1, Ordering::Relaxed);
            return Err(err);
        }

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
            tap_copies_ignored = final_metrics.tap_copies_ignored,
            turns_queued = final_metrics.turns_queued,
            turns_waiting = final_metrics.turns_waiting,
            turns_refused = final_metrics.turns_refused,
            direct_dropped = final_metrics.direct_dropped,
            "ai runtime stopped"
        );

        Ok(())
    }
}

/// Whether the runtime runs `msg` as soon as it reads it, beside the model workers, rather than as
/// an LLM turn that waits for one (FINDINGS A-257). It is a message the AI nodes on this runtime
/// (ai.generic, SY.frontdesk.gov) answer without their model:
///
/// - **Control traffic:** any `system` or `admin` message, in any case. Both nodes take it to
///   their control plane, never to the model (`is_control_plane` in each): the verbs it runs
///   (`NODE_STATUS_GET`, `NODE_HELP`, `CONFIG_GET`, `CONFIG_SET`, `PING`, `STATUS`) or refuses,
///   `VAULT_SECRET_CHANGED`, and the notices and replies it leaves unanswered
///   (`is_infrastructure_message`).
/// - **The frontdesk's handoff:** a `user` message an IO node sends marked as one, `meta.msg` and
///   the payload's `type` both `frontdesk_handoff`. SY.frontdesk.gov runs it in code
///   (`handle_frontdesk_handoff`: an `ILK_REGISTER` to SY.identity, every attempt within 8 s) while
///   the IO node waits for the answer, 10 s for io.api and 20 s for IO.cloud. Only an IO node hands
///   a person over, and only to the frontdesk (`frontdesk_handoff_sender_allowed`), so a handoff
///   from any other sender is an LLM turn here.
///
/// Anything else may need the model and waits for a worker: a conversation's turn, a WF's message,
/// and the few `user` messages a node answers in code that the runtime cannot tell apart (the
/// frontdesk's case of the root tenant, a turn to an agent that is not configured).
pub fn runs_without_model(msg: &Message) -> bool {
    let kind = msg.meta.msg_type.as_str();
    if is_system_kind(kind) || kind.eq_ignore_ascii_case("admin") {
        return true;
    }
    kind.eq_ignore_ascii_case("user")
        && msg.meta.msg.as_deref() == Some(FRONTDESK_HANDOFF)
        && msg.payload.get("type").and_then(Value::as_str) == Some(FRONTDESK_HANDOFF)
        && msg
            .routing
            .src_l2_name
            .as_deref()
            .map(str::trim)
            .is_some_and(|sender| sender.starts_with("IO."))
}

/// Where the runtime sends the answers: the node's connection to its router, or a test's.
#[async_trait]
trait ReplyWriter: Clone + Send + Sync + 'static {
    fn uuid(&self) -> &str;
    fn full_name(&self) -> Arc<str>;
    async fn send_traced(
        &self,
        msg: Message,
        note: TraceNote,
    ) -> std::result::Result<(), NodeError>;
}

#[async_trait]
impl ReplyWriter for NodeSender {
    fn uuid(&self) -> &str {
        NodeSender::uuid(self)
    }

    fn full_name(&self) -> Arc<str> {
        NodeSender::full_name(self)
    }

    async fn send_traced(
        &self,
        msg: Message,
        note: TraceNote,
    ) -> std::result::Result<(), NodeError> {
        NodeSender::send_traced(self, msg, note).await
    }
}

/// How a task of a lane ended.
type TaskEnd = std::result::Result<Result<()>, JoinError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// The model workers, which run the LLM turns.
    Worker,
    /// What runs without the model, as it is read.
    Direct,
}

/// Takes each message the reader hands on into `lanes` as it comes, while their tasks run, and
/// settles each task as it ends, until the reader stops. It never waits for a worker: a status
/// request or a handoff that arrives while every worker runs an LLM turn starts at once.
async fn dispatch<N: AiNode + 'static, W: ReplyWriter>(
    lanes: &mut Lanes<N, W>,
    rx: &mut mpsc::Receiver<Message>,
    reader_task: &mut JoinHandle<Result<()>>,
) -> Result<()> {
    loop {
        tokio::select! {
            read_result = &mut *reader_task => {
                return match read_result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(err)) => Err(err),
                    Err(err) => Err(AiSdkError::Protocol(format!("reader task join error: {err}"))),
                };
            }
            ended = lanes.join_next(), if lanes.is_running() => {
                if let Some(ended) = ended {
                    lanes.settle(ended)?;
                }
            }
            maybe_msg = rx.recv() => {
                let Some(msg) = maybe_msg else {
                    return Ok(());
                };
                lanes.take(msg)?;
            }
        }
    }
}

/// The runtime's two lanes (FINDINGS A-257).
///
/// - **Direct:** a message that runs without the model (`runs_without_model`) starts as it is
///   read, up to `direct_capacity` at once. One that finds every place taken is dropped: no handler
///   runs and nothing answers it, as for a handler that fails on a `system` request.
/// - **Workers:** every other message is an LLM turn. It takes a free worker, of
///   `worker_pool_size`, or waits for one in the order turns came, up to `queue_capacity`; the next
///   one is answered `ai_runtime_busy` at once (retryable: it never ran), on the direct lane.
///
/// So turns start in the order they came, at most `worker_pool_size` at once, as before; nothing
/// waits without a bound; and a node with a free worker runs each message as it comes, as before.
struct Lanes<N, W> {
    node: Arc<N>,
    writer: W,
    config: RuntimeConfig,
    metrics: Arc<RuntimeMetrics>,
    /// The LLM turns that run, one per worker.
    workers: JoinSet<Result<()>>,
    /// The LLM turns that wait for a worker, in the order they came.
    waiting: VecDeque<Message>,
    /// What runs without the model, and the answers `ai_runtime_busy`.
    direct: JoinSet<Result<()>>,
    turns_refused_log: RefusalLog,
    direct_dropped_log: RefusalLog,
}

impl<N: AiNode + 'static, W: ReplyWriter> Lanes<N, W> {
    fn new(node: Arc<N>, writer: W, config: RuntimeConfig, metrics: Arc<RuntimeMetrics>) -> Self {
        Self {
            node,
            writer,
            config,
            metrics,
            workers: JoinSet::new(),
            waiting: VecDeque::new(),
            direct: JoinSet::new(),
            turns_refused_log: RefusalLog::default(),
            direct_dropped_log: RefusalLog::default(),
        }
    }

    fn is_running(&self) -> bool {
        !self.workers.is_empty() || !self.direct.is_empty()
    }

    /// Places a message the reader handed on: a tap copy nowhere, a message that runs without the
    /// model on the direct lane, an LLM turn on a free worker, at the end of the queue, or, with the
    /// queue full, nowhere but its answer `ai_runtime_busy`. A task that ended first gives back its
    /// place.
    fn take(&mut self, msg: Message) -> Result<()> {
        self.settle_ended()?;
        if ignored_tap_copy(&msg, &self.metrics) {
            return Ok(());
        }
        if runs_without_model(&msg) {
            let task = process_one(
                self.node.clone(),
                self.writer.clone(),
                msg.clone(),
                self.config.clone(),
                self.metrics.clone(),
            );
            self.run_direct(&msg, task);
        } else if self.waiting.is_empty() && self.workers.len() < self.config.worker_pool_size {
            self.start_turn(msg);
        } else if self.waiting.len() < self.config.queue_capacity {
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                thread_id = ?msg.meta.thread_id,
                waiting = self.waiting.len() + 1,
                "LLM turn waits for a model worker"
            );
            self.waiting.push_back(msg);
            self.metrics.turns_queued.fetch_add(1, Ordering::Relaxed);
            self.note_waiting();
        } else {
            self.refuse_turn(msg);
        }
        Ok(())
    }

    /// The next task of either lane to end, once one does; None when none runs.
    async fn join_next(&mut self) -> Option<(Lane, TaskEnd)> {
        tokio::select! {
            Some(ended) = self.workers.join_next() => Some((Lane::Worker, ended)),
            Some(ended) = self.direct.join_next() => Some((Lane::Direct, ended)),
            else => None,
        }
    }

    /// Settles every task that has ended, so the places counted are those of tasks that run.
    fn settle_ended(&mut self) -> Result<()> {
        while let Some(ended) = self.workers.try_join_next() {
            self.settle((Lane::Worker, ended))?;
        }
        while let Some(ended) = self.direct.try_join_next() {
            self.settle((Lane::Direct, ended))?;
        }
        Ok(())
    }

    /// A worker a task gives back takes the next waiting turn. A task's error is the runtime's, as
    /// it was: a write that failed after its retries counts in `recoverable_exhausted` and the
    /// runtime goes on; any other error, or a task that panicked, stops it.
    fn settle(&mut self, (lane, ended): (Lane, TaskEnd)) -> Result<()> {
        if lane == Lane::Worker {
            self.start_waiting_turns();
        }
        match ended {
            Ok(Ok(())) => Ok(()),
            Ok(Err(AiSdkError::RecoverableExhausted(_))) => {
                self.metrics
                    .recoverable_exhausted
                    .fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Ok(Err(err)) => Err(err),
            Err(err) => Err(AiSdkError::Protocol(format!(
                "worker task join error: {err}"
            ))),
        }
    }

    fn start_turn(&mut self, msg: Message) {
        self.workers.spawn(process_one(
            self.node.clone(),
            self.writer.clone(),
            msg,
            self.config.clone(),
            self.metrics.clone(),
        ));
    }

    fn start_waiting_turns(&mut self) {
        while self.workers.len() < self.config.worker_pool_size {
            let Some(msg) = self.waiting.pop_front() else {
                break;
            };
            self.start_turn(msg);
        }
        self.note_waiting();
    }

    fn note_waiting(&self) {
        self.metrics
            .turns_waiting
            .store(self.waiting.len() as u64, Ordering::Relaxed);
    }

    /// Runs `task`, for `msg`, on the direct lane, or drops it when every place is taken.
    fn run_direct<F>(&mut self, msg: &Message, task: F)
    where
        F: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        if self.direct.len() < self.config.direct_capacity {
            self.direct.spawn(task);
            return;
        }
        let dropped_total = self.metrics.direct_dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(dropped) = self.direct_dropped_log.note(Instant::now()) {
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                msg = ?msg.meta.msg,
                src_l2_name = ?msg.routing.src_l2_name,
                direct_capacity = self.config.direct_capacity,
                dropped,
                direct_dropped = dropped_total,
                "ai runtime dropped a message, neither run nor answered: every place of its lane for \
                 what runs without the model is taken"
            );
        }
    }

    /// An LLM turn that finds every worker busy and the queue full is never run: its sender gets
    /// `ai_runtime_busy` at once, from the direct lane.
    fn refuse_turn(&mut self, msg: Message) {
        let refused_total = self.metrics.turns_refused.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(refused) = self.turns_refused_log.note(Instant::now()) {
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                src_l2_name = ?msg.routing.src_l2_name,
                thread_id = ?msg.meta.thread_id,
                worker_pool_size = self.config.worker_pool_size,
                queue_capacity = self.config.queue_capacity,
                refused,
                turns_refused = refused_total,
                "ai runtime answered an LLM turn ai_runtime_busy without running it: every model \
                 worker is busy and the queue for them is full"
            );
        }
        let Some(reply) = busy_reply(&msg, &self.config) else {
            return;
        };
        let task = send_reply(
            self.writer.clone(),
            msg.routing.src_l2_name.clone(),
            reply,
            self.config.clone(),
            self.metrics.clone(),
        );
        self.run_direct(&msg, task);
    }

    /// The runtime stops: the turns still waiting are dropped, as the messages not handed on yet
    /// were before, and the tasks that run end first.
    async fn finish(&mut self) -> Result<()> {
        if !self.waiting.is_empty() {
            tracing::warn!(
                turns = self.waiting.len(),
                "ai runtime stopped with LLM turns waiting for a worker; they are not run"
            );
            self.waiting.clear();
            self.note_waiting();
        }
        while let Some(ended) = self.join_next().await {
            self.settle(ended)?;
        }
        Ok(())
    }
}

/// When a lane says it turned messages away: the first time, then at most once every
/// `REFUSAL_LOG_INTERVAL`.
#[derive(Debug, Default)]
struct RefusalLog {
    logged_at: Option<Instant>,
    unlogged: u64,
}

impl RefusalLog {
    /// Counts one; when its line is due, how many to name in it (this one and those since the last).
    fn note(&mut self, now: Instant) -> Option<u64> {
        self.unlogged += 1;
        if self
            .logged_at
            .is_some_and(|at| now.duration_since(at) < REFUSAL_LOG_INTERVAL)
        {
            return None;
        }
        self.logged_at = Some(now);
        Some(std::mem::take(&mut self.unlogged))
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

async fn process_one<N: AiNode, W: ReplyWriter>(
    node: Arc<N>,
    writer: W,
    msg: Message,
    config: RuntimeConfig,
    metrics: Arc<RuntimeMetrics>,
) -> Result<()> {
    // The reply goes to the requester's UUID: its entry in the cycle's trace names the requester by
    // the name the router stamped on the request.
    let requester = msg.routing.src_l2_name.clone();
    let maybe_response = run_handler(node.as_ref(), msg, config.handler_timeout, &metrics).await;

    if let Some(response) = maybe_response {
        send_reply(writer, requester, response, config, metrics.clone()).await?;
    }

    metrics.processed_messages.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Sends an answer to `requester`, the name the router stamped on the request: stamped as this
/// node's (`stamp_reply`), with the node's entry in the cycle's trace.
async fn send_reply<W: ReplyWriter>(
    writer: W,
    requester: Option<String>,
    mut response: Message,
    config: RuntimeConfig,
    metrics: Arc<RuntimeMetrics>,
) -> Result<()> {
    stamp_reply(
        &mut response,
        writer.uuid(),
        &writer.full_name(),
        config.self_ilk_id.as_deref(),
    );
    let note = reply_trace_note(requester.as_deref(), &mut response);
    // Sending a reply again is safe: a send that timed out or was refused never queued it (the
    // SDK's send is a cancel-safe channel send), so a reply cannot go out twice.
    execute_with_retry(
        || async {
            timeout(
                config.write_timeout,
                writer.send_traced(response.clone(), note.clone()),
            )
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
    Ok(())
}

/// What the runtime stamps on an answer as it sends it: this node as its sender, its ILK when the
/// answer names no `src_ilk`, and itself as the writer of a solution answer, its ILK and its node
/// name (docs/cognition-hands-v1.md §5), in place of the request's writer, which the reply's context
/// brought along.
fn stamp_reply(response: &mut Message, node_uuid: &str, node_name: &str, self_ilk: Option<&str>) {
    response.routing.src = node_uuid.to_string();
    if response.meta.src_ilk.is_none() {
        response.meta.src_ilk = self_ilk.map(ToString::to_string);
    }
    if is_solution_kind(&response.meta.msg_type) {
        message_writer::set_writer(&mut response.meta, Writer::new(self_ilk, Some(node_name)));
    }
}

/// The agent's entry in its reply's trace (docs/message-trace-v1.md §5): to whoever asked, with what
/// the node noted it did (`set_reply_act`, taken out of the reply) and the option it chose when it
/// answered a menu (`meta.context.evaluation.option`).
fn reply_trace_note(requester: Option<&str>, response: &mut Message) -> TraceNote {
    let mut act = take_reply_act(response);
    let option = response
        .meta
        .context
        .as_ref()
        .and_then(|context| context.pointer("/evaluation/option"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|option| !option.is_empty());
    act.extend(option.map(ToString::to_string));
    TraceNote {
        to: requester.map(ToString::to_string),
        act,
    }
}

/// Whether `msg` is a tap copy (`meta.via_tap`), which never reaches a handler, and which nothing
/// answers (FINDINGS A-248): it is the copy of another node's message that a router sends a tap's
/// target (docs/reference/routing-and-policy.md §8.3), there to be observed. It keeps the original
/// sender, so an answer would go to that sender, under the copy's trace, about a message it never
/// sent this node, and the handler would run a model or a tool for it. Every message the node
/// answers, its control plane's included, is checked here: the dispatch loop takes a copy to no lane
/// (so it never waits for a worker, nor is answered `ai_runtime_busy`), and `run_handler` checks
/// again. Each copy is a debug line and counts in `tap_copies_ignored`, which the periodic metrics
/// line shows.
fn ignored_tap_copy(msg: &Message, metrics: &RuntimeMetrics) -> bool {
    if !msg.meta.via_tap {
        return false;
    }
    metrics.tap_copies_ignored.fetch_add(1, Ordering::Relaxed);
    tracing::debug!(
        trace_id = %msg.routing.trace_id,
        msg_type = %msg.meta.msg_type,
        msg = ?msg.meta.msg,
        src_l2_name = ?msg.routing.src_l2_name,
        "a tap copy, there to be observed: not answered"
    );
    true
}

/// Runs a message's handler once. When it fails or runs out of time it may already have called a
/// model (billed) or a tool that acts (`publish_html_page`), so it is never run again: the sender
/// gets an error instead, and the runtime goes on with the next message. A tap copy never reaches
/// the handler (`ignored_tap_copy`).
async fn run_handler<N: AiNode>(
    node: &N,
    msg: Message,
    handler_timeout: Duration,
    metrics: &RuntimeMetrics,
) -> Option<Message> {
    if ignored_tap_copy(&msg, metrics) {
        return None;
    }
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
            error_reply(
                &msg,
                "ai_runtime_error",
                "The AI runtime failed while processing this message.".to_string(),
                false,
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
            error_reply(
                &msg,
                "ai_runtime_timeout",
                format!(
                    "The AI runtime did not finish this message within {} s. It may have done part \
                     of its work before it stopped.",
                    handler_timeout.as_secs()
                ),
                false,
            )
        }
    }
}

/// The answer to an LLM turn the runtime turned away without running it (FINDINGS A-257): every
/// worker busy and the queue for them full. It is `retryable`: nothing was done.
fn busy_reply(msg: &Message, config: &RuntimeConfig) -> Option<Message> {
    error_reply(
        msg,
        "ai_runtime_busy",
        format!(
            "The AI runtime is busy: its {} model workers are all taken and {} more messages wait \
             for them. This message was not run; send it again later.",
            config.worker_pool_size, config.queue_capacity
        ),
        true,
    )
}

/// An AI runtime error the sender gets in place of the handler's answer, in the shape of every one
/// (`type`, `code`, `message`, `retryable`). A message whose handler failed or ran out of time is
/// not `retryable`: the handler may have acted before it stopped. A `system` or `admin` request gets
/// none: its caller waits for a named answer and reports its own timeout, and answering a router
/// notice (`UNREACHABLE`) could bounce.
fn error_reply(msg: &Message, code: &str, message: String, retryable: bool) -> Option<Message> {
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
            "retryable": retryable
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
            tap_copies_ignored = snapshot.tap_copies_ignored,
            turns_queued = snapshot.turns_queued,
            turns_waiting = snapshot.turns_waiting,
            turns_refused = snapshot.turns_refused,
            direct_dropped = snapshot.direct_dropped,
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

    #[test]
    fn a_reply_names_its_requester_and_the_option_the_agent_chose() {
        let mut response = message("user");
        response.meta.context = Some(json!({
            "evaluation": {"reply": "I'm passing you to the team", "option": " handover ", "params": {}}
        }));
        let note = reply_trace_note(Some("WF.ventas@worker1"), &mut response);
        assert_eq!(note.to.as_deref(), Some("WF.ventas@worker1"));
        assert_eq!(note.act, ["handover"]);

        response.meta.context = None;
        let note = reply_trace_note(None, &mut response);
        assert_eq!(note, TraceNote::default());

        // What the node noted it did comes first, and never goes out in the reply.
        crate::message::set_reply_act(&mut response, ["identity_assigned", "registered"]);
        let note = reply_trace_note(Some("IO.slack@motherbee"), &mut response);
        assert_eq!(note.act, ["identity_assigned", "registered"]);
        assert_eq!(response.meta.context, Some(json!({})));
    }

    /// An answer names the agent as its writer, in place of the request's (docs/cognition-hands-v1.md
    /// §5); a control answer names none.
    #[test]
    fn an_answer_names_the_agent_as_its_writer() {
        let mut response = message("user");
        response.meta.context =
            Some(json!({"writer": {"ilk": "ilk:ana", "name": "Ana"}, "trace": []}));
        stamp_reply(
            &mut response,
            "uuid-sales",
            "AI.sales@worker1",
            Some("ilk:sales"),
        );
        assert_eq!(response.routing.src, "uuid-sales");
        assert_eq!(response.meta.src_ilk.as_deref(), Some("ilk:sales"));
        assert_eq!(
            message_writer::writer_of(response.meta.context.as_ref()),
            Some(Writer::new(Some("ilk:sales"), Some("AI.sales@worker1")))
        );
        assert!(response.meta.context.as_ref().unwrap()["trace"].is_array());

        // With no ILK of its own, the agent names its node.
        let mut response = message("user");
        stamp_reply(&mut response, "uuid-sales", "AI.sales@worker1", None);
        assert_eq!(
            message_writer::writer_of(response.meta.context.as_ref()),
            Some(Writer::new(None, Some("AI.sales@worker1")))
        );

        let mut control = message("system");
        stamp_reply(
            &mut control,
            "uuid-sales",
            "AI.sales@worker1",
            Some("ilk:sales"),
        );
        assert_eq!(control.meta.context, None);
    }

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

    /// FINDINGS A-248: a tap copy is there to be observed. Whatever its kind, the handler never runs
    /// for it (no model, no tool, no control verb) and nothing answers it, not even with an error.
    #[tokio::test]
    async fn a_tap_copy_is_neither_handled_nor_answered() {
        for kind in ["user", "system", "admin"] {
            for outcome in [Outcome::Answers, Outcome::Fails] {
                let node = CountingNode::new(outcome);
                let metrics = RuntimeMetrics::default();
                let mut copy = message(kind);
                copy.routing.src_l2_name = Some("IO.slack.main@motherbee".to_string());
                copy.meta.via_tap = true;

                assert!(
                    run_handler(&node, copy, SHORT, &metrics).await.is_none(),
                    "{kind}"
                );
                assert_eq!(node.calls(), 0, "{kind}");
                assert_eq!(metrics.handler_failures.load(Ordering::Relaxed), 0);
            }
        }
        // The same message, not a copy, is answered.
        let node = CountingNode::new(Outcome::Answers);
        let metrics = RuntimeMetrics::default();
        assert!(run_handler(&node, message("user"), SHORT, &metrics)
            .await
            .is_some());
        assert_eq!(node.calls(), 1);
    }

    /// A-248's review: each tap copy the runtime leaves alone counts in `tap_copies_ignored`, which
    /// the periodic metrics line shows (the copy's own line is at debug, below the nodes' level); a
    /// message that is not a copy does not.
    #[tokio::test]
    async fn the_tap_copies_the_runtime_ignores_are_counted() {
        let node = CountingNode::new(Outcome::Answers);
        let metrics = RuntimeMetrics::default();
        for kind in ["user", "system", "user"] {
            let mut copy = message(kind);
            copy.meta.via_tap = true;
            assert!(run_handler(&node, copy, SHORT, &metrics).await.is_none());
        }
        assert!(run_handler(&node, message("user"), SHORT, &metrics)
            .await
            .is_some());

        assert_eq!(metrics.tap_copies_ignored.load(Ordering::Relaxed), 3);
        assert_eq!(metrics.snapshot().tap_copies_ignored, 3);
        assert_eq!(node.calls(), 1);
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

    // FINDINGS A-257: the lanes.

    /// The answers the runtime sends, handed to the test.
    #[derive(Clone)]
    struct TestWriter {
        sent: mpsc::UnboundedSender<Message>,
    }

    #[async_trait]
    impl ReplyWriter for TestWriter {
        fn uuid(&self) -> &str {
            "uuid-of-the-frontdesk"
        }

        fn full_name(&self) -> Arc<str> {
            Arc::from("SY.frontdesk.gov@motherbee")
        }

        async fn send_traced(
            &self,
            msg: Message,
            _note: TraceNote,
        ) -> std::result::Result<(), NodeError> {
            let _ = self.sent.send(msg);
            Ok(())
        }
    }

    /// A node as the frontdesk is to its runtime: an LLM turn holds its worker until the test lets
    /// one go (the earliest first), and a status request or a handoff is answered at once. A
    /// `system` message named `SLOW` holds its place until the test lets it go. It notes the order
    /// its handler starts each message in, by trace id.
    struct GatedNode {
        started: std::sync::Mutex<Vec<String>>,
        turns: tokio::sync::Semaphore,
        slow_control: tokio::sync::Semaphore,
    }

    impl GatedNode {
        fn new() -> Self {
            Self {
                started: std::sync::Mutex::new(Vec::new()),
                turns: tokio::sync::Semaphore::new(0),
                slow_control: tokio::sync::Semaphore::new(0),
            }
        }

        fn started(&self) -> Vec<String> {
            self.started.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl AiNode for GatedNode {
        async fn on_message(&self, msg: Message) -> Result<Option<Message>> {
            self.started
                .lock()
                .unwrap()
                .push(msg.routing.trace_id.clone());
            if is_system_kind(&msg.meta.msg_type) {
                if msg.meta.msg.as_deref() == Some("SLOW") {
                    self.slow_control.acquire().await.unwrap().forget();
                }
                let mut answer = build_reply_message_runtime_src(&msg, json!({"status": "ok"}));
                answer.meta.msg = Some("NODE_STATUS_GET_RESPONSE".to_string());
                return Ok(Some(answer));
            }
            if msg.meta.msg.as_deref() == Some(FRONTDESK_HANDOFF) {
                return Ok(Some(build_reply_message_runtime_src(
                    &msg,
                    json!({"type": "frontdesk_result", "status": "ok"}),
                )));
            }
            self.turns.acquire().await.unwrap().forget();
            Ok(Some(build_reply_message_runtime_src(
                &msg,
                json!({"type": "text", "content": msg.routing.trace_id}),
            )))
        }
    }

    struct Harness {
        node: Arc<GatedNode>,
        metrics: Arc<RuntimeMetrics>,
        to_runtime: mpsc::Sender<Message>,
        answers: mpsc::UnboundedReceiver<Message>,
    }

    impl Harness {
        /// The dispatch loop of `run_with_config`, on `config`, with the test as its router.
        fn start(config: RuntimeConfig) -> Self {
            let node = Arc::new(GatedNode::new());
            let metrics = Arc::new(RuntimeMetrics::default());
            let (sent, answers) = mpsc::unbounded_channel();
            let (to_runtime, mut rx) = mpsc::channel(DISPATCH_CHANNEL_CAPACITY);
            let mut lanes = Lanes::new(node.clone(), TestWriter { sent }, config, metrics.clone());
            tokio::spawn(async move {
                let mut reader = tokio::spawn(std::future::pending::<Result<()>>());
                dispatch(&mut lanes, &mut rx, &mut reader).await
            });
            Self {
                node,
                metrics,
                to_runtime,
                answers,
            }
        }

        async fn send(&self, msg: Message) {
            self.to_runtime.send(msg).await.expect("the runtime reads");
        }

        /// The next answer, which comes at once (paused time moves only when nothing else can).
        async fn answer(&mut self) -> Message {
            timeout(Duration::from_secs(5), self.answers.recv())
                .await
                .expect("an answer at once")
                .expect("the runtime runs")
        }

        async fn no_answer(&mut self) {
            if let Ok(answer) = timeout(Duration::from_secs(5), self.answers.recv()).await {
                panic!("no answer was due, got {answer:?}");
            }
        }

        /// Waits until the node has started `count` messages.
        async fn started(&self, count: usize) -> Vec<String> {
            for _ in 0..100 {
                let started = self.node.started();
                if started.len() >= count {
                    return started;
                }
                sleep(Duration::from_millis(10)).await;
            }
            panic!("the node started {:?}", self.node.started());
        }

        fn let_turns_go(&self, count: usize) {
            self.node.turns.add_permits(count);
        }
    }

    fn turn(id: &str) -> Message {
        let mut msg = message("user");
        msg.routing.trace_id = id.to_string();
        msg.routing.src_l2_name = Some("IO.slack.main@motherbee".to_string());
        msg.meta.thread_id = Some(format!("thread-{id}"));
        msg.payload = json!({"type": "text", "content": id});
        msg
    }

    fn status_request(id: &str) -> Message {
        let mut msg = message("system");
        msg.routing.trace_id = id.to_string();
        msg.routing.src_l2_name = Some("SY.orchestrator@motherbee".to_string());
        msg.meta.msg = Some("NODE_STATUS_GET".to_string());
        msg.meta.thread_id = None;
        msg.payload = json!({});
        msg
    }

    fn handoff(id: &str) -> Message {
        let mut msg = message("user");
        msg.routing.trace_id = id.to_string();
        msg.routing.src_l2_name = Some("IO.api.shop@motherbee".to_string());
        msg.meta.msg = Some(FRONTDESK_HANDOFF.to_string());
        msg.meta.thread_id = None;
        msg.payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": {"display_name": "Ana", "email": "ana@example.com"}
        });
        msg
    }

    fn ids(answers: &[Message]) -> Vec<&str> {
        answers
            .iter()
            .map(|answer| answer.routing.trace_id.as_str())
            .collect()
    }

    /// FINDINGS A-257: with all 4 workers on LLM turns, the frontdesk's status request and an IO
    /// node's handoff are answered at once, where they waited for a turn to end.
    #[tokio::test(start_paused = true)]
    async fn a_status_request_and_a_handoff_are_answered_while_every_worker_runs_a_turn() {
        let mut runtime = Harness::start(RuntimeConfig::default());
        for id in ["turn-1", "turn-2", "turn-3", "turn-4"] {
            runtime.send(turn(id)).await;
        }
        // A node with free workers runs each turn as it comes.
        assert_eq!(
            runtime.started(4).await,
            ["turn-1", "turn-2", "turn-3", "turn-4"]
        );

        runtime.send(status_request("status")).await;
        runtime.send(handoff("handoff")).await;
        let answers = [runtime.answer().await, runtime.answer().await];
        let mut answered = ids(&answers);
        answered.sort_unstable();
        assert_eq!(answered, ["handoff", "status"]);
        let status = answers
            .iter()
            .find(|answer| answer.routing.trace_id == "status")
            .unwrap();
        assert_eq!(status.meta.msg.as_deref(), Some("NODE_STATUS_GET_RESPONSE"));
        assert_eq!(status.routing.src, "uuid-of-the-frontdesk");
        let handoff = answers
            .iter()
            .find(|answer| answer.routing.trace_id == "handoff")
            .unwrap();
        assert_eq!(handoff.payload["type"], json!("frontdesk_result"));
        runtime.no_answer().await;
        assert_eq!(runtime.metrics.turns_queued.load(Ordering::Relaxed), 0);

        // The turns were never held up, and end in their order.
        for id in ["turn-1", "turn-2", "turn-3", "turn-4"] {
            runtime.let_turns_go(1);
            assert_eq!(runtime.answer().await.routing.trace_id, id);
        }
    }

    /// Turns start in the order they came, at most `worker_pool_size` at once, whatever runs
    /// without the model between them.
    #[tokio::test(start_paused = true)]
    async fn turns_start_in_the_order_they_came() {
        let mut runtime = Harness::start(RuntimeConfig {
            worker_pool_size: 2,
            queue_capacity: 8,
            ..RuntimeConfig::default()
        });
        for id in ["turn-1", "turn-2", "turn-3"] {
            runtime.send(turn(id)).await;
        }
        runtime.send(status_request("status")).await;
        for id in ["turn-4", "turn-5", "turn-6"] {
            runtime.send(turn(id)).await;
        }
        assert_eq!(runtime.answer().await.routing.trace_id, "status");
        assert_eq!(runtime.started(3).await, ["turn-1", "turn-2", "status"]);
        assert_eq!(runtime.metrics.turns_queued.load(Ordering::Relaxed), 4);
        assert_eq!(runtime.metrics.turns_waiting.load(Ordering::Relaxed), 4);

        // Each turn that ends gives its worker to the turn that came first of those that wait.
        for (count, (ended, next)) in [
            ("turn-1", "turn-3"),
            ("turn-2", "turn-4"),
            ("turn-3", "turn-5"),
            ("turn-4", "turn-6"),
        ]
        .into_iter()
        .enumerate()
        {
            runtime.let_turns_go(1);
            assert_eq!(runtime.answer().await.routing.trace_id, ended);
            let started = runtime.started(4 + count).await;
            assert_eq!(started.len(), 4 + count);
            assert_eq!(started.last().map(String::as_str), Some(next));
            runtime.no_answer().await;
        }
        for id in ["turn-5", "turn-6"] {
            runtime.let_turns_go(1);
            assert_eq!(runtime.answer().await.routing.trace_id, id);
        }
        assert_eq!(
            runtime.node.started(),
            ["turn-1", "turn-2", "status", "turn-3", "turn-4", "turn-5", "turn-6"]
        );
        assert_eq!(runtime.metrics.turns_waiting.load(Ordering::Relaxed), 0);
    }

    /// The bound on what waits: with every worker busy and the queue full, a turn is answered
    /// `ai_runtime_busy` at once and never run; those that wait run in their order.
    #[tokio::test(start_paused = true)]
    async fn a_turn_past_the_queue_is_answered_busy_and_never_run() {
        let mut runtime = Harness::start(RuntimeConfig {
            worker_pool_size: 1,
            queue_capacity: 2,
            ..RuntimeConfig::default()
        });
        for id in ["turn-1", "turn-2", "turn-3", "turn-4"] {
            runtime.send(turn(id)).await;
        }

        let busy = runtime.answer().await;
        assert_eq!(busy.routing.trace_id, "turn-4");
        assert_eq!(busy.payload["type"], json!("error"));
        assert_eq!(busy.payload["code"], json!("ai_runtime_busy"));
        assert_eq!(busy.payload["retryable"], json!(true));
        assert!(matches!(
            busy.routing.dst,
            Destination::Unicast(ref dst) if dst == "IO.slack.main@motherbee"
        ));
        assert_eq!(busy.meta.thread_id.as_deref(), Some("thread-turn-4"));
        assert_eq!(busy.routing.src, "uuid-of-the-frontdesk");
        assert_eq!(runtime.metrics.turns_refused.load(Ordering::Relaxed), 1);
        assert_eq!(runtime.metrics.turns_waiting.load(Ordering::Relaxed), 2);
        runtime.no_answer().await;

        runtime.let_turns_go(3);
        let answers = [
            runtime.answer().await,
            runtime.answer().await,
            runtime.answer().await,
        ];
        assert_eq!(ids(&answers), ["turn-1", "turn-2", "turn-3"]);
        assert_eq!(runtime.node.started(), ["turn-1", "turn-2", "turn-3"]);
        runtime.no_answer().await;

        // As the workers drain, turns are taken again.
        runtime.send(turn("turn-5")).await;
        runtime.let_turns_go(1);
        assert_eq!(runtime.answer().await.routing.trace_id, "turn-5");
    }

    /// The direct lane's bound: past it a message is dropped, neither run nor answered, and the
    /// place a message gives back takes the next one.
    #[tokio::test(start_paused = true)]
    async fn past_the_direct_lane_a_message_is_dropped_unanswered() {
        let mut runtime = Harness::start(RuntimeConfig {
            direct_capacity: 1,
            ..RuntimeConfig::default()
        });
        let mut slow = status_request("slow");
        slow.meta.msg = Some("SLOW".to_string());
        runtime.send(slow).await;
        assert_eq!(runtime.started(1).await, ["slow"]);

        runtime.send(status_request("dropped")).await;
        runtime.no_answer().await;
        assert_eq!(runtime.node.started(), ["slow"]);
        assert_eq!(runtime.metrics.direct_dropped.load(Ordering::Relaxed), 1);

        runtime.node.slow_control.add_permits(1);
        assert_eq!(runtime.answer().await.routing.trace_id, "slow");
        runtime.send(status_request("status")).await;
        assert_eq!(runtime.answer().await.routing.trace_id, "status");
        assert_eq!(runtime.node.started(), ["slow", "status"]);
    }

    /// A tap copy takes no worker and no place in the queue, and is answered by nothing, not even
    /// `ai_runtime_busy` when the queue is full (FINDINGS A-248).
    #[tokio::test(start_paused = true)]
    async fn a_tap_copy_takes_no_place_and_gets_no_answer_with_the_queue_full() {
        let mut runtime = Harness::start(RuntimeConfig {
            worker_pool_size: 1,
            queue_capacity: 0,
            ..RuntimeConfig::default()
        });
        runtime.send(turn("turn-1")).await;
        assert_eq!(runtime.started(1).await, ["turn-1"]);
        let mut copy = turn("copy");
        copy.meta.via_tap = true;
        runtime.send(copy).await;
        runtime.no_answer().await;
        assert_eq!(
            runtime.metrics.tap_copies_ignored.load(Ordering::Relaxed),
            1
        );
        assert_eq!(runtime.metrics.turns_refused.load(Ordering::Relaxed), 0);

        // The queue was full: a turn is answered busy.
        runtime.send(turn("turn-2")).await;
        assert_eq!(
            runtime.answer().await.payload["code"],
            json!("ai_runtime_busy")
        );
        assert_eq!(runtime.node.started(), ["turn-1"]);
    }

    #[test]
    fn what_runs_without_the_model() {
        for kind in ["system", "SYSTEM", "admin", "Admin"] {
            for name in [
                "NODE_STATUS_GET",
                "CONFIG_SET",
                "VAULT_SECRET_CHANGED",
                "UNREACHABLE",
            ] {
                let mut msg = status_request("control");
                msg.meta.msg_type = kind.to_string();
                msg.meta.msg = Some(name.to_string());
                assert!(runs_without_model(&msg), "{kind} {name}");
            }
        }
        let mut notice = status_request("notice");
        notice.routing.src_l2_name = None;
        notice.meta.msg = Some("UNREACHABLE".to_string());
        assert!(runs_without_model(&notice));

        assert!(runs_without_model(&handoff("handoff")));
        let mut user_kind = handoff("handoff");
        user_kind.meta.msg_type = "USER".to_string();
        assert!(runs_without_model(&user_kind));

        // A conversation's turn, and a WF's message, whatever its name.
        assert!(!runs_without_model(&turn("turn")));
        let mut wf = turn("wf");
        wf.routing.src_l2_name = Some("WF.onboarding@motherbee".to_string());
        wf.meta.msg = Some("forward".to_string());
        assert!(!runs_without_model(&wf));

        // What the frontdesk does not take for a handoff, or takes from no IO node.
        let mut unmarked = handoff("unmarked");
        unmarked.meta.msg = None;
        assert!(!runs_without_model(&unmarked));
        let mut text = handoff("text");
        text.payload = json!({"type": "text", "content": "frontdesk_handoff"});
        assert!(!runs_without_model(&text));
        for sender in [
            None,
            Some(""),
            Some("WF.onboarding@motherbee"),
            Some("AI.sales@worker1"),
            Some("SY.admin@motherbee"),
            Some("io.api@motherbee"),
        ] {
            let mut msg = handoff("handoff");
            msg.routing.src_l2_name = sender.map(str::to_string);
            assert!(!runs_without_model(&msg), "{sender:?}");
        }
        let mut event = handoff("event");
        event.meta.msg_type = "event".to_string();
        assert!(!runs_without_model(&event));
    }

    #[test]
    fn a_lane_says_it_turned_messages_away_at_once_then_once_an_interval() {
        let mut log = RefusalLog::default();
        let start = Instant::now();
        assert_eq!(log.note(start), Some(1));
        assert_eq!(log.note(start + Duration::from_secs(1)), None);
        assert_eq!(log.note(start + Duration::from_secs(9)), None);
        assert_eq!(log.note(start + REFUSAL_LOG_INTERVAL), Some(3));
        assert_eq!(
            log.note(start + REFUSAL_LOG_INTERVAL + Duration::from_secs(1)),
            None
        );
    }
}
