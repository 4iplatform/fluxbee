//! Multiplexed RPC client over a router connection.
//!
//! `RouterDispatcher` owns a single `NodeSender + NodeReceiver` pair and serves two
//! roles: it correlates outgoing requests with incoming responses by
//! `trace_id` against caller-supplied response contracts, and it routes
//! operational traffic that is not a pending response according to a
//! profile-declared, ordered rule table over `(meta.msg_type, meta.msg)`.
//!
//! Design contract (see `docs/onworking COA/orchestrator_internal_rpc_multiplexing_tasks.md`,
//! sections "Decisions taken 2026-05-23" and "Target design v2"):
//!
//! - The router recv loop runs in a dedicated `tokio::spawn` and **never**
//!   awaits operational handlers. This is what prevents the deadlock that
//!   the v1 `OrchestratorRouterClient` introduced.
//! - Each binary declares an `OperationalRouteProfile` in `main` with named
//!   `mpsc` command channels, named `broadcast` observational streams, and
//!   ordered rules over `(meta.msg_type, meta.msg)`. The SDK ships the
//!   routing engine but no predefined `sy_admin()` / `sy_orchestrator()`
//!   profiles — `system_command` / `internal_admin` are not protocol
//!   `msg_type`s, they are profile-derived operational channels.
//! - Completion of a pending RPC is decided by a declarative
//!   `PendingMatcher` supplied by the caller. `trace_id` is only an index.
//! - Late correlated responses after waiter removal are caught by the
//!   recent-stale trace table; orphaned responses after stale TTL are
//!   caught by the response-only registry. Neither falls through to
//!   profile-declared operational workers.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::{self, Duration, Instant as TokioInstant};
use uuid::Uuid;

use crate::node_client::{connect, NodeConfig, NodeError};
use crate::policy::{classify_admin_action, classify_system_message, ActionResult};
use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::split::{ConnectionInfo, ConnectionState};
use crate::split::{NodeReceiver, NodeSender};

pub const ADMIN_KIND: &str = "admin";
pub const MSG_ADMIN_COMMAND: &str = "ADMIN_COMMAND";
pub const MSG_ADMIN_COMMAND_RESPONSE: &str = "ADMIN_COMMAND_RESPONSE";

#[derive(Debug, Clone)]
pub struct AdminCommandRequest<'a> {
    pub admin_target: &'a str,
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub params: Value,
    pub request_id: Option<&'a str>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct AdminCommandResult {
    pub status: String,
    pub action: String,
    pub payload: Value,
    pub action_result: Option<ActionResult>,
    pub result_origin: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<Value>,
    pub request_id: Option<String>,
    pub trace_id: String,
}

/// Soft threshold for command-channel depth gauges. Crossing it upwards
/// emits a single WARN log per crossing (see `CommandChannel::enqueue`).
pub const RPC_COMMAND_DEPTH_WARN_THRESHOLD: usize = 1000;

const OBSERVATIONAL_BROADCAST_CAPACITY: usize = 256;

/// How long a recently-completed waiter's matcher is kept around so a late
/// correlated response can be classified as stale.
const RECENT_STALE_TTL: StdDuration = StdDuration::from_secs(30);

/// Upper bound on the recent-stale table. Oldest entry evicted on overflow.
const RECENT_STALE_MAX: usize = 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("transport unreachable: reason={reason} original_dst={original_dst}")]
    Unreachable {
        reason: String,
        original_dst: String,
    },
    #[error("transport ttl exceeded: original_dst={original_dst} last_hop={last_hop}")]
    TtlExceeded {
        original_dst: String,
        last_hop: String,
    },
    #[error(
        "timeout waiting response trace_id={trace_id} target={target} request_msg={request_msg} response_msg={response_msg} timeout_ms={timeout_ms}"
    )]
    Timeout {
        trace_id: String,
        target: String,
        request_msg: String,
        response_msg: String,
        timeout_ms: u64,
    },
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("response channel closed trace_id={trace_id}")]
    ResponseChannelClosed { trace_id: String },
    #[error("rpc client disconnected")]
    Disconnected,
    #[error("invalid route profile: {0}")]
    InvalidRouteProfile(String),
    #[error("unknown rpc route channel name={name}")]
    UnknownRouteChannel { name: String },
    #[error("rpc command receiver already taken: category={category}")]
    ReceiverAlreadyTaken { category: String },
    #[error("rpc rejected: action={action} error_code={error_code} message={message}")]
    Rejected {
        action: String,
        error_code: String,
        message: String,
    },
}

impl RpcError {
    /// True when the call expired without a response — i.e. the outcome is
    /// **unknown**, not failed: the peer may well have completed the work.
    ///
    /// Exists so callers stop reconstructing this from `to_string()`. Sniffing
    /// the Display is how `sy_architect` silently lost its whole
    /// `timeout_unknown` reconciliation path: it matched
    /// `"timeout waiting ADMIN_COMMAND_RESPONSE"`, a substring this Display has
    /// never produced (the message reads `timeout waiting response …
    /// response_msg=ADMIN_COMMAND_RESPONSE …`), so every expired operation was
    /// recorded as terminally `failed`.
    pub fn is_timeout(&self) -> bool {
        matches!(self, RpcError::Timeout { .. })
    }

