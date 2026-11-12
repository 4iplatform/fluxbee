//! Multiplexed RPC client over a router connection.
//!
//! `RouterDispatcher` owns a single `NodeSender + NodeReceiver` pair and serves two
//! roles: it correlates outgoing requests with incoming responses by
//! `trace_id` against caller-supplied response contracts, and it routes
//! operational traffic that is not a pending response according to a
//! profile-declared, ordered rule table over `(meta.msg_type, meta.msg)`.
//!
//! Design contract (see `docs/history/onworking-coa/orchestrator_internal_rpc_multiplexing_tasks.md`,
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
//! - A command channel is bounded: two lanes, its `system` messages and every other kind, each of
//!   `RPC_COMMAND_CHANNEL_CAPACITY` messages and `RPC_COMMAND_CHANNEL_MAX_BYTES`, the `system` lane
//!   read first. The recv loop hands a message to its lane without waiting. A message that finds
//!   its lane full is refused, and its sender told `NOT_TAKEN` (`node_busy`, retryable) when it may
//!   be answered, as the Go SDK does (FINDINGS A-295, A-329; since 0.1.143, planned; unbounded
//!   before).
//! - A `NOT_TAKEN` on a pending request's `trace_id` ends the request with `RpcError::NotTaken`,
//!   whatever its matcher.
//! - A call its caller drops removes its pending entry and notes its trace stale (`PendingGuard`,
//!   FINDINGS A-309).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration as StdDuration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::{self, Duration, Instant as TokioInstant};
use uuid::Uuid;

use crate::node_client::{connect, NodeConfig, NodeError};
use crate::policy::{classify_admin_action, classify_system_message, ActionResult};
use crate::protocol::{
    is_system_kind, Destination, Message, Meta, Routing, MSG_ANNOUNCE, MSG_HELLO, MSG_NOT_TAKEN,
    MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_WITHDRAW, NOT_TAKEN_REASON_NODE_BUSY, SYSTEM_KIND,
};
use crate::split::{ConnectionInfo, ConnectionState};
use crate::split::{NodeReceiver, NodeSender, Received};

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

/// The most messages each of a command channel's two lanes holds for its consumer (FINDINGS A-295;
/// since 0.1.143, planned). The Go SDK's lanes take as many since 0.1.141 (A-329).
///
/// - One lane takes the channel's `system` messages, which its consumer gets first
///   (`RpcCommandReceiver::recv`). The other takes every other kind.
/// - A message that finds its lane at this bound, or at `RPC_COMMAND_CHANNEL_MAX_BYTES`, is
///   refused: dropped, counted (`RouterDispatcher::command_refused`) and logged. Its sender, when it
///   may be answered, is told `NOT_TAKEN` (`node_busy`, retryable).
/// - Until 0.1.143 the channels had no bound, only a warning at a depth of 1,000. 1,024 keeps that
///   depth, and matches the router's per-link queue (`LINK_QUEUE_MAX_FRAMES`, A-222).
pub const RPC_COMMAND_CHANNEL_CAPACITY: usize = 1_024;

/// The most bytes each lane of a command channel holds, counted as its messages' frames came off
/// the socket (FINDINGS A-295): the router's bound for a node's link (`LINK_QUEUE_MAX_BYTES`), as
/// the Go SDK's lanes. A lane that holds nothing takes any one message (a frame is at most
/// 128 KiB).
pub const RPC_COMMAND_CHANNEL_MAX_BYTES: usize = 8 * 1024 * 1024;

/// At most one warning per command channel and interval about the messages it refused, as the Go
/// SDK's. The ones in between are counted in the next warning (`CommandChannel::note_refusal`).
const COMMAND_REFUSAL_LOG_INTERVAL: StdDuration = StdDuration::from_secs(10);

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
    /// The destination answered `NOT_TAKEN` on the request's `trace_id`: it took the request
    /// nowhere, and nothing of it ran (FINDINGS A-295).
    /// - `node_busy` comes from a node whose command channel had no room (the Rust SDK's since
    ///   0.1.143, the Go SDK's since 0.1.141), and is `retryable`: asking again later may pass.
    /// - A WF's own reasons say whether they are retryable (`unavailable` is, since 0.1.141). One
    ///   without `retryable`, from a node before 0.1.141, reads as `false`.
    #[error("rpc not taken: NOT_TAKEN reason={reason} retryable={retryable}: {detail}")]
    NotTaken {
        reason: String,
        detail: String,
        retryable: bool,
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
    /// Its call's id. It tells this call's entry from a later call's under the same `trace_id`
    /// (`PendingGuard`).
    id: u64,
    matcher: PendingMatcher,
    response_tx: oneshot::Sender<Result<Message, RpcError>>,
}

/// A request's hold on its pending entry while its call runs (FINDINGS A-309; since 0.1.143,
/// planned).
///
/// - A caller can drop a call before it ends: an outer `timeout`, or the losing branch of a
///   `select!`. Dropping the call drops its guard, which removes the entry, so the `trace_id` is
///   free again.
/// - The guard notes the trace stale, so a late answer is dropped and counted rather than handed
///   to the node as a command.
/// - Until 0.1.143 such a call left its entry for the life of the process.
/// - A guard removes only its own call's entry (`id`), never a later call's under the same
///   `trace_id`.
struct PendingGuard<'a> {
    dispatcher: &'a RouterDispatcher,
    trace_id: String,
    id: u64,
    /// Whether dropping the guard removes the entry: until the call ends on a path of its own.
    armed: bool,
}

impl PendingGuard<'_> {
    /// The call ended with its answer, or with the dispatcher's (`drain_pending_waiters`). Both
    /// remove the entry first.
    fn settled(mut self) {
        self.armed = false;
    }

    /// The call's message did not go out: its entry goes, and no answer can come to note stale.
    fn withdraw(mut self) {
        self.armed = false;
        self.remove(false);
    }

    /// The call timed out: its entry goes, and its trace is noted stale.
    fn expire(mut self) {
        self.armed = false;
        self.remove(true);
    }

    /// Removes this call's entry and, with `note_stale`, notes its trace stale under the same
    /// hold of the pending table. So the recv loop always finds the trace in one table or the
    /// other, and never routes a late answer to a command channel. It never removes a later
    /// call's entry. Returns whether it removed one.
    fn remove(&self, note_stale: bool) -> bool {
        let mut pending = lock(&self.dispatcher.pending);
        if pending
            .get(&self.trace_id)
            .is_none_or(|entry| entry.id != self.id)
        {
            return false;
        }
        let Some(entry) = pending.remove(&self.trace_id) else {
            return false;
        };
        if note_stale {
            lock(&self.dispatcher.stale).insert(self.trace_id.clone(), entry.matcher);
        }
        true
    }
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed || !self.remove(true) {
            return;
        }
        self.dispatcher
            .metric_abandoned_requests
            .fetch_add(1, Ordering::Relaxed);
        tracing::debug!(
            trace_id = %self.trace_id,
            "rpc request dropped by its caller before it ended; its pending entry removed and its \
             trace noted stale"
        );
    }
}

/// Locks one of the dispatcher's tables. No holder waits on anything, and no panic leaves a table
/// half written, so a lock that a panic poisoned is taken as it is.
fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
        // A NOT_TAKEN ends any request on its trace, whatever its matcher (A-295); a late one is
        // stale the same way.
        if is_not_taken(msg) {
            return StaleClassification::Stale;
        }
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