    /// For a [`RpcError::Timeout`], the message that was awaited
    /// (e.g. `ADMIN_COMMAND_RESPONSE`). `None` for every other variant.
    pub fn timeout_response_msg(&self) -> Option<&str> {
        match self {
            RpcError::Timeout { response_msg, .. } => Some(response_msg.as_str()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Pending matcher
// ---------------------------------------------------------------------------

/// Declarative matcher for a pending RPC.
///
/// All three vectors reuse the [`RouteMatch`] vocabulary used by the
/// operational profile. The dispatcher checks `success` first, then
/// `terminal_error`, then `invalid_response`. Anything that does not match
/// any of the three is treated as unrelated operational traffic and the
/// waiter stays pending.
///
/// `send_admin_rpc` deliberately keeps `invalid_response = [AnyMsgOfType(ADMIN_KIND)]`
/// (not `[AnyMsgOfType(SYSTEM_KIND)]`), so that admin transport errors
/// (`(SYSTEM_KIND, MSG_UNREACHABLE/TTL_EXCEEDED)`) are caught as exact
/// `terminal_error` matches without flagging every colliding `SYSTEM_KIND`
/// operational message as malformed.
#[derive(Debug, Clone)]
pub struct PendingMatcher {
    pub success: Vec<RouteMatch>,
    pub terminal_error: Vec<RouteMatch>,
    pub invalid_response: Vec<RouteMatch>,
}

impl PendingMatcher {
    pub fn new(
        success: Vec<RouteMatch>,
        terminal_error: Vec<RouteMatch>,
        invalid_response: Vec<RouteMatch>,
    ) -> Self {
        Self {
            success,
            terminal_error,
            invalid_response,
        }
    }

    fn classify(&self, msg_type: &str, msg: Option<&str>) -> MatchOutcome {
        if self.success.iter().any(|m| m.matches(msg_type, msg)) {
            return MatchOutcome::Success;
        }
        if self.terminal_error.iter().any(|m| m.matches(msg_type, msg)) {
            return MatchOutcome::TerminalTransportError;
        }
        if self
            .invalid_response
            .iter()
            .any(|m| m.matches(msg_type, msg))
        {
            return MatchOutcome::InvalidResponse;
        }
        MatchOutcome::Unrelated
    }
}

enum MatchOutcome {
    Success,
    TerminalTransportError,
    InvalidResponse,
    Unrelated,
}

// ---------------------------------------------------------------------------
// Operational route profile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RouteMatch {
    Exact { msg_type: String, msg: String },
    OneOf { msg_type: String, msgs: Vec<String> },
    AnyMsgOfType(String),
    Any,
}

impl RouteMatch {
    pub fn exact(msg_type: impl Into<String>, msg: impl Into<String>) -> Self {
        Self::Exact {
            msg_type: msg_type.into(),
            msg: msg.into(),
        }
    }

    pub fn one_of<I, S>(msg_type: impl Into<String>, msgs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::OneOf {
            msg_type: msg_type.into(),
            msgs: msgs.into_iter().map(Into::into).collect(),
        }
    }

    pub fn any_msg_type(msg_type: impl Into<String>) -> Self {
        Self::AnyMsgOfType(msg_type.into())
    }

    fn matches(&self, msg_type: &str, msg: Option<&str>) -> bool {
        match self {
            RouteMatch::Exact {
                msg_type: t,
                msg: m,
            } => msg_type == t && msg == Some(m.as_str()),
            RouteMatch::OneOf { msg_type: t, msgs } => {
                msg_type == t && msg.map(|m| msgs.iter().any(|v| v == m)).unwrap_or(false)
            }
            RouteMatch::AnyMsgOfType(t) => msg_type == t,
            RouteMatch::Any => true,
        }
    }

    fn is_broad(&self) -> bool {
        matches!(self, RouteMatch::Any | RouteMatch::AnyMsgOfType(_))
    }
}

#[derive(Debug, Clone)]
pub enum RouteTarget {
    Command(&'static str),
    Broadcast(&'static str),
    Drop { reason: &'static str },
}

#[derive(Debug, Clone)]
pub struct OperationalRouteProfile {
    command_channels: Vec<&'static str>,
    broadcast_channels: Vec<&'static str>,
    pre_pending_rules: Vec<(RouteMatch, RouteTarget)>,
    post_pending_rules: Vec<(RouteMatch, RouteTarget)>,
}

impl OperationalRouteProfile {
    pub fn builder() -> OperationalRouteProfileBuilder {
        OperationalRouteProfileBuilder {
            command_channels: Vec::new(),
            broadcast_channels: Vec::new(),
            pre_pending_rules: Vec::new(),
            post_pending_rules: Vec::new(),
        }
    }

    pub fn command_channels(&self) -> &[&'static str] {
        &self.command_channels
    }

    pub fn broadcast_channels(&self) -> &[&'static str] {
        &self.broadcast_channels
    }
}

pub struct OperationalRouteProfileBuilder {
    command_channels: Vec<&'static str>,
    broadcast_channels: Vec<&'static str>,
    pre_pending_rules: Vec<(RouteMatch, RouteTarget)>,
    post_pending_rules: Vec<(RouteMatch, RouteTarget)>,
}

impl OperationalRouteProfileBuilder {
    pub fn command_channel(mut self, name: &'static str) -> Self {
        self.command_channels.push(name);
        self
    }

    pub fn broadcast_channel(mut self, name: &'static str) -> Self {
        self.broadcast_channels.push(name);
        self
    }

    pub fn pre_pending_rule(mut self, when: RouteMatch, target: RouteTarget) -> Self {
        self.pre_pending_rules.push((when, target));
        self
    }

    pub fn post_pending_rule(mut self, when: RouteMatch, target: RouteTarget) -> Self {
        self.post_pending_rules.push((when, target));
        self
    }

    pub fn build(self) -> Result<OperationalRouteProfile, RpcError> {
        // Reject empty / whitespace names.
        for name in self
            .command_channels
            .iter()
            .chain(self.broadcast_channels.iter())
        {
            if name.trim().is_empty() {
                return Err(RpcError::InvalidRouteProfile(
                    "channel name must be non-empty".to_string(),
                ));
            }
        }
        // Reject duplicates within command channels.
        if has_duplicates(&self.command_channels) {
            return Err(RpcError::InvalidRouteProfile(
                "duplicate command channel name".to_string(),
            ));
        }
        // Reject duplicates within broadcast channels.
        if has_duplicates(&self.broadcast_channels) {
            return Err(RpcError::InvalidRouteProfile(
                "duplicate broadcast channel name".to_string(),
            ));
        }
        // Reject collisions between command and broadcast.
        let command_set: HashSet<&'static str> = self.command_channels.iter().copied().collect();
        for bname in &self.broadcast_channels {
            if command_set.contains(bname) {
                return Err(RpcError::InvalidRouteProfile(format!(
                    "channel name {bname} declared as both command and broadcast"
                )));
            }
        }
        let broadcast_set: HashSet<&'static str> =
            self.broadcast_channels.iter().copied().collect();

        // Validate both rule tables independently.
        validate_rule_table(
            "pre_pending",
            &self.pre_pending_rules,
            &command_set,
            &broadcast_set,
        )?;
        validate_rule_table(
            "post_pending",
            &self.post_pending_rules,
            &command_set,
            &broadcast_set,
        )?;

        Ok(OperationalRouteProfile {
            command_channels: self.command_channels,
            broadcast_channels: self.broadcast_channels,
            pre_pending_rules: self.pre_pending_rules,
            post_pending_rules: self.post_pending_rules,
        })
    }
}

fn validate_rule_table(
    table_name: &'static str,
    rules: &[(RouteMatch, RouteTarget)],
    command_set: &HashSet<&'static str>,
    broadcast_set: &HashSet<&'static str>,
) -> Result<(), RpcError> {
    for (idx, (_, target)) in rules.iter().enumerate() {
        match target {
            RouteTarget::Command(name) => {
                if !command_set.contains(name) {
                    return Err(RpcError::InvalidRouteProfile(format!(
                        "{table_name} rule #{idx} targets unknown command channel {name}"
                    )));
                }
            }
            RouteTarget::Broadcast(name) => {
                if !broadcast_set.contains(name) {
                    return Err(RpcError::InvalidRouteProfile(format!(
                        "{table_name} rule #{idx} targets unknown broadcast channel {name}"
                    )));
                }
            }
            RouteTarget::Drop { reason: _ } => {}
        }
    }
    for (broad_idx, (broad, _)) in rules.iter().enumerate().filter(|(_, (m, _))| m.is_broad()) {
        for (idx, (later_match, _)) in rules.iter().enumerate().skip(broad_idx + 1) {
            let unreachable = match broad {
                RouteMatch::Any => true,
                RouteMatch::AnyMsgOfType(t) => match later_match {
                    RouteMatch::Exact { msg_type, .. }
                    | RouteMatch::OneOf { msg_type, .. }
                    | RouteMatch::AnyMsgOfType(msg_type) => msg_type == t,
                    RouteMatch::Any => false,
                },
                _ => false,
            };
            if unreachable {
                return Err(RpcError::InvalidRouteProfile(format!(
                    "{table_name} broad rule #{broad_idx} makes rule #{idx} unreachable"
                )));
            }
        }
    }
    Ok(())
}

fn has_duplicates(list: &[&'static str]) -> bool {
    let mut seen = HashSet::new();
    for name in list {
        if !seen.insert(*name) {
            return true;
        }
    }
    false
}

fn receiver_error_is_connection_loss(err: &NodeError) -> bool {
    matches!(err, NodeError::Disconnected | NodeError::Io(_))
}

// ---------------------------------------------------------------------------
// Internal data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SystemRpcRequest<'a> {
    pub target: &'a str,
    pub request_msg: &'a str,
    pub response_msg: &'a str,
    pub payload: Value,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct RpcRequestLabels {
    pub target: String,
    pub request_msg: String,
    pub response_msg: String,
}

impl RpcRequestLabels {
    pub fn new(
        target: impl Into<String>,
        request_msg: impl Into<String>,
        response_msg: impl Into<String>,
    ) -> Self {
        Self {
            target: target.into(),
            request_msg: request_msg.into(),
            response_msg: response_msg.into(),
        }
    }
}

struct PendingEntry {
    matcher: PendingMatcher,
    response_tx: oneshot::Sender<Result<Message, RpcError>>,
}

struct StaleEntry {
    matcher: PendingMatcher,
    expires_at: TokioInstant,
}

struct RecentStaleTable {
    entries: HashMap<String, StaleEntry>,
    order: VecDeque<String>,
    ttl: Duration,
    max_size: usize,
}

impl RecentStaleTable {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            ttl: Duration::from_secs(RECENT_STALE_TTL.as_secs()),
            max_size: RECENT_STALE_MAX,
        }
    }

    fn insert(&mut self, trace_id: String, matcher: PendingMatcher) {
        self.gc(TokioInstant::now());
        if self.entries.len() >= self.max_size {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        let expires_at = TokioInstant::now() + self.ttl;
        if self
            .entries
            .insert(
                trace_id.clone(),
                StaleEntry {
                    matcher,
                    expires_at,
                },
            )
            .is_none()
        {
            self.order.push_back(trace_id);
        }
    }

    fn classify(&mut self, msg: &Message) -> StaleClassification {
        self.gc(TokioInstant::now());
        let Some(entry) = self.entries.get(&msg.routing.trace_id) else {
            return StaleClassification::NotStale;
        };
        match entry
            .matcher
            .classify(msg.meta.msg_type.as_str(), msg.meta.msg.as_deref())
        {
            MatchOutcome::Success
            | MatchOutcome::TerminalTransportError
            | MatchOutcome::InvalidResponse => StaleClassification::Stale,
            MatchOutcome::Unrelated => StaleClassification::NotStale,
        }
    }

    fn gc(&mut self, now: TokioInstant) {
        while let Some(front) = self.order.front() {
            let Some(entry) = self.entries.get(front) else {
                self.order.pop_front();
                continue;
            };
            if entry.expires_at <= now {
                let removed = self.order.pop_front().unwrap();
                self.entries.remove(&removed);
            } else {
                break;
            }
        }
    }
}

enum StaleClassification {
    Stale,
    NotStale,
}

struct CommandChannel {
    sender: mpsc::UnboundedSender<Message>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Message>>>,
    depth: Arc<AtomicUsize>,
    name: &'static str,
}

impl CommandChannel {
    fn new(name: &'static str) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            depth: Arc::new(AtomicUsize::new(0)),
            name,
        }
    }

    fn enqueue(&self, msg: Message) {
        let prev = self.depth.fetch_add(1, Ordering::Relaxed);
        let new_depth = prev + 1;
        if self.sender.send(msg).is_err() {
            // No live consumer — revert the gauge increment.
            self.depth.fetch_sub(1, Ordering::Relaxed);
            tracing::debug!(
                category = self.name,
                "rpc command channel send failed; no consumer attached"
            );
            return;
        }
        if new_depth == RPC_COMMAND_DEPTH_WARN_THRESHOLD {
            tracing::warn!(
                category = self.name,
                depth = new_depth,
                threshold = RPC_COMMAND_DEPTH_WARN_THRESHOLD,
                "rpc command channel depth crossed soft threshold"
            );
        }
    }

    async fn take_receiver(&self) -> Result<RpcCommandReceiver, RpcError> {
        let mut guard = self.receiver.lock().await;
        let inner = guard.take().ok_or_else(|| RpcError::ReceiverAlreadyTaken {
            category: self.name.to_string(),
        })?;
        Ok(RpcCommandReceiver {
            inner,
            depth: Arc::clone(&self.depth),
        })
    }

    fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct RpcCommandReceiver {
    inner: mpsc::UnboundedReceiver<Message>,
    depth: Arc<AtomicUsize>,
}

impl RpcCommandReceiver {
    pub async fn recv(&mut self) -> Option<Message> {
        let msg = self.inner.recv().await?;
        self.depth.fetch_sub(1, Ordering::Relaxed);
        Some(msg)
    }

    pub fn try_recv(&mut self) -> Option<Message> {
        match self.inner.try_recv() {
            Ok(msg) => {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                Some(msg)
            }
            Err(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// RouterDispatcher
// ---------------------------------------------------------------------------

pub struct RouterDispatcher {
    sender: NodeSender,
    profile: OperationalRouteProfile,
    command: HashMap<&'static str, CommandChannel>,
    broadcasts: HashMap<&'static str, broadcast::Sender<Message>>,
    pending: Mutex<HashMap<String, PendingEntry>>,
    stale: Mutex<RecentStaleTable>,
    response_only: Mutex<HashSet<RouteMatch>>,
    metric_stale_responses: AtomicU64,
    metric_unknown_responses: AtomicU64,
    metric_route_unmatched: AtomicU64,
}

impl RouterDispatcher {
    /// Connect to the local router and start the dispatcher loop. Retries
    /// indefinitely on transient connect failures, waiting `delay` between
    /// attempts.
    pub async fn connect_with_retry(
        config: NodeConfig,
        delay: Duration,
        profile: OperationalRouteProfile,
    ) -> Result<Arc<Self>, NodeError> {
        loop {
            match connect(&config).await {
                Ok((sender, receiver)) => {
                    return Ok(Self::start(sender, receiver, profile));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "rpc client connect failed; retrying");
                    time::sleep(delay).await;
                }
            }
        }
    }

    /// Build a client around in-process channel fixtures.
    fn from_test_channels(
        sender: NodeSender,
        receiver: NodeReceiver,
        profile: OperationalRouteProfile,
    ) -> Arc<Self> {
        Self::start(sender, receiver, profile)
    }

    fn start(
        sender: NodeSender,
        receiver: NodeReceiver,
        profile: OperationalRouteProfile,
    ) -> Arc<Self> {
        let command: HashMap<&'static str, CommandChannel> = profile
            .command_channels
            .iter()
            .map(|name| (*name, CommandChannel::new(name)))
            .collect();
        let broadcasts: HashMap<&'static str, broadcast::Sender<Message>> = profile
            .broadcast_channels
            .iter()
            .map(|name| {
                let (tx, _) = broadcast::channel(OBSERVATIONAL_BROADCAST_CAPACITY);
                (*name, tx)
            })
            .collect();

        let client = Arc::new(Self {
            sender,
            profile,
            command,
            broadcasts,
            pending: Mutex::new(HashMap::new()),
            stale: Mutex::new(RecentStaleTable::new()),
            response_only: Mutex::new(HashSet::new()),
            metric_stale_responses: AtomicU64::new(0),
            metric_unknown_responses: AtomicU64::new(0),
            metric_route_unmatched: AtomicU64::new(0),
        });

        let task_client = Arc::clone(&client);
        tokio::spawn(async move {
            task_client.recv_loop(receiver).await;
        });

        client
    }

    pub fn sender_snapshot(&self) -> NodeSender {
        self.sender.clone()
    }

    pub async fn take_command_receiver(&self, name: &str) -> Result<RpcCommandReceiver, RpcError> {
        let channel = self
            .command
            .get(name)
            .ok_or_else(|| RpcError::UnknownRouteChannel {
                name: name.to_string(),
            })?;
        channel.take_receiver().await
    }

    pub fn subscribe(&self, name: &str) -> Result<broadcast::Receiver<Message>, RpcError> {
        let tx = self
            .broadcasts
            .get(name)
            .ok_or_else(|| RpcError::UnknownRouteChannel {
                name: name.to_string(),
            })?;
        Ok(tx.subscribe())
    }

    /// Cancel every pending waiter with `RpcError::Disconnected`. Also
    /// snapshots their matchers into the recent-stale table so the
    /// dispatcher classifies late correlated responses as stale instead of
    /// routing them operationally.
    pub async fn drain_pending_waiters(&self) {
        let mut pending = self.pending.lock().await;
        let mut stale = self.stale.lock().await;
        for (trace_id, entry) in pending.drain() {
            stale.insert(trace_id, entry.matcher);
            let _ = entry.response_tx.send(Err(RpcError::Disconnected));
        }
    }

    /// Depth of a command channel. Returns `None` for names not in the
    /// profile.
    pub fn command_depth(&self, name: &str) -> Option<usize> {
        self.command.get(name).map(|c| c.depth())
    }

    pub fn metric_stale_responses(&self) -> u64 {
        self.metric_stale_responses.load(Ordering::Relaxed)
    }

    pub fn metric_unknown_responses(&self) -> u64 {
        self.metric_unknown_responses.load(Ordering::Relaxed)
    }

    pub fn metric_route_unmatched(&self) -> u64 {
        self.metric_route_unmatched.load(Ordering::Relaxed)
    }

    async fn recv_loop(self: Arc<Self>, mut receiver: NodeReceiver) {
        loop {
            match receiver.recv().await {
                Ok(msg) => self.dispatch(msg).await,
                Err(err) if receiver_error_is_connection_loss(&err) => {
                    tracing::warn!(
                        error = %err,
                        "rpc router recv connection lost; draining pending waiters"
                    );
                    self.drain_pending_waiters().await;
                    // The SDK's connection manager reconnects transparently
                    // and resumes feeding this receiver. Keep looping; if
                    // recv permanently fails the next iteration also errors
                    // out and we drain again (idempotent).
                }
                Err(err) => {
                    tracing::warn!(error = %err, "rpc router recv error");
                }
            }
        }
    }

    async fn dispatch(&self, msg: Message) {
        let trace_id = msg.routing.trace_id.clone();
        let action = self.classify(msg, &trace_id).await;
        match action {
            DispatchAction::CompleteSuccess(entry, msg) => {
                self.note_stale(trace_id, entry.matcher.clone()).await;
                let _ = entry.response_tx.send(Ok(msg));
            }
            DispatchAction::CompleteTransportError(entry, msg) => {
                let err = transport_error_from_message(&msg);
                self.note_stale(trace_id, entry.matcher.clone()).await;
                let _ = entry.response_tx.send(Err(err));
            }
            DispatchAction::CompleteInvalidResponse(entry, msg) => {
                let err = RpcError::InvalidResponse(format!(
                    "unexpected response shape msg_type={} msg={:?}",
                    msg.meta.msg_type, msg.meta.msg
                ));
                self.note_stale(trace_id, entry.matcher.clone()).await;
                let _ = entry.response_tx.send(Err(err));
            }
            DispatchAction::Stale(msg) => {
                self.metric_stale_responses.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    trace_id = %msg.routing.trace_id,
                    msg_type = %msg.meta.msg_type,
                    msg = ?msg.meta.msg,
                    "rpc dispatcher: stale correlated response discarded"
                );
            }
            DispatchAction::UnknownResponse(msg) => {
                self.metric_unknown_responses
                    .fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    trace_id = %msg.routing.trace_id,
                    msg_type = %msg.meta.msg_type,
                    msg = ?msg.meta.msg,
                    "rpc dispatcher: orphaned response-only shape discarded"
                );
            }
            DispatchAction::RouteByProfile(msg, target) => self.route_target(msg, target),
            DispatchAction::Unmatched(msg) => {
                self.metric_route_unmatched.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    msg_type = %msg.meta.msg_type,
                    msg = ?msg.meta.msg,
                    "rpc dispatcher: no matching rule for message"
                );
            }
        }
    }

    async fn classify(&self, msg: Message, trace_id: &str) -> DispatchAction {
        // 1. Operational commands that are never responses go first. This
        // protects them from colliding trace ids and broad invalid_response
        // matchers.
        if let Some(target) = self.match_profile_rules(&self.profile.pre_pending_rules, &msg) {
            return DispatchAction::RouteByProfile(msg, target);
        }

        // 2. Active pending waiter for this trace_id.
        {
            let mut pending = self.pending.lock().await;
            if let Some(entry_ref) = pending.get(trace_id) {
                match entry_ref
                    .matcher
                    .classify(msg.meta.msg_type.as_str(), msg.meta.msg.as_deref())
                {
                    MatchOutcome::Success => {
                        let entry = pending.remove(trace_id).expect("entry present");
                        return DispatchAction::CompleteSuccess(entry, msg);
                    }
                    MatchOutcome::TerminalTransportError => {
                        let entry = pending.remove(trace_id).expect("entry present");
                        return DispatchAction::CompleteTransportError(entry, msg);
                    }
                    MatchOutcome::InvalidResponse => {
                        let entry = pending.remove(trace_id).expect("entry present");
                        return DispatchAction::CompleteInvalidResponse(entry, msg);
                    }
                    MatchOutcome::Unrelated => {
                        // Fall through to operational routing without
                        // touching the waiter.
                    }
                }
            }
        }
        // 3. Recent-stale trace? Drop with metric.
        {
            let mut stale = self.stale.lock().await;
            if let StaleClassification::Stale = stale.classify(&msg) {
                return DispatchAction::Stale(msg);
            }
        }

        // 4. Response-only shape orphan? Drop with metric.
        let response_only = {
            let registry = self.response_only.lock().await;
            registry
                .iter()
                .any(|m| m.matches(msg.meta.msg_type.as_str(), msg.meta.msg.as_deref()))
        };
        if response_only {
            return DispatchAction::UnknownResponse(msg);
        }

        // 5. Observational fan-out and broad operational catch-alls.
        if let Some(target) = self.match_profile_rules(&self.profile.post_pending_rules, &msg) {
            return DispatchAction::RouteByProfile(msg, target);
        }

        // 6. Unknown operational message.
        DispatchAction::Unmatched(msg)
    }

    async fn note_stale(&self, trace_id: String, matcher: PendingMatcher) {
        let mut stale = self.stale.lock().await;
        stale.insert(trace_id, matcher);
    }

    fn match_profile_rules(
        &self,
        rules: &[(RouteMatch, RouteTarget)],
        msg: &Message,
    ) -> Option<RouteTarget> {
        let msg_type = msg.meta.msg_type.as_str();
        let msg_name = msg.meta.msg.as_deref();
        rules
            .iter()
            .find(|(rule, _)| rule.matches(msg_type, msg_name))
            .map(|(_, target)| target.clone())
    }

    fn route_target(&self, msg: Message, target: RouteTarget) {
        let msg_type = msg.meta.msg_type.clone();
        let msg_name = msg.meta.msg.clone();
        match target {
            RouteTarget::Command(name) => {
                if let Some(channel) = self.command.get(name) {
                    channel.enqueue(msg);
                } else {
                    tracing::error!(
                        channel = %name,
                        "rpc dispatcher: command channel missing despite passing profile validation"
                    );
                }
            }
            RouteTarget::Broadcast(name) => {
                if let Some(tx) = self.broadcasts.get(name) {
                    let _ = tx.send(msg);
                } else {
                    tracing::error!(
                        channel = %name,
                        "rpc dispatcher: broadcast channel missing despite passing profile validation"
                    );
                }
            }
            RouteTarget::Drop { reason } => {
                tracing::debug!(
                    reason = %reason,
                    msg_type = %msg_type,
                    msg = ?msg_name,
                    "rpc dispatcher: profile-directed drop"
                );
            }
        }
    }

    async fn register_response_only(&self, matcher: &PendingMatcher) {
        let mut registry = self.response_only.lock().await;
        for rule in &matcher.success {
            match rule {
                RouteMatch::Exact { msg_type, msg } => {
                    if !self.post_pending_declares_observational_exact(msg_type, msg) {
                        registry.insert(rule.clone());
                    }
                }
                RouteMatch::OneOf { msg_type, msgs } => {
                    for msg in msgs {
                        if !self.post_pending_declares_observational_exact(msg_type, msg) {
                            registry.insert(RouteMatch::exact(msg_type.clone(), msg.clone()));
                        }
                    }
                }
                RouteMatch::AnyMsgOfType(msg_type) => {
                    if !self.post_pending_declares_observational_family(msg_type) {
                        registry.insert(rule.clone());
                    }
                }
                RouteMatch::Any => {
                    tracing::debug!(
                        "rpc response-only registry skipped Any success matcher to avoid global drops"
                    );
                }
            }
        }
    }

    /// AF-P2b: a success shape is observational-exempt from the response-only
    /// registry **only** when the matching post_pending rule routes to
    /// `RouteTarget::Broadcast(_)`. A rule that routes to `Command` (e.g.
    /// the orchestrator's broad `AnyMsgOfType(SYSTEM_KIND) -> Command("system")`
    /// catch-all) must NOT exempt response shapes — otherwise a late
    /// correlated response after the stale TTL would slip through to a
    /// worker as if it were a brand-new command.
    fn post_pending_declares_observational_exact(&self, msg_type: &str, msg: &str) -> bool {
        self.profile
            .post_pending_rules
            .iter()
            .any(|(rule, target)| {
                if !matches!(target, RouteTarget::Broadcast(_)) {
                    return false;
                }
                match rule {
                    RouteMatch::Exact {
                        msg_type: rule_type,
                        msg: rule_msg,
                    } => rule_type == msg_type && rule_msg == msg,
                    RouteMatch::OneOf {
                        msg_type: rule_type,
                        msgs,
                    } => rule_type == msg_type && msgs.iter().any(|rule_msg| rule_msg == msg),
                    RouteMatch::AnyMsgOfType(rule_type) => rule_type == msg_type,
                    RouteMatch::Any => false,
                }
            })
    }

    /// AF-P2b: same as `post_pending_declares_observational_exact` but for
    /// `AnyMsgOfType` matchers. Only exempts if the target is broadcast.
    fn post_pending_declares_observational_family(&self, msg_type: &str) -> bool {
        self.profile
            .post_pending_rules
            .iter()
            .any(|(rule, target)| {
                matches!(rule, RouteMatch::AnyMsgOfType(rule_type) if rule_type == msg_type)
                    && matches!(target, RouteTarget::Broadcast(_))
            })
    }

    pub async fn send_with_matcher(
        &self,
        mut outgoing: Message,
        matcher: PendingMatcher,
        labels: RpcRequestLabels,
        timeout: Duration,
    ) -> Result<Message, RpcError> {
        // AF-P2a: fail fast if the router connection is down. Otherwise the
        // SDK enqueues into its internal mpsc, the connection manager drains
        // it without sending, and we'd wait the full `timeout` for a reply
        // that can never arrive. Returning `Disconnected` lets the caller
        // decide whether to retry, log, or surface immediately.
        if !self.sender.is_connected() {
            return Err(RpcError::Disconnected);
        }

        let timeout = default_rpc_timeout(timeout);
        if outgoing.routing.trace_id.trim().is_empty() {
            outgoing.routing.trace_id = Uuid::new_v4().to_string();
        }
        if outgoing.routing.src.trim().is_empty() {
            outgoing.routing.src = self.sender.uuid().to_string();
        }
        let trace_id = outgoing.routing.trace_id.clone();

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.contains_key(&trace_id) {
                return Err(RpcError::InvalidRequest(format!(
                    "duplicate active trace_id {trace_id}"
                )));
            }
            pending.insert(
                trace_id.clone(),
                PendingEntry {
                    matcher: matcher.clone(),
                    response_tx: tx,
                },
            );
        }
        if let Err(err) = self.sender.send(outgoing).await {
            self.pending.lock().await.remove(&trace_id);
            // AF-P2a: normalize disconnect to a single `RpcError` variant so
            // callers can `match` without unwrapping `RpcError::Node(...)`.
            return Err(match err {
                NodeError::Disconnected => RpcError::Disconnected,
                other => other.into(),
            });
        }
        self.register_response_only(&matcher).await;

        match time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::ResponseChannelClosed { trace_id }),
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&trace_id);
                drop(pending);
                self.note_stale(trace_id.clone(), matcher).await;
                Err(RpcError::Timeout {
                    trace_id,
                    target: labels.target,
                    request_msg: labels.request_msg,
                    response_msg: labels.response_msg,
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    pub async fn send_system_rpc(
        &self,
        request: SystemRpcRequest<'_>,
    ) -> Result<Message, RpcError> {
        let target = request.target.trim();
        let request_msg = request.request_msg.trim();
        let response_msg = request.response_msg.trim();
        if target.is_empty() {
            return Err(RpcError::InvalidRequest(
                "target must be non-empty".to_string(),
            ));
        }
        if request_msg.is_empty() {
            return Err(RpcError::InvalidRequest(
                "request_msg must be non-empty".to_string(),
            ));
        }
        if response_msg.is_empty() {
            return Err(RpcError::InvalidRequest(
                "response_msg must be non-empty".to_string(),
            ));
        }
        let matcher = PendingMatcher::new(
            vec![RouteMatch::exact(SYSTEM_KIND, response_msg)],
            vec![
                RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
                RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
            ],
            vec![RouteMatch::any_msg_type(SYSTEM_KIND)],
        );
        let outgoing = Message {
            routing: Routing {
                src: self.sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(target.to_string()),
                ttl: 16,
                trace_id: String::new(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(request_msg.to_string()),
                target: None,
                action: None,
                action_class: classify_system_message(request_msg),
                ..Meta::default()
            },
            payload: request.payload,
        };
        self.send_with_matcher(
            outgoing,
            matcher,
            RpcRequestLabels::new(target, request_msg, response_msg),
            request.timeout,
        )
        .await
    }

    pub async fn send_admin_rpc(
        &self,
        request: AdminCommandRequest<'_>,
    ) -> Result<AdminCommandResult, RpcError> {
        let admin_target = request.admin_target.trim();
        let action = request.action.trim();
        if admin_target.is_empty() {
            return Err(RpcError::InvalidRequest(
                "admin_target must be non-empty".to_string(),
            ));
        }
        if action.is_empty() {
            return Err(RpcError::InvalidRequest(
                "action must be non-empty".to_string(),
            ));
        }
        if !request.params.is_null() && !request.params.is_object() {
            return Err(RpcError::InvalidRequest(
                "params must be a JSON object or null".to_string(),
            ));
        }
        let timeout = default_rpc_timeout(request.timeout);
        let request_id = request
            .request_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut payload = json!({
            "action": action,
            "params": request.params,
            "request_id": request_id,
        });
        if let Some(target) = request.target.map(str::trim).filter(|s| !s.is_empty()) {
            payload["target"] = Value::String(target.to_string());
        }
        let matcher = PendingMatcher::new(
            vec![RouteMatch::exact(ADMIN_KIND, MSG_ADMIN_COMMAND_RESPONSE)],
            vec![
                RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
                RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
            ],
            // Note: SYSTEM_KIND is NOT in invalid_response_families. Unrelated
            // SYSTEM_KIND traffic with a colliding trace stays operational.
            vec![RouteMatch::any_msg_type(ADMIN_KIND)],
        );
        let outgoing = Message {
            routing: Routing {
                src: self.sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(admin_target.to_string()),
                ttl: 16,
                trace_id: String::new(),
            },
            meta: Meta {
                msg_type: ADMIN_KIND.to_string(),
                msg: Some(MSG_ADMIN_COMMAND.to_string()),
                target: Some(admin_target.to_string()),
                action: Some(action.to_string()),
                action_class: classify_admin_action(action),
                ..Meta::default()
            },
            payload,
        };
        let response = self
            .send_with_matcher(
                outgoing,
                matcher,
                RpcRequestLabels::new(admin_target, MSG_ADMIN_COMMAND, MSG_ADMIN_COMMAND_RESPONSE),
                timeout,
            )
            .await?;
        let trace_id = response.routing.trace_id.clone();
        parse_admin_response(action, trace_id, response)
    }
}

enum DispatchAction {
    RouteByProfile(Message, RouteTarget),
    CompleteSuccess(PendingEntry, Message),
    CompleteTransportError(PendingEntry, Message),
    CompleteInvalidResponse(PendingEntry, Message),
    Stale(Message),
    UnknownResponse(Message),
    Unmatched(Message),
}

#[derive(Debug, Deserialize, Default)]
struct UnreachablePayloadInner {
    #[serde(default)]
    original_dst: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize, Default)]
struct TtlExceededPayloadInner {
    #[serde(default)]
    original_dst: String,
    #[serde(default)]
    last_hop: String,
}

fn transport_error_from_message(msg: &Message) -> RpcError {
    match msg.meta.msg.as_deref() {
        Some(MSG_UNREACHABLE) => {
            let payload = serde_json::from_value::<UnreachablePayloadInner>(msg.payload.clone())
                .unwrap_or_default();
            RpcError::Unreachable {
                reason: payload.reason,
                original_dst: payload.original_dst,
            }
        }
        Some(MSG_TTL_EXCEEDED) => {
            let payload = serde_json::from_value::<TtlExceededPayloadInner>(msg.payload.clone())
                .unwrap_or_default();
            RpcError::TtlExceeded {
                original_dst: payload.original_dst,
                last_hop: payload.last_hop,
            }
        }
        other => {
            RpcError::InvalidResponse(format!("expected transport error message, got {other:?}"))
        }
    }
}

fn default_rpc_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        Duration::from_secs(5)
    } else {
        timeout
    }
}

pub fn parse_admin_response(
    requested_action: &str,
    trace_id: String,
    message: Message,
) -> Result<AdminCommandResult, RpcError> {
    let payload = message.payload;
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::InvalidResponse("missing status".to_string()))?
        .to_string();
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(requested_action)
        .to_string();
    let payload_value = admin_response_payload_value(&payload);
    let error_code = payload
        .get("error_code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_detail =
        payload
            .get("error_detail")
            .cloned()
            .and_then(|v| if v.is_null() { None } else { Some(v) });
    let request_id = payload
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(AdminCommandResult {
        status,
        action,
        payload: payload_value,
        action_result: message.meta.action_result,
        result_origin: message.meta.result_origin,
        error_code,
        error_detail,
        request_id,
        trace_id,
    })
}

pub fn admin_response_payload_value(payload: &Value) -> Value {
    if let Some(value) = payload.get("payload") {
        return value.clone();
    }
    let Some(mut object) = payload.as_object().cloned() else {
        return Value::Null;
    };
    for key in [
        "status",
        "action",
        "error_code",
        "error_detail",
        "request_id",
        "trace_id",
    ] {
        object.remove(key);
    }
    if object.is_empty() {
        Value::Null
    } else {
        Value::Object(object)
    }
}

pub fn extract_error_message<'a>(
    payload: &'a Value,
    error_detail: Option<&'a Value>,
) -> Option<&'a str> {
    if let Some(message) = error_detail
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        return Some(message);
    }
    if let Some(message) = error_detail
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        return Some(message);
    }
    payload
        .get("message")
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Test harness that wires a `RouterDispatcher` over in-process channels. Use it
/// from downstream crate tests (orchestrator, sy_admin) to exercise the
/// real dispatcher / matcher path without a router.
pub struct RouterDispatcherTestHarness {
    outbound_rx: mpsc::Receiver<Vec<u8>>,
    inbound_tx: mpsc::Sender<Result<Message, NodeError>>,
    sender_uuid: String,
}

impl RouterDispatcherTestHarness {
    pub fn new(full_name: &str, profile: OperationalRouteProfile) -> (Arc<RouterDispatcher>, Self) {
        Self::new_with_uuid("test-uuid", full_name, profile)
    }

    pub fn new_with_uuid(
        uuid: &str,
        full_name: &str,
        profile: OperationalRouteProfile,
    ) -> (Arc<RouterDispatcher>, Self) {
        let (outbound_tx, outbound_rx) = mpsc::channel(64);
        let (inbound_tx, inbound_rx) = mpsc::channel(64);
        let state = Arc::new(ConnectionState::new_connected());
        let info = Arc::new(ConnectionInfo::new(
            uuid.to_string(),
            full_name.to_string(),
            7,
            "router-test".to_string(),
            state,
        ));
        let sender = NodeSender::new(outbound_tx, Arc::clone(&info));
        let receiver = NodeReceiver::new(inbound_rx, info);
        let client = RouterDispatcher::from_test_channels(sender, receiver, profile);
        (
            client,
            Self {
                outbound_rx,
                inbound_tx,
                sender_uuid: uuid.to_string(),
            },
        )
    }

    pub fn sender_uuid(&self) -> &str {
        &self.sender_uuid
    }

    pub async fn next_outgoing(&mut self) -> Option<Message> {
        let frame = self.outbound_rx.recv().await?;
        serde_json::from_slice(&frame).ok()
    }

    pub async fn next_outgoing_within(&mut self, timeout: Duration) -> Option<Message> {
        time::timeout(timeout, self.next_outgoing())
            .await
            .ok()
            .flatten()
    }