/// A command channel's lanes (FINDINGS A-295): its `system` messages, which its consumer gets first,
/// and every other kind. Each holds at most `RPC_COMMAND_CHANNEL_CAPACITY` messages and
/// `RPC_COMMAND_CHANNEL_MAX_BYTES` of their frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    System,
    Other,
}

impl Lane {
    fn of(msg: &Message) -> Self {
        if is_system_kind(&msg.meta.msg_type) {
            Lane::System
        } else {
            Lane::Other
        }
    }

    /// Its name in the logs and in `NOT_TAKEN`'s detail, as the Go SDK names its lanes.
    fn as_str(self) -> &'static str {
        match self {
            Lane::System => "system",
            Lane::Other => "non-system",
        }
    }
}

/// The sending side of a lane: its queue, and the bytes its messages' frames hold, which its
/// receiver gives back as it takes each.
struct LaneSender {
    queue: mpsc::Sender<Received>,
    bytes: Arc<AtomicUsize>,
}

impl LaneSender {
    /// The messages the lane holds: its bound less the room it has.
    fn depth(&self) -> usize {
        self.queue.max_capacity() - self.queue.capacity()
    }

    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct LaneReceiver {
    queue: mpsc::Receiver<Received>,
    bytes: Arc<AtomicUsize>,
}

impl LaneReceiver {
    /// Takes `received` off the lane's bytes.
    fn taken(&self, (msg, frame_len): Received) -> Message {
        self.bytes.fetch_sub(frame_len, Ordering::AcqRel);
        msg
    }
}

/// A new lane, empty.
fn new_lane() -> (LaneSender, LaneReceiver) {
    let (tx, rx) = mpsc::channel(RPC_COMMAND_CHANNEL_CAPACITY);
    let bytes = Arc::new(AtomicUsize::new(0));
    (
        LaneSender {
            queue: tx,
            bytes: Arc::clone(&bytes),
        },
        LaneReceiver { queue: rx, bytes },
    )
}

/// What a command channel did with a message (`CommandChannel::enqueue`).
enum Enqueued {
    /// In its lane, for the consumer.
    Taken,
    /// Its lane held as many messages or bytes as it takes: refused, and handed back.
    Refused(Message),
    /// The consumer dropped its receiver: nothing reads the channel any more.
    NoConsumer,
}

/// What became of the `NOT_TAKEN` of a message a command channel refused
/// (`RouterDispatcher::refuse`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// Queued to its sender.
    Told,
    /// Not queued: the link's outgoing queue was full, or the link down.
    Unsent,
    /// The message gets none (`may_answer_not_taken`).
    Never,
}

/// The refusals of a command channel since its last warning (`CommandChannel::note_refusal`).
#[derive(Default)]
struct RefusalLog {
    /// When the last warning was written; `None` before the first.
    line_at: Option<TokioInstant>,
    /// Whether a warning is scheduled for the end of the interval (`flush_refusals`).
    flush_due: bool,
    /// Messages refused since the last warning.
    refused: u64,
    /// Of them, those whose sender was told `NOT_TAKEN`.
    told: u64,
    /// Those whose `NOT_TAKEN` could not be queued.
    unsent: u64,
    /// Those that get none: notices, replies, broadcasts, tap copies.
    unanswered: u64,
    /// The last of them, as the warning names it.
    last: RefusedMessage,
}

impl RefusalLog {
    /// The warning for what the log holds; the log starts its next interval at `now`.
    fn take_line(&mut self, now: TokioInstant) -> RefusalLog {
        std::mem::replace(
            self,
            RefusalLog {
                line_at: Some(now),
                flush_due: self.flush_due,
                ..RefusalLog::default()
            },
        )
    }

    /// Writes the warning of `channel` for these refusals.
    fn warn(self, channel: &'static str, refused_total: u64) {
        let last = self.last;
        tracing::warn!(
            channel,
            refused = self.refused,
            told_not_taken = self.told,
            unsent = self.unsent,
            unanswered = self.unanswered,
            refused_total,
            lane = last.lane,
            held = last.held,
            held_bytes = last.held_bytes,
            capacity = RPC_COMMAND_CHANNEL_CAPACITY,
            max_bytes = RPC_COMMAND_CHANNEL_MAX_BYTES,
            msg_type = %last.msg_type,
            msg = %last.msg,
            from = %last.from,
            trace_id = %last.trace_id,
            "rpc command channel full: messages refused, never handed to the node; each sender \
             that may be answered was told NOT_TAKEN (node_busy, retryable); the last refused named"
        );
    }
}

/// What a warning names of the last message a command channel refused, and what its lane held.
#[derive(Default)]
struct RefusedMessage {
    lane: &'static str,
    held: usize,
    held_bytes: usize,
    msg_type: String,
    msg: String,
    from: String,
    trace_id: String,
}

struct CommandChannel {
    name: &'static str,
    system: LaneSender,
    other: LaneSender,
    /// Its consumer's side, until `take_command_receiver` takes it.
    receiver: Mutex<Option<RpcCommandReceiver>>,
    refused_total: Arc<AtomicU64>,
    refusals: Arc<StdMutex<RefusalLog>>,
}

impl CommandChannel {
    fn new(name: &'static str) -> Self {
        let (system, system_rx) = new_lane();
        let (other, other_rx) = new_lane();
        Self {
            name,
            system,
            other,
            receiver: Mutex::new(Some(RpcCommandReceiver {
                system: system_rx,
                other: other_rx,
            })),
            refused_total: Arc::new(AtomicU64::new(0)),
            refusals: Arc::new(StdMutex::new(RefusalLog::default())),
        }
    }

    fn lane(&self, lane: Lane) -> &LaneSender {
        match lane {
            Lane::System => &self.system,
            Lane::Other => &self.other,
        }
    }

    /// Hands `msg`, whose frame was `frame_len` bytes, to its lane without waiting: the recv loop
    /// never waits for a consumer. The lane takes its bytes before the message goes, so its
    /// consumer never gives back bytes the lane did not take. A lane that holds nothing takes any
    /// one message: a frame is at most 128 KiB.
    fn enqueue(&self, msg: Message, frame_len: usize) -> Enqueued {
        let lane = self.lane(Lane::of(&msg));
        let held = lane.bytes.fetch_add(frame_len, Ordering::AcqRel);
        if held > 0 && held + frame_len > RPC_COMMAND_CHANNEL_MAX_BYTES {
            lane.bytes.fetch_sub(frame_len, Ordering::AcqRel);
            return Enqueued::Refused(msg);
        }
        match lane.queue.try_send((msg, frame_len)) {
            Ok(()) => Enqueued::Taken,
            Err(TrySendError::Full((msg, _))) => {
                lane.bytes.fetch_sub(frame_len, Ordering::AcqRel);
                Enqueued::Refused(msg)
            }
            Err(TrySendError::Closed(_)) => {
                lane.bytes.fetch_sub(frame_len, Ordering::AcqRel);
                Enqueued::NoConsumer
            }
        }
    }

    async fn take_receiver(&self) -> Result<RpcCommandReceiver, RpcError> {
        self.receiver
            .lock()
            .await
            .take()
            .ok_or_else(|| RpcError::ReceiverAlreadyTaken {
                category: self.name.to_string(),
            })
    }

    /// The messages both lanes hold.
    fn depth(&self) -> usize {
        self.system.depth() + self.other.depth()
    }

    /// The messages refused for want of room since the process started.
    fn refused(&self) -> u64 {
        self.refused_total.load(Ordering::Relaxed)
    }