    pub async fn inject(&self, message: Message) -> Result<(), NodeError> {
        self.inbound_tx
            .send(Ok(message))
            .await
            .map_err(|_| NodeError::Disconnected)
    }

    pub async fn inject_error(&self, error: NodeError) -> Result<(), NodeError> {
        self.inbound_tx
            .send(Err(error))
            .await
            .map_err(|_| NodeError::Disconnected)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::time::sleep;

    fn simple_profile() -> OperationalRouteProfile {
        OperationalRouteProfile::builder()
            .command_channel("admin")
            .command_channel("system")
            .pre_pending_rule(
                RouteMatch::exact(ADMIN_KIND, MSG_ADMIN_COMMAND),
                RouteTarget::Command("admin"),
            )
            .post_pending_rule(
                RouteMatch::any_msg_type(ADMIN_KIND),
                RouteTarget::Command("admin"),
            )
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                RouteTarget::Command("system"),
            )
            .build()
            .expect("simple profile")
    }

    fn make_response(outgoing: &Message, msg_type: &str, msg: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "responder-uuid".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(outgoing.routing.src.clone()),
                ttl: 16,
                trace_id: outgoing.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    fn make_loose(trace_id: &str, msg_type: &str, msg: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "other-uuid".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("test-uuid".to_string()),
                ttl: 16,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    fn make_outgoing(msg_type: &str, msg: &str) -> Message {
        Message {
            routing: Routing {
                src: String::new(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.target@hive".to_string()),
                ttl: 16,
                trace_id: String::new(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    // ---- Profile builder validations ----

    #[test]
    fn profile_builder_rejects_empty_name() {
        let err = OperationalRouteProfile::builder()
            .command_channel("")
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_duplicate_command_channels() {
        let err = OperationalRouteProfile::builder()
            .command_channel("admin")
            .command_channel("admin")
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_duplicate_broadcast_channels() {
        let err = OperationalRouteProfile::builder()
            .broadcast_channel("status")
            .broadcast_channel("status")
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_command_broadcast_collision() {
        let err = OperationalRouteProfile::builder()
            .command_channel("status")
            .broadcast_channel("status")
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_rule_to_unknown_command() {
        let err = OperationalRouteProfile::builder()
            .command_channel("admin")
            .pre_pending_rule(
                RouteMatch::exact(ADMIN_KIND, MSG_ADMIN_COMMAND),
                RouteTarget::Command("internal_admin"),
            )
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_rule_to_unknown_broadcast() {
        let err = OperationalRouteProfile::builder()
            .broadcast_channel("status")
            .post_pending_rule(RouteMatch::Any, RouteTarget::Broadcast("query"))
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_broad_unreachable_rule() {
        let err = OperationalRouteProfile::builder()
            .command_channel("system")
            .command_channel("system_command")
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                RouteTarget::Command("system"),
            )
            .post_pending_rule(
                RouteMatch::exact(SYSTEM_KIND, "CONFIG_GET"),
                RouteTarget::Command("system_command"),
            )
            .build()
            .unwrap_err();
        assert!(matches!(err, RpcError::InvalidRouteProfile(_)));
    }

    #[test]
    fn profile_builder_rejects_later_broad_unreachable_rule() {
        let err = OperationalRouteProfile::builder()
            .command_channel("system")
            .command_channel("admin")
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                RouteTarget::Command("system"),
            )
            .post_pending_rule(
                RouteMatch::any_msg_type(ADMIN_KIND),
                RouteTarget::Command("admin"),
            )
            .post_pending_rule(
                RouteMatch::exact(ADMIN_KIND, MSG_ADMIN_COMMAND),
                RouteTarget::Command("admin"),
            )
            .build()
            .unwrap_err();
        assert!(format!("{err}").contains("broad rule #1 makes rule #2 unreachable"));
    }

    #[tokio::test]
    async fn unknown_take_command_receiver_returns_unknown_route_channel() {
        let (client, _harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let err = client.take_command_receiver("missing").await.unwrap_err();
        assert!(matches!(err, RpcError::UnknownRouteChannel { .. }));
    }

    #[tokio::test]
    async fn unknown_subscribe_returns_unknown_route_channel() {
        let (client, _harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let err = client.subscribe("missing").unwrap_err();
        assert!(matches!(err, RpcError::UnknownRouteChannel { .. }));
    }

    // ---- Operational dispatch with ordered rules ----

    #[tokio::test]
    async fn ordered_first_match_wins_one_of_before_broad() {
        let profile = OperationalRouteProfile::builder()
            .command_channel("system_command")
            .command_channel("system")
            .post_pending_rule(
                RouteMatch::one_of(SYSTEM_KIND, ["CONFIG_GET", "CONFIG_SET"]),
                RouteTarget::Command("system_command"),
            )
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                RouteTarget::Command("system"),
            )
            .build()
            .unwrap();
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut sc = client
            .take_command_receiver("system_command")
            .await
            .unwrap();
        let mut sys = client.take_command_receiver("system").await.unwrap();

        harness
            .inject(make_loose("t-1", SYSTEM_KIND, "CONFIG_GET", json!({})))
            .await
            .unwrap();
        harness
            .inject(make_loose("t-2", SYSTEM_KIND, "SYSTEM_UPDATE", json!({})))
            .await
            .unwrap();

        let routed_sc = time::timeout(Duration::from_secs(1), sc.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed_sc.meta.msg.as_deref(), Some("CONFIG_GET"));
        let routed_sys = time::timeout(Duration::from_secs(1), sys.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed_sys.meta.msg.as_deref(), Some("SYSTEM_UPDATE"));
    }

    #[tokio::test]
    async fn named_broadcast_subscriber_receives_post_pending_match() {
        let profile = OperationalRouteProfile::builder()
            .broadcast_channel("config_response")
            .post_pending_rule(
                RouteMatch::exact(SYSTEM_KIND, "CONFIG_RESPONSE"),
                RouteTarget::Broadcast("config_response"),
            )
            .build()
            .unwrap();
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut sub = client.subscribe("config_response").unwrap();

        harness
            .inject(make_loose(
                "t-1",
                SYSTEM_KIND,
                "CONFIG_RESPONSE",
                json!({"v": 1}),
            ))
            .await
            .unwrap();

        let routed = time::timeout(Duration::from_secs(1), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.meta.msg.as_deref(), Some("CONFIG_RESPONSE"));
    }

    // ---- Pending dispatch by trace_id ----

    #[tokio::test]
    async fn dispatch_by_trace_id_with_concurrent_rpcs() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c1 = Arc::clone(&client);
        let c2 = Arc::clone(&client);
        let h1 = tokio::spawn(async move {
            c1.send_system_rpc(SystemRpcRequest {
                target: "SY.target@hive",
                request_msg: "ONE_REQ",
                response_msg: "ONE_RESP",
                payload: json!({"i": 1}),
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let h2 = tokio::spawn(async move {
            c2.send_system_rpc(SystemRpcRequest {
                target: "SY.target@hive",
                request_msg: "TWO_REQ",
                response_msg: "TWO_RESP",
                payload: json!({"i": 2}),
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let out1 = harness.next_outgoing().await.unwrap();
        let out2 = harness.next_outgoing().await.unwrap();
        harness
            .inject(make_response(
                &out2,
                SYSTEM_KIND,
                "TWO_RESP",
                json!({"ok": 2}),
            ))
            .await
            .unwrap();
        harness
            .inject(make_response(
                &out1,
                SYSTEM_KIND,
                "ONE_RESP",
                json!({"ok": 1}),
            ))
            .await
            .unwrap();
        let r1 = h1.await.unwrap().expect("rpc 1 ok");
        let r2 = h2.await.unwrap().expect("rpc 2 ok");
        assert_eq!(r1.payload, json!({"ok": 1}));
        assert_eq!(r2.payload, json!({"ok": 2}));
    }

    #[tokio::test]
    async fn unknown_trace_id_flows_to_operational_receiver() {
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();
        harness
            .inject(make_loose("free-trace", SYSTEM_KIND, "PING", json!({})))
            .await
            .unwrap();
        let got = time::timeout(Duration::from_secs(1), sys_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.meta.msg.as_deref(), Some("PING"));
    }

    #[tokio::test]
    async fn pre_pending_rule_wins_against_colliding_admin_waiter() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut admin_rx = client.take_command_receiver("admin").await.unwrap();
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@hive",
                action: "ping",
                target: None,
                params: json!({}),
                request_id: None,
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();

        harness
            .inject(make_loose(
                &outgoing.routing.trace_id,
                ADMIN_KIND,
                MSG_ADMIN_COMMAND,
                json!({"inbound": true}),
            ))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), admin_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.meta.msg.as_deref(), Some(MSG_ADMIN_COMMAND));

        harness
            .inject(make_response(
                &outgoing,
                ADMIN_KIND,
                MSG_ADMIN_COMMAND_RESPONSE,
                json!({"status": "ok", "action": "ping"}),
            ))
            .await
            .unwrap();
        let result = waiter.await.unwrap().expect("admin rpc ok");
        assert_eq!(result.status, "ok");
    }

    #[tokio::test]
    async fn send_with_matcher_supports_msg_type_wildcard_success() {
        let profile = OperationalRouteProfile::builder().build().unwrap();
        let (client, mut harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let matcher = PendingMatcher::new(
            vec![RouteMatch::any_msg_type("command_response")],
            vec![],
            vec![RouteMatch::any_msg_type("command")],
        );
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_with_matcher(
                make_outgoing("command", "DO_WORK"),
                matcher,
                RpcRequestLabels::new("SY.target@hive", "DO_WORK", "command_response"),
                Duration::from_secs(2),
            )
            .await
        });

        let outgoing = harness.next_outgoing().await.unwrap();
        assert!(!outgoing.routing.trace_id.is_empty());
        harness
            .inject(make_response(
                &outgoing,
                "command_response",
                "ANY_RESPONSE_NAME",
                json!({"ok": true}),
            ))
            .await
            .unwrap();

        let response = waiter.await.unwrap().expect("wildcard response ok");
        assert_eq!(response.meta.msg_type, "command_response");
        assert_eq!(response.meta.msg.as_deref(), Some("ANY_RESPONSE_NAME"));
    }

    #[tokio::test]
    async fn send_with_matcher_rejects_duplicate_active_trace_id() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let matcher = PendingMatcher::new(
            vec![RouteMatch::exact(SYSTEM_KIND, "RESP")],
            vec![],
            vec![RouteMatch::any_msg_type(SYSTEM_KIND)],
        );
        let mut first = make_outgoing(SYSTEM_KIND, "REQ");
        first.routing.trace_id = "shared-trace".to_string();
        let c = Arc::clone(&client);
        let first_waiter = tokio::spawn({
            let matcher = matcher.clone();
            async move {
                c.send_with_matcher(
                    first,
                    matcher,
                    RpcRequestLabels::new("SY.target@hive", "REQ", "RESP"),
                    Duration::from_secs(2),
                )
                .await
            }
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        assert_eq!(outgoing.routing.trace_id, "shared-trace");

        let mut second = make_outgoing(SYSTEM_KIND, "REQ2");
        second.routing.trace_id = "shared-trace".to_string();
        let err = client
            .send_with_matcher(
                second,
                matcher,
                RpcRequestLabels::new("SY.target@hive", "REQ2", "RESP"),
                Duration::from_secs(2),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, RpcError::InvalidRequest(ref message) if message.contains("duplicate active trace_id")),
            "got {err:?}"
        );

        harness
            .inject(make_response(
                &outgoing,
                SYSTEM_KIND,
                "RESP",
                json!({"ok": true}),
            ))
            .await
            .unwrap();
        let response = first_waiter.await.unwrap().expect("first rpc completes");
        assert_eq!(response.payload, json!({"ok": true}));
    }

    #[tokio::test]
    async fn colliding_trace_outside_invalid_family_keeps_waiter_pending() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut admin_rx = client.take_command_receiver("admin").await.unwrap();
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            // Admin RPC: invalid_response_families = {ADMIN_KIND}. A colliding
            // SYSTEM_KIND message must NOT fail the waiter.
            c.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@hive",
                action: "ping",
                target: None,
                params: json!({}),
                request_id: None,
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        // Colliding SYSTEM_KIND non-transport message — must route as operational.
        // But SYSTEM channel is not in profile; we use simple_profile which only
        // has admin+system commands. SYSTEM_UPDATE goes to "system" worker.
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();
        harness
            .inject(make_loose(
                &outgoing.routing.trace_id,
                SYSTEM_KIND,
                "SYSTEM_UPDATE",
                json!({}),
            ))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), sys_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.meta.msg.as_deref(), Some("SYSTEM_UPDATE"));
        // Now complete admin properly.
        harness
            .inject(make_response(
                &outgoing,
                ADMIN_KIND,
                MSG_ADMIN_COMMAND_RESPONSE,
                json!({"status": "ok", "action": "ping"}),
            ))
            .await
            .unwrap();
        let result = waiter.await.unwrap().expect("admin rpc ok");
        assert_eq!(result.status, "ok");
        // The admin command receiver should not have gotten the colliding
        // SYSTEM_KIND message.
        assert!(admin_rx.try_recv().is_none());
    }

    #[tokio::test]
    async fn colliding_trace_in_invalid_family_unknown_msg_fails_invalid_response() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            // System RPC: invalid_response_families = {SYSTEM_KIND}.
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        harness
            .inject(make_loose(
                &outgoing.routing.trace_id,
                SYSTEM_KIND,
                "WRONG_RESP",
                json!({}),
            ))
            .await
            .unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::InvalidResponse(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn admin_waiter_completes_on_system_kind_transport_error() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@hive",
                action: "ping",
                target: None,
                params: json!({}),
                request_id: None,
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        let unreachable = make_response(
            &outgoing,
            SYSTEM_KIND,
            MSG_UNREACHABLE,
            json!({"reason": "NODE_NOT_FOUND", "original_dst": "SY.admin@hive"}),
        );
        harness.inject(unreachable).await.unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        match err {
            RpcError::Unreachable {
                reason,
                original_dst,
            } => {
                assert_eq!(reason, "NODE_NOT_FOUND");
                assert_eq!(original_dst, "SY.admin@hive");
            }
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn admin_waiter_colliding_non_terminal_system_routes_operationally() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@hive",
                action: "ping",
                target: None,
                params: json!({}),
                request_id: None,
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        // Non-terminal SYSTEM_KIND with colliding trace; admin invalid_family
        // is {ADMIN_KIND} so this must route operationally, not complete.
        harness
            .inject(make_loose(
                &outgoing.routing.trace_id,
                SYSTEM_KIND,
                "SOMETHING",
                json!({}),
            ))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), sys_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.meta.msg.as_deref(), Some("SOMETHING"));
        // Waiter still pending.
        harness
            .inject(make_response(
                &outgoing,
                ADMIN_KIND,
                MSG_ADMIN_COMMAND_RESPONSE,
                json!({"status": "ok", "action": "ping"}),
            ))
            .await
            .unwrap();
        let result = waiter.await.unwrap().expect("admin rpc ok");
        assert_eq!(result.status, "ok");
    }

    // ---- Stale / response-only ----

    #[tokio::test]
    async fn late_correlated_response_after_timeout_is_stale_not_delivered() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_millis(80),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Timeout { .. }));
        // Now inject the late response.
        harness
            .inject(make_response(
                &outgoing,
                SYSTEM_KIND,
                "RESP",
                json!({"late": true}),
            ))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        // Stale counter incremented; system worker did NOT receive it.
        assert_eq!(client.metric_stale_responses(), 1);
        assert!(sys_rx.try_recv().is_none());
    }

    #[tokio::test]
    async fn response_only_orphan_dropped_with_metric() {
        // Build a profile whose broad rule WOULD catch a SYSTEM_KIND RESP if
        // it weren't filtered by the response-only registry first.
        let profile = OperationalRouteProfile::builder()
            .command_channel("system")
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                RouteTarget::Command("system"),
            )
            .build()
            .unwrap();
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();

        // Issue and time-out an RPC so its response shape lands in the
        // response-only registry. Wait long enough for the stale entry to
        // also expire would be slow; instead, exercise the registry by
        // injecting the orphan with a *different* trace_id so the stale
        // table check (keyed by trace_id) does not match.
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_millis(50),
            })
            .await
        });
        let _ = waiter.await;

        // Different trace_id, same response shape → orphan from registry.
        harness
            .inject(make_loose(
                "unrelated-trace",
                SYSTEM_KIND,
                "RESP",
                json!({}),
            ))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert_eq!(client.metric_unknown_responses(), 1);
        assert!(sys_rx.try_recv().is_none());
    }

    #[tokio::test]
    async fn response_only_wildcard_success_shape_dropped_before_post_rules() {
        let profile = OperationalRouteProfile::builder()
            .command_channel("catch_all")
            .post_pending_rule(RouteMatch::Any, RouteTarget::Command("catch_all"))
            .build()
            .unwrap();
        let (client, mut harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut cmd_rx = client.take_command_receiver("catch_all").await.unwrap();
        let matcher = PendingMatcher::new(
            vec![RouteMatch::any_msg_type("command_response")],
            vec![],
            vec![],
        );
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_with_matcher(
                make_outgoing("command", "DO_WORK"),
                matcher,
                RpcRequestLabels::new("SY.target@hive", "DO_WORK", "command_response"),
                Duration::from_millis(50),
            )
            .await
        });
        let _ = harness.next_outgoing().await.unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Timeout { .. }));

        harness
            .inject(make_loose(
                "unrelated-trace",
                "command_response",
                "ANY_RESPONSE_NAME",
                json!({}),
            ))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert_eq!(client.metric_unknown_responses(), 1);
        assert!(cmd_rx.try_recv().is_none());
    }

    #[tokio::test]
    async fn response_only_skips_profile_declared_observational_family() {
        let profile = OperationalRouteProfile::builder()
            .broadcast_channel("query")
            .post_pending_rule(
                RouteMatch::any_msg_type("query_response"),
                RouteTarget::Broadcast("query"),
            )
            .build()
            .unwrap();
        let (client, mut harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut sub = client.subscribe("query").unwrap();
        let matcher = PendingMatcher::new(
            vec![RouteMatch::any_msg_type("query_response")],
            vec![],
            vec![],
        );
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_with_matcher(
                make_outgoing("query", "DO_QUERY"),
                matcher,
                RpcRequestLabels::new("SY.target@hive", "DO_QUERY", "query_response"),
                Duration::from_millis(50),
            )
            .await
        });
        let _ = harness.next_outgoing().await.unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Timeout { .. }));

        harness
            .inject(make_loose(
                "unrelated-trace",
                "query_response",
                "ANY_RESPONSE_NAME",
                json!({}),
            ))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.meta.msg_type, "query_response");
        assert_eq!(client.metric_unknown_responses(), 0);
    }

    #[tokio::test]
    async fn response_only_skips_exact_success_declared_by_observational_family() {
        let profile = OperationalRouteProfile::builder()
            .broadcast_channel("query")
            .post_pending_rule(
                RouteMatch::any_msg_type("query_response"),
                RouteTarget::Broadcast("query"),
            )
            .build()
            .unwrap();
        let (client, mut harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut sub = client.subscribe("query").unwrap();
        let matcher = PendingMatcher::new(
            vec![RouteMatch::exact("query_response", "QUERY_DONE")],
            vec![],
            vec![],
        );
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_with_matcher(
                make_outgoing("query", "DO_QUERY"),
                matcher,
                RpcRequestLabels::new("SY.target@hive", "DO_QUERY", "QUERY_DONE"),
                Duration::from_millis(50),
            )
            .await
        });
        let _ = harness.next_outgoing().await.unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Timeout { .. }));

        harness
            .inject(make_loose(
                "unrelated-trace",
                "query_response",
                "QUERY_DONE",
                json!({}),
            ))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(routed.meta.msg_type, "query_response");
        assert_eq!(routed.meta.msg.as_deref(), Some("QUERY_DONE"));
        assert_eq!(client.metric_unknown_responses(), 0);
    }

    // ---- Lifecycle ----

    #[tokio::test]
    async fn timeout_cleans_waiter_and_subsequent_rpc_works() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_millis(50),
            })
            .await
        });
        let _ = harness.next_outgoing().await;
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Timeout { .. }));
        assert!(client.pending.lock().await.is_empty());
        let c2 = Arc::clone(&client);
        let waiter2 = tokio::spawn(async move {
            c2.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ2",
                response_msg: "RESP2",
                payload: json!({}),
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let out2 = harness.next_outgoing().await.unwrap();
        harness
            .inject(make_response(&out2, SYSTEM_KIND, "RESP2", json!({"k": 1})))
            .await
            .unwrap();
        let ok = waiter2.await.unwrap().expect("rpc 2");
        assert_eq!(ok.payload, json!({"k": 1}));
    }

    #[tokio::test]
    async fn drain_pending_waiters_completes_with_disconnected() {
        let (client, _harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(5),
            })
            .await
        });
        sleep(Duration::from_millis(20)).await;
        client.drain_pending_waiters().await;
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Disconnected));
    }

    #[tokio::test]
    async fn recv_loop_io_error_drains_pending_waiters() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(5),
            })
            .await
        });
        let _ = harness.next_outgoing().await.unwrap();
        harness
            .inject_error(NodeError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "socket dropped in test",
            )))
            .await
            .unwrap();

        let err = time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter should complete")
            .unwrap()
            .unwrap_err();
        assert!(matches!(err, RpcError::Disconnected));
        assert!(client.pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn ttl_exceeded_mapped_correctly() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        harness
            .inject(make_response(
                &outgoing,
                SYSTEM_KIND,
                MSG_TTL_EXCEEDED,
                json!({"original_dst": "SY.t@h", "last_hop": "router-a"}),
            ))
            .await
            .unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        match err {
            RpcError::TtlExceeded {
                original_dst,
                last_hop,
            } => {
                assert_eq!(original_dst, "SY.t@h");
                assert_eq!(last_hop, "router-a");
            }
            other => panic!("expected TtlExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn admin_response_parses_status_action_request_id_trace_id() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@hive",
                action: "delete_ilk",
                target: Some("motherbee"),
                params: json!({"ilk_id": "ilk:abc"}),
                request_id: Some("req-7"),
                timeout: Duration::from_secs(2),
            })
            .await
        });
        let outgoing = harness.next_outgoing().await.unwrap();
        assert_eq!(outgoing.meta.msg.as_deref(), Some(MSG_ADMIN_COMMAND));
        assert_eq!(outgoing.meta.action.as_deref(), Some("delete_ilk"));
        let payload = json!({
            "status": "ok",
            "action": "delete_ilk",
            "payload": {"deleted": true},
            "request_id": "req-7",
        });
        harness
            .inject(make_response(
                &outgoing,
                ADMIN_KIND,
                MSG_ADMIN_COMMAND_RESPONSE,
                payload,
            ))
            .await
            .unwrap();
        let result = waiter.await.unwrap().expect("admin rpc ok");
        assert_eq!(result.status, "ok");
        assert_eq!(result.action, "delete_ilk");
        assert_eq!(result.request_id.as_deref(), Some("req-7"));
        assert_eq!(result.payload, json!({"deleted": true}));
        assert!(!result.trace_id.is_empty());
    }

    #[tokio::test]
    async fn command_depth_gauge_consistent_with_recv() {
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        for i in 0..3 {
            harness
                .inject(make_loose(
                    &format!("t-{i}"),
                    SYSTEM_KIND,
                    "PING",
                    json!({}),
                ))
                .await
                .unwrap();
        }
        sleep(Duration::from_millis(40)).await;
        assert_eq!(client.command_depth("system"), Some(3));
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();
        let _ = sys_rx.recv().await;
        let _ = sys_rx.recv().await;
        let _ = sys_rx.recv().await;
        assert_eq!(client.command_depth("system"), Some(0));
    }

    fn admin_response_message_local(payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "src".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("dst".to_string()),
                ttl: 16,
                trace_id: "trace-test".to_string(),
            },
            meta: Meta {
                msg_type: ADMIN_KIND.to_string(),
                msg: Some(MSG_ADMIN_COMMAND_RESPONSE.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    #[test]
    fn parse_admin_response_prefers_payload_field_when_present() {
        let raw = json!({
            "status": "ok",
            "action": "get_hive",
            "payload": {"hive_id": "worker-220"},
            "error_code": null,
            "error_detail": null
        });
        let parsed = parse_admin_response(
            "get_hive",
            "trace-1".to_string(),
            admin_response_message_local(raw),
        )
        .unwrap();
        assert_eq!(parsed.action, "get_hive");
        assert_eq!(parsed.payload, json!({"hive_id": "worker-220"}));
    }

    #[test]
    fn parse_admin_response_falls_back_to_top_level_body_when_payload_missing() {
        let raw = json!({
            "status": "ok",
            "action": "get_status",
            "responses": [{"hive": "motherbee", "status": "ok", "payload": {"version": 10}}],
            "pending": [],
            "expected_hives_policy": ["motherbee"],
            "expected_hives_topology": ["motherbee"],
            "pending_hives_policy": [],
            "pending_hives_topology": [],
            "error_code": null,
            "error_detail": null
        });
        let parsed = parse_admin_response(
            "opa_get_status",
            "trace-2".to_string(),
            admin_response_message_local(raw),
        )
        .unwrap();
        assert_eq!(
            parsed.payload,
            json!({
                "responses": [{"hive": "motherbee", "status": "ok", "payload": {"version": 10}}],
                "pending": [],
                "expected_hives_policy": ["motherbee"],
                "expected_hives_topology": ["motherbee"],
                "pending_hives_policy": [],
                "pending_hives_topology": []
            })
        );
    }

    #[tokio::test]
    async fn take_receiver_returns_error_on_double_take() {
        let (client, _harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let _first = client.take_command_receiver("admin").await.unwrap();
        let second = client.take_command_receiver("admin").await;
        match second.unwrap_err() {
            RpcError::ReceiverAlreadyTaken { category } => {
                assert_eq!(category, "admin");
            }
            other => panic!("expected ReceiverAlreadyTaken, got {other:?}"),
        }
    }

    /// AF-P2b: an `AnyMsgOfType` post_pending rule routed to `Command`
    /// must NOT exempt success shapes from the response-only registry.
    /// Otherwise a late correlated response after stale TTL falls through
    /// to the worker as if it were a brand-new command.
    #[tokio::test]
    async fn post_pending_command_catch_all_does_not_exempt_response_only_registry() {
        let profile = OperationalRouteProfile::builder()
            .command_channel("worker")
            .post_pending_rule(
                RouteMatch::any_msg_type(SYSTEM_KIND),
                RouteTarget::Command("worker"),
            )
            .build()
            .unwrap();
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut worker_rx = client.take_command_receiver("worker").await.unwrap();

        // Fire an RPC and let it time out. After timeout the stale TTL
        // window opens, then expires; here we just immediately inject the
        // late response under a fresh trace_id — what matters is the
        // response shape matches what `send_system_rpc` registered.
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_millis(50),
            })
            .await
        });
        let _ = waiter.await;

        // Different trace_id, registered response shape. Without AF-P2b the
        // broad `AnyMsgOfType(SYSTEM_KIND) -> Command("worker")` would
        // swallow this; with AF-P2b the registry catches it first.
        harness
            .inject(make_loose("orphan-trace", SYSTEM_KIND, "RESP", json!({})))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert_eq!(client.metric_unknown_responses(), 1);
        assert!(worker_rx.try_recv().is_none());
    }

    /// AF-P2b mirror: a `Broadcast` post_pending rule (real observational
    /// stream) DOES exempt success shapes — the response fans out to
    /// subscribers, not the response-only drop.
    #[tokio::test]
    async fn post_pending_broadcast_rule_does_exempt_response_only_registry() {
        let profile = OperationalRouteProfile::builder()
            .broadcast_channel("config_response")
            .post_pending_rule(
                RouteMatch::exact(SYSTEM_KIND, "CONFIG_RESPONSE"),
                RouteTarget::Broadcast("config_response"),
            )
            .build()
            .unwrap();
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", profile);
        let mut subscriber = client.subscribe("config_response").unwrap();

        // Send and complete the RPC so the response shape is registered.
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.t@h",
                request_msg: "REQ",
                response_msg: "CONFIG_RESPONSE",
                payload: json!({}),
                timeout: Duration::from_millis(50),
            })
            .await
        });
        let _ = waiter.await;

        // Orphan CONFIG_RESPONSE arrives. With AF-P2b's observational
        // exemption (broadcast target), it must fan out to the subscriber,
        // NOT count as unknown_responses.
        harness
            .inject(make_loose(
                "orphan-trace",
                SYSTEM_KIND,
                "CONFIG_RESPONSE",
                json!({"v": 1}),
            ))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), subscriber.recv())
            .await
            .expect("subscriber timed out")
            .expect("subscriber recv");
        assert_eq!(routed.meta.msg.as_deref(), Some("CONFIG_RESPONSE"));
        assert_eq!(client.metric_unknown_responses(), 0);
    }

    /// AF-P2a: an RPC sent while the SDK marks the connection as down must
    /// fail fast with `Disconnected`, not register a waiter and time out.
    /// We trip the disconnect flag by closing the inbound channel inside
    /// the harness: the recv loop sees `Disconnected`, calls
    /// `drain_pending_waiters`, and flips `is_connected()` to false via the
    /// `NodeReceiver::recv()` Disconnected path.
    #[tokio::test]
    async fn send_with_matcher_fails_fast_when_sender_is_disconnected() {
        let (client, harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        // Force the SDK to observe a disconnect: dropping the harness's
        // inbound transmitter closes the receiver, the recv loop errors out
        // with `Disconnected`, and `NodeReceiver::recv` flips the shared
        // ConnectionState to disconnected.
        drop(harness);
        // Give the recv loop a tick to observe the closed channel.
        sleep(Duration::from_millis(50)).await;

        assert!(
            !client.sender_snapshot().is_connected(),
            "sender should report disconnected after recv loop drained"
        );
        let err = client
            .send_system_rpc(SystemRpcRequest {
                target: "SY.target@hive",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(10),
            })
            .await
            .expect_err("expected RpcError::Disconnected");
        assert!(matches!(err, RpcError::Disconnected), "got {err:?}");
        // Pending map must stay clean — no waiter was registered.
        assert!(client.pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn send_failure_does_not_register_response_only_shape() {
        let (outbound_tx, outbound_rx) = mpsc::channel(1);
        drop(outbound_rx);
        let (_inbound_tx, inbound_rx) = mpsc::channel(1);
        let state = Arc::new(ConnectionState::new_connected());
        let info = Arc::new(ConnectionInfo::new(
            "test-uuid".to_string(),
            "SY.test@hive".to_string(),
            7,
            "router-test".to_string(),
            state,
        ));
        let sender = NodeSender::new(outbound_tx, Arc::clone(&info));
        let receiver = NodeReceiver::new(inbound_rx, info);
        let client = RouterDispatcher::from_test_channels(sender, receiver, simple_profile());

        let err = client
            .send_system_rpc(SystemRpcRequest {
                target: "SY.target@hive",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(10),
            })
            .await
            .expect_err("expected send failure");

        assert!(matches!(err, RpcError::Disconnected), "got {err:?}");
        assert!(client.pending.lock().await.is_empty());
        assert!(client.response_only.lock().await.is_empty());
    }
}