    /// Counts `msg`, a message its lane refused, and logs it. The first refusal gets a warning at
    /// once. After that, at most one warning every 10 s names the refusals since the last: at the
    /// next refusal once the interval has passed, or, when none comes, at the end of the interval
    /// (`flush_refusals`). Each refusal in between gets a debug line. This is the Go SDK's rule.
    fn note_refusal(&self, msg: &Message, answer: Answer) {
        self.refused_total.fetch_add(1, Ordering::Relaxed);
        let lane = Lane::of(msg);
        let sender = self.lane(lane);
        let mut log = lock(&self.refusals);
        log.refused += 1;
        match answer {
            Answer::Told => log.told += 1,
            Answer::Unsent => log.unsent += 1,
            Answer::Never => log.unanswered += 1,
        }
        log.last = RefusedMessage {
            lane: lane.as_str(),
            held: sender.depth(),
            held_bytes: sender.bytes(),
            msg_type: msg.meta.msg_type.clone(),
            msg: msg.meta.msg.clone().unwrap_or_default(),
            from: msg.routing.src_l2_name.clone().unwrap_or_default(),
            trace_id: msg.routing.trace_id.clone(),
        };
        let now = TokioInstant::now();
        let wait = log.line_at.map_or(Duration::ZERO, |at| {
            (at + COMMAND_REFUSAL_LOG_INTERVAL).saturating_duration_since(now)
        });
        if wait.is_zero() {
            let line = log.take_line(now);
            drop(log);
            line.warn(self.name, self.refused());
            return;
        }
        if !log.flush_due {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                log.flush_due = true;
                let (name, refusals, refused_total) = (
                    self.name,
                    Arc::clone(&self.refusals),
                    Arc::clone(&self.refused_total),
                );
                runtime.spawn(async move {
                    time::sleep(wait).await;
                    flush_refusals(name, &refusals, &refused_total);
                });
            }
        }
        drop(log);
        tracing::debug!(
            channel = self.name,
            lane = lane.as_str(),
            msg_type = %msg.meta.msg_type,
            msg = msg.meta.msg.as_deref().unwrap_or_default(),
            from = msg.routing.src_l2_name.as_deref().unwrap_or_default(),
            trace_id = %msg.routing.trace_id,
            answer = ?answer,
            "rpc command channel full: message refused (counted in the next warning)"
        );
    }
}

/// The warning due at the end of a command channel's interval, for the refusals no warning named
/// yet: a burst refused within 10 s of a warning gets its count even when nothing follows it.
fn flush_refusals(
    channel: &'static str,
    refusals: &StdMutex<RefusalLog>,
    refused_total: &AtomicU64,
) {
    let mut log = lock(refusals);
    log.flush_due = false;
    if log.refused == 0 {
        return;
    }
    let line = log.take_line(TokioInstant::now());
    drop(log);
    line.warn(channel, refused_total.load(Ordering::Relaxed));
}

/// The consumer's side of a command channel (`RouterDispatcher::take_command_receiver`): its two
/// lanes, the `system` one read first (FINDINGS A-295).
#[derive(Debug)]
pub struct RpcCommandReceiver {
    system: LaneReceiver,
    other: LaneReceiver,
}

impl RpcCommandReceiver {
    /// The next message: a `system` one while any waits, so they never wait behind other kinds;
    /// each lane in its order. `None` once the dispatcher is gone. Cancel safe: a `recv` dropped
    /// before it returns takes nothing.
    pub async fn recv(&mut self) -> Option<Message> {
        let (lane, received) = tokio::select! {
            biased;
            Some(received) = self.system.queue.recv() => (Lane::System, received),
            Some(received) = self.other.queue.recv() => (Lane::Other, received),
            else => return None,
        };
        Some(self.lane(lane).taken(received))
    }

    /// The next message, if one waits now, a `system` one first.
    pub fn try_recv(&mut self) -> Option<Message> {
        if let Ok(received) = self.system.queue.try_recv() {
            return Some(self.system.taken(received));
        }
        let received = self.other.queue.try_recv().ok()?;
        Some(self.other.taken(received))
    }

    fn lane(&self, lane: Lane) -> &LaneReceiver {
        match lane {
            Lane::System => &self.system,
            Lane::Other => &self.other,
        }
    }
}

/// Whether `msg` is a node's `NOT_TAKEN`. It ends any request on its `trace_id`, whatever the
/// request's matcher (FINDINGS A-295).
fn is_not_taken(msg: &Message) -> bool {
    is_system_kind(&msg.meta.msg_type) && msg.meta.msg.as_deref() == Some(MSG_NOT_TAKEN)
}

/// Whether the sender of `msg`, a message refused for want of room, is told `NOT_TAKEN` (FINDINGS
/// A-295). Any message a node sent is answered, named or not, from any node, except:
/// - a notice: one with no router-stamped `src_l2_name`, a presence or router notice by name, or a
///   `system` message without a name. The router made a message without `src_l2_name`, as it makes
///   UNREACHABLE and TTL_EXCEEDED. Its `routing.src` is the router's UUID, which no node has, so an
///   answer would bounce (FINDINGS A-112). The rule is wf-generic's `isNotice`, the AI SDK's
///   `is_infrastructure_message` and SY.timer's `isInfrastructureMessage`;
/// - a reply: `*_RESPONSE`, `NOT_TAKEN` or `<msg>.error` (wf-generic's `isReply`), or a kind ending
///   in `_response`. So two nodes cannot bounce a `NOT_TAKEN` (FINDINGS A-119);
/// - a broadcast, which names no one to answer;
/// - a tap copy, which its sender sent to another node (FINDINGS A-209).
fn may_answer_not_taken(msg: &Message) -> bool {
    let from_a_node = msg
        .routing
        .src_l2_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty());
    if !from_a_node || msg.routing.src.trim().is_empty() {
        return false;
    }
    if matches!(msg.routing.dst, Destination::Broadcast) || msg.meta.via_tap {
        return false;
    }
    if msg
        .meta
        .msg_type
        .trim()
        .to_ascii_lowercase()
        .ends_with("_response")
    {
        return false;
    }
    match msg.meta.msg.as_deref().map(str::trim).unwrap_or_default() {
        "" => !is_system_kind(&msg.meta.msg_type),
        MSG_HELLO | MSG_ANNOUNCE | MSG_WITHDRAW | MSG_UNREACHABLE | MSG_TTL_EXCEEDED
        | MSG_NOT_TAKEN => false,
        name => !name.ends_with("_RESPONSE") && !name.ends_with(".error"),
    }
}

/// The `NOT_TAKEN` that tells the sender of `refused` that its message found no room (FINDINGS
/// A-295). It is a `system` message to its `routing.src`, on its `trace_id` and `meta.thread_id`,
/// `ttl` 16, with reason `node_busy` and `retryable: true`: the Go SDK's `BuildNotTaken` (A-329)
/// and the WF engine's `answerNotTaken`.
fn not_taken_node_busy(own_uuid: &str, refused: &Message, detail: String) -> Message {
    Message {
        routing: Routing {
            src: own_uuid.to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(refused.routing.src.clone()),
            ttl: 16,
            trace_id: refused.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_NOT_TAKEN.to_string()),
            thread_id: refused.meta.thread_id.clone(),
            ..Meta::default()
        },
        payload: json!({
            "ok": false,
            "error": "not_taken",
            "reason": NOT_TAKEN_REASON_NODE_BUSY,
            "retryable": true,
            "detail": detail,
        }),
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
    /// The requests waiting for their answer, by `trace_id`. Locked with `lock`. When a holder
    /// takes `stale` too, it takes `pending` first.
    pending: StdMutex<HashMap<String, PendingEntry>>,
    stale: StdMutex<RecentStaleTable>,
    response_only: Mutex<HashSet<RouteMatch>>,
    /// The next call's id (`PendingEntry::id`).
    next_request_id: AtomicU64,
    metric_stale_responses: AtomicU64,
    metric_unknown_responses: AtomicU64,
    metric_route_unmatched: AtomicU64,
    metric_abandoned_requests: AtomicU64,
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
            pending: StdMutex::new(HashMap::new()),
            stale: StdMutex::new(RecentStaleTable::new()),
            response_only: Mutex::new(HashSet::new()),
            next_request_id: AtomicU64::new(0),
            metric_stale_responses: AtomicU64::new(0),
            metric_unknown_responses: AtomicU64::new(0),
            metric_route_unmatched: AtomicU64::new(0),
            metric_abandoned_requests: AtomicU64::new(0),
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
        let mut pending = lock(&self.pending);
        let mut stale = lock(&self.stale);
        for (trace_id, entry) in pending.drain() {
            stale.insert(trace_id, entry.matcher);
            let _ = entry.response_tx.send(Err(RpcError::Disconnected));
        }
    }

    /// The messages a command channel holds, both lanes (`RPC_COMMAND_CHANNEL_CAPACITY` each).
    /// Returns `None` for names not in the profile.
    pub fn command_depth(&self, name: &str) -> Option<usize> {
        self.command.get(name).map(|c| c.depth())
    }

    /// The messages a command channel refused for want of room since the process started
    /// (FINDINGS A-295). Returns `None` for names not in the profile.
    pub fn command_refused(&self, name: &str) -> Option<u64> {
        self.command.get(name).map(CommandChannel::refused)
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

    /// Requests whose caller dropped the call before it ended, and whose pending entry the SDK
    /// removed then (FINDINGS A-309).
    pub fn metric_abandoned_requests(&self) -> u64 {
        self.metric_abandoned_requests.load(Ordering::Relaxed)
    }

    async fn recv_loop(self: Arc<Self>, mut receiver: NodeReceiver) {
        loop {
            match receiver.recv_sized().await {
                Ok((msg, frame_len)) => self.dispatch(msg, frame_len).await,
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

    /// Routes `msg`, whose frame was `frame_len` bytes: the command channels count their bytes by
    /// it.
    async fn dispatch(&self, msg: Message, frame_len: usize) {
        let trace_id = msg.routing.trace_id.clone();
        let action = self.classify(msg, &trace_id).await;
        match action {
            DispatchAction::CompleteSuccess(entry, msg) => {
                self.note_stale(trace_id, entry.matcher.clone());
                let _ = entry.response_tx.send(Ok(msg));
            }
            DispatchAction::CompleteTransportError(entry, msg) => {
                let err = transport_error_from_message(&msg);
                self.note_stale(trace_id, entry.matcher.clone());
                let _ = entry.response_tx.send(Err(err));
            }
            DispatchAction::CompleteInvalidResponse(entry, msg) => {
                let err = RpcError::InvalidResponse(format!(
                    "unexpected response shape msg_type={} msg={:?}",
                    msg.meta.msg_type, msg.meta.msg
                ));
                self.note_stale(trace_id, entry.matcher.clone());
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
            DispatchAction::RouteByProfile(msg, target) => {
                self.route_target(msg, frame_len, target)
            }
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

        // 2. Active pending waiter for this trace_id. A node's NOT_TAKEN on its trace ends it
        // whatever its matcher: the request was taken nowhere (A-295).
        {
            let mut pending = lock(&self.pending);
            if let Some(entry_ref) = pending.get(trace_id) {
                let outcome = if is_not_taken(&msg) {
                    MatchOutcome::TerminalTransportError
                } else {
                    entry_ref
                        .matcher
                        .classify(msg.meta.msg_type.as_str(), msg.meta.msg.as_deref())
                };
                match outcome {
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
        if let StaleClassification::Stale = lock(&self.stale).classify(&msg) {
            return DispatchAction::Stale(msg);
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

    fn note_stale(&self, trace_id: String, matcher: PendingMatcher) {
        lock(&self.stale).insert(trace_id, matcher);
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

    fn route_target(&self, msg: Message, frame_len: usize, target: RouteTarget) {
        let msg_type = msg.meta.msg_type.clone();
        let msg_name = msg.meta.msg.clone();
        match target {
            RouteTarget::Command(name) => {
                let Some(channel) = self.command.get(name) else {
                    tracing::error!(
                        channel = %name,
                        "rpc dispatcher: command channel missing despite passing profile validation"
                    );
                    return;
                };
                match channel.enqueue(msg, frame_len) {
                    Enqueued::Taken => {}
                    Enqueued::Refused(msg) => self.refuse(channel, msg),
                    Enqueued::NoConsumer => tracing::debug!(
                        category = channel.name,
                        msg_type = %msg_type,
                        msg = ?msg_name,
                        "rpc command channel send failed; no consumer attached"
                    ),
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

    /// Handles `msg`, a message its command channel had no room for (FINDINGS A-295): it never
    /// reaches the node. Its sender, when it may be answered (`may_answer_not_taken`), is told
    /// `NOT_TAKEN` (`node_busy`, retryable). The answer is queued without waiting, since the recv loop
    /// never waits: an answer the link cannot take now is dropped and counted.
    fn refuse(&self, channel: &CommandChannel, msg: Message) {
        let answer = if !may_answer_not_taken(&msg) {
            Answer::Never
        } else {
            let lane = Lane::of(&msg);
            // The Go SDK's words (A-329), so a sender reads the same from either.
            let detail = format!(
                "{} holds {} {} messages, as many as it takes; send it again later",
                self.sender.full_name(),
                channel.lane(lane).depth(),
                lane.as_str()
            );
            if self
                .sender
                .try_send_now(not_taken_node_busy(self.sender.uuid(), &msg, detail))
            {
                Answer::Told
            } else {
                Answer::Unsent
            }
        };
        channel.note_refusal(&msg, answer);
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
                RouteMatch::AnyMsgOfType(_) => {
                    // NEVER registered — same reason as the `Any` skip below, scoped to a
                    // family: registering an entire msg_type family as "never a command"
                    // permanently poisons any node whose OWN inbound commands live in that
                    // family. io.cloud awaits the frontdesk verdict with
                    // `any_msg_type(USER_KIND)` (the verdict is a `user` frame with NO msg
                    // name, so no narrower matcher exists) while ALL its Cloud requests also
                    // arrive as `user` frames through its `Any -> Command` catch-all: one
                    // register_human then silently dropped EVERY subsequent edge request
                    // until restart (prod outage 2026-08-26; io.api's handoff path carried
                    // the identical bomb). Late family-wide replies stay covered by the
                    // trace-keyed pending map and the recent-stale table; only EXACT response
                    // shapes — which cannot collide with a whole family of commands — earn a
                    // permanent registry entry.
                    tracing::debug!(
                        "rpc response-only registry skipped AnyMsgOfType success matcher to avoid family-wide drops"
                    );
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
        // From here until the call ends on a path of its own, a caller that drops the call drops
        // `waiter`, which removes the entry (A-309).
        let waiter = self.register_waiter(&trace_id, &matcher, tx)?;
        if let Err(err) = self.sender.send(outgoing).await {
            waiter.withdraw();
            // AF-P2a: normalize disconnect to a single `RpcError` variant so
            // callers can `match` without unwrapping `RpcError::Node(...)`.
            return Err(match err {
                NodeError::Disconnected => RpcError::Disconnected,
                other => other.into(),
            });
        }
        self.register_response_only(&matcher).await;

        match time::timeout(timeout, rx).await {
            Ok(Ok(result)) => {
                waiter.settled();
                result
            }
            Ok(Err(_)) => {
                waiter.settled();
                Err(RpcError::ResponseChannelClosed { trace_id })
            }
            Err(_) => {
                waiter.expire();
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

    /// Records a request's waiter under `trace_id`, and the guard that removes it if the call is
    /// dropped (`PendingGuard`). Refused while another request holds that `trace_id`.
    fn register_waiter(
        &self,
        trace_id: &str,
        matcher: &PendingMatcher,
        response_tx: oneshot::Sender<Result<Message, RpcError>>,
    ) -> Result<PendingGuard<'_>, RpcError> {
        let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let mut pending = lock(&self.pending);
        if pending.contains_key(trace_id) {
            return Err(RpcError::InvalidRequest(format!(
                "duplicate active trace_id {trace_id}"
            )));
        }
        pending.insert(
            trace_id.to_string(),
            PendingEntry {
                id,
                matcher: matcher.clone(),
                response_tx,
            },
        );
        Ok(PendingGuard {
            dispatcher: self,
            trace_id: trace_id.to_string(),
            id,
            armed: true,
        })
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
        // Read field by field: a field of another type loses only itself.
        Some(MSG_NOT_TAKEN) => {
            let text = |field: &str| {
                msg.payload
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            RpcError::NotTaken {
                reason: text("reason"),
                detail: text("detail"),
                retryable: msg
                    .payload
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
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
    inbound_tx: mpsc::Sender<Result<Received, NodeError>>,
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

    /// Hands `message` to the dispatcher as the socket would, with its frame's length.
    pub async fn inject(&self, message: Message) -> Result<(), NodeError> {
        let frame_len = serde_json::to_vec(&message).map_or(0, |frame| frame.len());
        self.inbound_tx
            .send(Ok((message, frame_len)))
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

    /// Regression for the io.cloud edge-poisoning outage (2026-08-26): a
    /// family-wide (`AnyMsgOfType`) success matcher must NEVER land in the
    /// response-only registry. io.cloud awaits the frontdesk verdict with
    /// `any_msg_type("user")` while ALL its Cloud requests also arrive as
    /// `user` frames through its `Any -> Command` catch-all — registering the
    /// family silently dropped every subsequent edge request until restart.
    /// After the fix, a fresh-trace `user` frame injected AFTER such an RPC
    /// still reaches the command channel.
    #[tokio::test]
    async fn family_wide_success_matcher_never_poisons_command_traffic() {
        // io.cloud's exact profile shape: one catch-all command channel.
        let profile = OperationalRouteProfile::builder()
            .command_channel("incoming")
            .post_pending_rule(RouteMatch::Any, RouteTarget::Command("incoming"))
            .build()
            .unwrap();
        let (client, mut harness) = RouterDispatcherTestHarness::new("IO.cloud@hive", profile);
        let mut cmd_rx = client.take_command_receiver("incoming").await.unwrap();
        // io.cloud's frontdesk_reply_matcher shape: success = whole user family.
        let matcher = PendingMatcher::new(vec![RouteMatch::any_msg_type("user")], vec![], vec![]);
        let c = Arc::clone(&client);
        let waiter = tokio::spawn(async move {
            c.send_with_matcher(
                make_outgoing("user", "FRONTDESK_HANDOFF"),
                matcher,
                RpcRequestLabels::new(
                    "SY.frontdesk.gov@hive",
                    "FRONTDESK_HANDOFF",
                    "FRONTDESK_REPLY",
                ),
                Duration::from_millis(50),
            )
            .await
        });
        let _ = harness.next_outgoing().await.unwrap();
        let err = waiter.await.unwrap().unwrap_err();
        assert!(matches!(err, RpcError::Timeout { .. }));

        // A brand-new edge-forwarded Cloud request: user family, fresh trace.
        harness
            .inject(make_loose("fresh-edge-trace", "user", "user", json!({})))
            .await
            .unwrap();
        let routed = time::timeout(Duration::from_secs(1), cmd_rx.recv())
            .await
            .expect("edge request must not be silently dropped")
            .expect("command channel open");
        assert_eq!(routed.routing.trace_id, "fresh-edge-trace");
        assert_eq!(client.metric_unknown_responses(), 0);
    }

    /// NOTE post-2026-08-26: family (`AnyMsgOfType`) success shapes are now NEVER
    /// registered response-only (see the AnyMsgOfType arm in `register_response_only`
    /// — the io.cloud edge-poisoning fix), so this passes regardless of the Broadcast
    /// rule. Kept as a behavioral regression test: an observational family reply must
    /// keep fanning out to its subscribers. Re-introducing conditional family
    /// registration is guarded by `family_wide_success_matcher_never_poisons_command_traffic`.
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
        assert!(lock(&client.pending).is_empty());
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
        assert!(lock(&client.pending).is_empty());
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
        assert!(lock(&client.pending).is_empty());
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
        assert!(lock(&client.pending).is_empty());
        assert!(client.response_only.lock().await.is_empty());
    }

    // ---- A bounded command channel (FINDINGS A-295) ----

    /// A message the node `sender` sent this one, on thread `thread-1`: its router stamped its name.
    fn from_node(sender: &str, trace_id: &str, msg_type: &str, msg: Option<&str>) -> Message {
        Message {
            routing: Routing {
                src: format!("{sender}-uuid"),
                src_l2_name: Some(sender.to_string()),
                dst: Destination::Unicast("test-uuid".to_string()),
                ttl: 16,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: msg.map(str::to_string),
                thread_id: Some("thread-1".to_string()),
                ..Meta::default()
            },
            payload: json!({"text": "hi"}),
        }
    }

    /// One command channel, "incoming", that takes every message, as an IO node's profile does.
    fn incoming_profile() -> OperationalRouteProfile {
        OperationalRouteProfile::builder()
            .command_channel("incoming")
            .post_pending_rule(RouteMatch::Any, RouteTarget::Command("incoming"))
            .build()
            .expect("incoming profile")
    }

    async fn wait_for_depth(client: &RouterDispatcher, depth: usize) {
        time::timeout(Duration::from_secs(5), async {
            while client.command_depth("incoming") != Some(depth) {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the channel holds the messages");
    }

    /// Fills the lane of `msg_type` of "incoming", which nobody reads yet, with
    /// `RPC_COMMAND_CHANNEL_CAPACITY` messages from `from`.
    async fn fill_lane(
        client: &RouterDispatcher,
        harness: &RouterDispatcherTestHarness,
        msg_type: &str,
        from: &str,
        msg: Option<&str>,
    ) {
        let before = client.command_depth("incoming").unwrap();
        for i in 0..RPC_COMMAND_CHANNEL_CAPACITY {
            harness
                .inject(from_node(
                    from,
                    &format!("fill-{msg_type}-{i}"),
                    msg_type,
                    msg,
                ))
                .await
                .unwrap();
        }
        wait_for_depth(client, before + RPC_COMMAND_CHANNEL_CAPACITY).await;
    }

    /// Checks that `answer` is the SDK's NOT_TAKEN for the message `trace_id` of `sender`, refused
    /// by `IO.test@hive`'s lane `lane` holding `held` messages.
    fn assert_not_taken_busy(
        answer: &Message,
        sender: &str,
        trace_id: &str,
        lane: &str,
        held: usize,
    ) {
        assert!(is_system_kind(&answer.meta.msg_type), "{answer:?}");
        assert_eq!(answer.meta.msg.as_deref(), Some(MSG_NOT_TAKEN));
        assert_eq!(answer.routing.src, "test-uuid");
        assert!(
            matches!(&answer.routing.dst, Destination::Unicast(dst) if *dst == format!("{sender}-uuid")),
            "to the sender's uuid: {answer:?}"
        );
        assert_eq!(answer.routing.trace_id, trace_id);
        assert_eq!(answer.meta.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(answer.payload["ok"], json!(false));
        assert_eq!(answer.payload["error"], json!("not_taken"));
        assert_eq!(answer.payload["reason"], json!(NOT_TAKEN_REASON_NODE_BUSY));
        assert_eq!(answer.payload["retryable"], json!(true));
        assert_eq!(
            answer.payload["detail"],
            json!(format!(
                "IO.test@hive holds {held} {lane} messages, as many as it takes; send it again later"
            ))
        );
    }

    /// A message past its lane's bound never reaches the node; its sender hears NOT_TAKEN node_busy at
    /// once, on its trace and thread; the node reads every message the lane held, in order, and the
    /// lane takes messages again once it has room. Before 0.1.143 the channel had no bound and
    /// nothing went back.
    #[tokio::test]
    async fn a_full_lane_refuses_the_next_message_and_tells_its_sender_not_taken() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("IO.test@hive", incoming_profile());
        fill_lane(&client, &harness, "user", "IO.web@hive", None).await;

        harness
            .inject(from_node("IO.web@hive", "trace-over", "user", None))
            .await
            .unwrap();
        let answer = harness
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("NOT_TAKEN to the sender");
        assert_not_taken_busy(
            &answer,
            "IO.web@hive",
            "trace-over",
            "non-system",
            RPC_COMMAND_CHANNEL_CAPACITY,
        );
        assert_eq!(client.command_refused("incoming"), Some(1));
        assert_eq!(
            client.command_depth("incoming"),
            Some(RPC_COMMAND_CHANNEL_CAPACITY)
        );

        let mut incoming = client.take_command_receiver("incoming").await.unwrap();
        for i in 0..RPC_COMMAND_CHANNEL_CAPACITY {
            let msg = incoming.recv().await.unwrap();
            assert_eq!(msg.routing.trace_id, format!("fill-user-{i}"));
        }
        assert!(
            incoming.try_recv().is_none(),
            "the refused one never reaches the node"
        );

        harness
            .inject(from_node("IO.web@hive", "trace-after", "user", None))
            .await
            .unwrap();
        let msg = time::timeout(Duration::from_secs(1), incoming.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.routing.trace_id, "trace-after");
        assert_eq!(client.command_refused("incoming"), Some(1));
        assert_eq!(client.command_refused("nope"), None);
    }

    /// With the other lane full, a system message is still taken, and the node reads it first;
    /// system messages keep their own order. Before 0.1.143 it waited behind every user message.
    #[tokio::test]
    async fn system_messages_never_wait_behind_other_kinds() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("IO.test@hive", incoming_profile());
        fill_lane(&client, &harness, "user", "IO.web@hive", None).await;
        for (trace, msg) in [
            ("trace-status", "NODE_STATUS_GET"),
            ("trace-config", "CONFIG_GET"),
        ] {
            harness
                .inject(from_node(
                    "SY.orchestrator@hive",
                    trace,
                    SYSTEM_KIND,
                    Some(msg),
                ))
                .await
                .unwrap();
        }
        wait_for_depth(&client, RPC_COMMAND_CHANNEL_CAPACITY + 2).await;
        assert_eq!(client.command_refused("incoming"), Some(0));
        assert!(harness
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());

        let mut incoming = client.take_command_receiver("incoming").await.unwrap();
        assert_eq!(
            incoming.recv().await.unwrap().routing.trace_id,
            "trace-status"
        );
        assert_eq!(
            incoming.try_recv().unwrap().routing.trace_id,
            "trace-config"
        );
        assert_eq!(
            incoming.recv().await.unwrap().routing.trace_id,
            "fill-user-0"
        );
    }

    /// Who hears NOT_TAKEN for a message refused for want of room: any message a node sent, named
    /// or not, from any node; never a notice, a reply, a broadcast or a tap copy, so two nodes
    /// cannot bounce one.
    #[tokio::test]
    async fn only_a_sender_that_may_be_answered_is_told_not_taken() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("IO.test@hive", incoming_profile());
        fill_lane(&client, &harness, "user", "IO.web@hive", None).await;
        fill_lane(
            &client,
            &harness,
            SYSTEM_KIND,
            "SY.orchestrator@hive",
            Some("NODE_STATUS_GET"),
        )
        .await;

        let mut refused = Vec::new();
        let mut answered = Vec::new();
        let mut refuse = |msg: Message, told: bool| {
            if told {
                answered.push(msg.routing.trace_id.clone());
            }
            refused.push(msg);
        };
        // Answered.
        refuse(
            from_node("IO.web@hive", "user-unnamed-io", "user", None),
            true,
        );
        refuse(
            from_node("AI.agent@hive", "user-unnamed-agent", "user", None),
            true,
        );
        refuse(
            from_node("WF.flow@hive", "user-named", "user", Some("ask")),
            true,
        );
        refuse(
            from_node(
                "SY.timer@hive",
                "system-named",
                SYSTEM_KIND,
                Some("TIMER_FIRED"),
            ),
            true,
        );
        refuse(
            from_node(
                "SY.admin@motherbee",
                "admin-named",
                ADMIN_KIND,
                Some(MSG_ADMIN_COMMAND),
            ),
            true,
        );
        // Notices.
        let mut from_router = from_node("IO.web@hive", "no-src-l2-name", "user", Some("ask"));
        from_router.routing.src_l2_name = None;
        refuse(from_router, false);
        refuse(
            from_node(
                "RT.gateway@hive",
                "unreachable",
                SYSTEM_KIND,
                Some(MSG_UNREACHABLE),
            ),
            false,
        );
        refuse(
            from_node("SY.x@hive", "hello", SYSTEM_KIND, Some(MSG_HELLO)),
            false,
        );
        refuse(
            from_node("SY.x@hive", "system-unnamed", SYSTEM_KIND, None),
            false,
        );
        // Replies.
        refuse(
            from_node(
                "SY.x@hive",
                "response",
                SYSTEM_KIND,
                Some("CONFIG_RESPONSE"),
            ),
            false,
        );
        refuse(
            from_node(
                "WF.flow@hive",
                "not-taken",
                SYSTEM_KIND,
                Some(MSG_NOT_TAKEN),
            ),
            false,
        );
        refuse(
            from_node("AI.agent@hive", "agent-error", "user", Some("ask.error")),
            false,
        );
        refuse(
            from_node("SY.x@hive", "kind-response", "query_response", None),
            false,
        );
        // A broadcast and a tap copy.
        let mut broadcast = from_node(
            "SY.vault@motherbee",
            "broadcast",
            SYSTEM_KIND,
            Some("VAULT_SECRET_CHANGED"),
        );
        broadcast.routing.dst = Destination::Broadcast;
        refuse(broadcast, false);
        let mut tap = from_node("IO.web@hive", "tap-copy", "user", Some("ask"));
        tap.meta.via_tap = true;
        refuse(tap, false);

        let count = refused.len() as u64;
        for msg in refused {
            harness.inject(msg).await.unwrap();
        }
        let mut told = Vec::new();
        while let Some(answer) = harness
            .next_outgoing_within(Duration::from_millis(200))
            .await
        {
            assert_eq!(answer.meta.msg.as_deref(), Some(MSG_NOT_TAKEN));
            told.push(answer.routing.trace_id);
        }
        assert_eq!(told, answered);
        assert_eq!(client.command_refused("incoming"), Some(count));
    }

    /// The recv loop never waits for a NOT_TAKEN: with the link's outgoing queue full, the answer
    /// is dropped and the refusal still counted, and the loop goes on taking messages.
    #[tokio::test]
    async fn a_not_taken_the_link_cannot_take_is_dropped_without_waiting() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("IO.test@hive", incoming_profile());
        let sender = client.sender_snapshot();
        for i in 0..64 {
            sender
                .send(make_loose(
                    &format!("filler-{i}"),
                    "user",
                    "FILL",
                    json!({}),
                ))
                .await
                .unwrap();
        }
        fill_lane(&client, &harness, "user", "IO.web@hive", None).await;
        harness
            .inject(from_node("IO.web@hive", "trace-over", "user", None))
            .await
            .unwrap();
        harness
            .inject(from_node(
                "SY.orchestrator@hive",
                "trace-status",
                SYSTEM_KIND,
                Some("NODE_STATUS_GET"),
            ))
            .await
            .unwrap();
        wait_for_depth(&client, RPC_COMMAND_CHANNEL_CAPACITY + 1).await;
        assert_eq!(client.command_refused("incoming"), Some(1));
        for i in 0..64 {
            let out = harness.next_outgoing().await.unwrap();
            assert_eq!(out.routing.trace_id, format!("filler-{i}"));
        }
        assert!(harness
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());
    }

    /// The refusals of a burst within 10 s of a warning get their own warning at the end of the
    /// 10 s, even when nothing follows them.
    #[tokio::test(start_paused = true)]
    async fn a_burst_refused_after_a_warning_is_warned_when_the_interval_ends() {
        let (client, harness) =
            RouterDispatcherTestHarness::new("IO.test@hive", incoming_profile());
        fill_lane(&client, &harness, "user", "IO.web@hive", None).await;
        for i in 0..3 {
            harness
                .inject(from_node("IO.web@hive", &format!("over-{i}"), "user", None))
                .await
                .unwrap();
        }
        let channel = client.command.get("incoming").unwrap();
        time::timeout(Duration::from_secs(5), async {
            while channel.refused() < 3 {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        // The first refusal was warned at once; the other two wait for the interval's end.
        {
            let log = lock(&channel.refusals);
            assert_eq!(log.refused, 2);
            assert!(log.flush_due);
        }

        time::advance(COMMAND_REFUSAL_LOG_INTERVAL).await;
        time::timeout(Duration::from_secs(1), async {
            while lock(&channel.refusals).flush_due {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the interval's warning was written");
        let log = lock(&channel.refusals);
        assert_eq!(log.refused, 0, "warned at the interval's end");
        assert!(log.line_at.is_some());
        drop(log);
        assert_eq!(client.command_refused("incoming"), Some(3));
    }

    /// A lane also holds at most `RPC_COMMAND_CHANNEL_MAX_BYTES` of its messages' frames, as the
    /// Go SDK's: large messages are refused well below 1,024, and each one the node reads gives its
    /// bytes back.
    #[tokio::test]
    async fn a_lane_holds_at_most_its_bytes() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("IO.test@hive", incoming_profile());
        let big = |trace: &str| {
            let mut msg = from_node("IO.web@hive", trace, "user", None);
            msg.payload = json!({"text": "x".repeat(100_000)});
            msg
        };
        let frame_len = serde_json::to_vec(&big("big-0000")).unwrap().len();
        let fit = RPC_COMMAND_CHANNEL_MAX_BYTES / frame_len;
        assert!(fit < RPC_COMMAND_CHANNEL_CAPACITY);
        for i in 0..fit {
            harness.inject(big(&format!("big-{i:04}"))).await.unwrap();
        }
        wait_for_depth(&client, fit).await;
        let channel = client.command.get("incoming").unwrap();
        assert_eq!(channel.other.bytes(), fit * frame_len);

        harness.inject(big("big-over")).await.unwrap();
        let answer = harness
            .next_outgoing_within(Duration::from_secs(2))
            .await
            .expect("NOT_TAKEN to the sender");
        assert_not_taken_busy(&answer, "IO.web@hive", "big-over", "non-system", fit);
        assert_eq!(client.command_refused("incoming"), Some(1));

        let mut incoming = client.take_command_receiver("incoming").await.unwrap();
        assert_eq!(incoming.recv().await.unwrap().routing.trace_id, "big-0000");
        assert_eq!(channel.other.bytes(), (fit - 1) * frame_len);
        harness.inject(big("big-room")).await.unwrap();
        wait_for_depth(&client, fit).await;
        while incoming.try_recv().is_some() {}
        assert_eq!(channel.other.bytes(), 0, "every byte given back");
        assert_eq!(client.command_depth("incoming"), Some(0));
    }

    fn not_taken_answer(request: &Message, retryable: Option<bool>) -> Message {
        let mut answer = from_node(
            "SY.target@hive",
            &request.routing.trace_id,
            SYSTEM_KIND,
            Some(MSG_NOT_TAKEN),
        );
        answer.payload =
            json!({"ok": false, "error": "not_taken", "reason": "node_busy", "detail": "full"});
        if let Some(retryable) = retryable {
            answer.payload["retryable"] = json!(retryable);
        }
        answer
    }

    /// A request its destination did not take ends at once with `RpcError::NotTaken`, whatever its
    /// matcher: a system request (before: `InvalidResponse`), an admin request (before: it waited
    /// for its timeout) and a call whose success matches any message (before: the NOT_TAKEN was its
    /// answer). A NOT_TAKEN after the request ended is stale.
    #[tokio::test]
    async fn a_request_its_destination_did_not_take_ends_at_once() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut admin_rx = client.take_command_receiver("admin").await.unwrap();
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();

        let c = Arc::clone(&client);
        let system = tokio::spawn(async move {
            c.send_system_rpc(SystemRpcRequest {
                target: "SY.target@hive",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(5),
            })
            .await
        });
        let request = harness.next_outgoing().await.unwrap();
        harness
            .inject(not_taken_answer(&request, Some(true)))
            .await
            .unwrap();
        let err = time::timeout(Duration::from_secs(1), system)
            .await
            .expect("ends at the NOT_TAKEN")
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(&err, RpcError::NotTaken { reason, detail, retryable: true } if reason == "node_busy" && detail == "full"),
            "got {err:?}"
        );

        let c = Arc::clone(&client);
        let admin = tokio::spawn(async move {
            c.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@motherbee",
                action: "list_hives",
                target: None,
                params: json!({}),
                request_id: None,
                timeout: Duration::from_secs(5),
            })
            .await
        });
        let request = harness.next_outgoing().await.unwrap();
        harness
            .inject(not_taken_answer(&request, None))
            .await
            .unwrap();
        let err = time::timeout(Duration::from_secs(1), admin)
            .await
            .expect("ends at the NOT_TAKEN")
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(
                &err,
                RpcError::NotTaken {
                    retryable: false,
                    ..
                }
            ),
            "a NOT_TAKEN without retryable reads false: {err:?}"
        );
        assert!(!err.is_timeout());

        let c = Arc::clone(&client);
        let any = tokio::spawn(async move {
            c.send_with_matcher(
                make_outgoing("user", "ASK"),
                PendingMatcher::new(vec![RouteMatch::Any], Vec::new(), Vec::new()),
                RpcRequestLabels::new("IO.handler@hive", "ASK", "*"),
                Duration::from_secs(5),
            )
            .await
        });
        let request = harness.next_outgoing().await.unwrap();
        harness
            .inject(not_taken_answer(&request, Some(true)))
            .await
            .unwrap();
        let err = time::timeout(Duration::from_secs(1), any)
            .await
            .expect("ends at the NOT_TAKEN")
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(
                err,
                RpcError::NotTaken {
                    retryable: true,
                    ..
                }
            ),
            "got {err:?}"
        );

        // Late: after its request timed out, a NOT_TAKEN is stale, never a command.
        let late = time::timeout(
            Duration::from_secs(1),
            client.send_admin_rpc(AdminCommandRequest {
                admin_target: "SY.admin@motherbee",
                action: "list_hives",
                target: None,
                params: json!({}),
                request_id: None,
                timeout: Duration::from_millis(30),
            }),
        )
        .await
        .unwrap();
        assert!(late.unwrap_err().is_timeout());
        let request = harness.next_outgoing().await.unwrap();
        let stale_before = client.metric_stale_responses();
        harness
            .inject(not_taken_answer(&request, Some(true)))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert_eq!(client.metric_stale_responses(), stale_before + 1);
        assert!(admin_rx.try_recv().is_none());
        assert!(sys_rx.try_recv().is_none());
    }

    // ---- A call its caller drops (FINDINGS A-309) ----

    fn plain_matcher() -> PendingMatcher {
        PendingMatcher::new(
            vec![RouteMatch::exact(SYSTEM_KIND, "RESP")],
            Vec::new(),
            vec![RouteMatch::any_msg_type(SYSTEM_KIND)],
        )
    }

    /// A call its caller drops, here at an outer timeout, leaves nothing pending: its trace_id is
    /// free for the next call at once. Before 0.1.143 its entry stayed, and the next call with that
    /// trace_id was refused as a duplicate.
    #[tokio::test]
    async fn a_call_its_caller_drops_frees_its_trace_id() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut request = make_outgoing(SYSTEM_KIND, "REQ");
        request.routing.trace_id = "trace-dropped".to_string();
        let labels = RpcRequestLabels::new("SY.target@hive", "REQ", "RESP");
        let cut = time::timeout(
            Duration::from_millis(50),
            client.send_with_matcher(
                request.clone(),
                plain_matcher(),
                labels.clone(),
                Duration::from_secs(30),
            ),
        )
        .await;
        assert!(cut.is_err(), "the caller's own cut ends the call");
        let first = harness.next_outgoing().await.unwrap();
        assert_eq!(first.routing.trace_id, "trace-dropped");
        assert!(lock(&client.pending).is_empty());
        assert_eq!(client.metric_abandoned_requests(), 1);

        let c = Arc::clone(&client);
        let again = tokio::spawn(async move {
            c.send_with_matcher(request, plain_matcher(), labels, Duration::from_secs(2))
                .await
        });
        let second = harness
            .next_outgoing_within(Duration::from_secs(1))
            .await
            .expect("the same trace_id is free again");
        harness
            .inject(make_response(&second, SYSTEM_KIND, "RESP", json!({"k": 1})))
            .await
            .unwrap();
        assert_eq!(again.await.unwrap().unwrap().payload, json!({"k": 1}));
        assert_eq!(client.metric_abandoned_requests(), 1);
    }

    /// An answer that comes after its call was dropped is stale: dropped and counted, never handed
    /// to the node as a command. Before 0.1.143 it completed the entry the call had left behind.
    #[tokio::test]
    async fn a_late_answer_to_a_dropped_call_is_stale() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let mut sys_rx = client.take_command_receiver("system").await.unwrap();
        let cut = time::timeout(
            Duration::from_millis(50),
            client.send_system_rpc(SystemRpcRequest {
                target: "SY.target@hive",
                request_msg: "REQ",
                response_msg: "RESP",
                payload: json!({}),
                timeout: Duration::from_secs(30),
            }),
        )
        .await;
        assert!(cut.is_err());
        let sent = harness.next_outgoing().await.unwrap();
        harness
            .inject(make_response(&sent, SYSTEM_KIND, "RESP", json!({})))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
        assert_eq!(client.metric_stale_responses(), 1);
        assert!(sys_rx.try_recv().is_none());
    }

    /// A call dropped while its message waits for room in the link's outgoing queue never sends
    /// it, and leaves nothing pending.
    #[tokio::test]
    async fn a_call_dropped_before_its_message_went_out_never_sends_it() {
        let (client, mut harness) =
            RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let sender = client.sender_snapshot();
        for i in 0..64 {
            sender
                .send(make_loose(
                    &format!("filler-{i}"),
                    "user",
                    "FILL",
                    json!({}),
                ))
                .await
                .unwrap();
        }
        let mut request = make_outgoing(SYSTEM_KIND, "REQ");
        request.routing.trace_id = "trace-unsent".to_string();
        let labels = RpcRequestLabels::new("SY.target@hive", "REQ", "RESP");
        let cut = time::timeout(
            Duration::from_millis(50),
            client.send_with_matcher(
                request.clone(),
                plain_matcher(),
                labels.clone(),
                Duration::from_secs(30),
            ),
        )
        .await;
        assert!(cut.is_err());
        assert!(lock(&client.pending).is_empty());
        for _ in 0..64 {
            let out = harness.next_outgoing().await.unwrap();
            assert_ne!(out.routing.trace_id, "trace-unsent");
        }
        assert!(harness
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());

        let c = Arc::clone(&client);
        let again = tokio::spawn(async move {
            c.send_with_matcher(request, plain_matcher(), labels, Duration::from_secs(2))
                .await
        });
        let resent = harness
            .next_outgoing_within(Duration::from_secs(1))
            .await
            .expect("the same trace_id is free again");
        harness
            .inject(make_response(&resent, SYSTEM_KIND, "RESP", json!({})))
            .await
            .unwrap();
        assert!(again.await.unwrap().is_ok());
    }

    /// A guard removes only its own call's entry: never a later call's under the same trace_id.
    #[tokio::test]
    async fn a_guard_never_removes_a_later_calls_entry() {
        let (client, _harness) = RouterDispatcherTestHarness::new("SY.test@hive", simple_profile());
        let (tx, _rx) = oneshot::channel();
        let later = client
            .register_waiter("trace-shared", &plain_matcher(), tx)
            .unwrap();
        let later_id = later.id;
        drop(PendingGuard {
            dispatcher: &client,
            trace_id: "trace-shared".to_string(),
            id: later_id.wrapping_sub(1),
            armed: true,
        });
        assert!(lock(&client.pending)
            .get("trace-shared")
            .is_some_and(|entry| entry.id == later_id));
        assert_eq!(client.metric_abandoned_requests(), 0);

        drop(later);
        assert!(lock(&client.pending).is_empty());
        assert_eq!(client.metric_abandoned_requests(), 1);
        let stale_trace = make_loose("trace-shared", SYSTEM_KIND, "RESP", json!({}));
        assert!(matches!(
            lock(&client.stale).classify(&stale_trace),
            StaleClassification::Stale
        ));
    }
}
