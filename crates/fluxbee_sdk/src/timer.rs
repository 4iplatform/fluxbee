//! Typed SDK support for [`SY.timer`](crate::timer::TIMER_NODE_KIND).
//!
//! Minimal live examples are available in:
//! - `examples/timer_client.rs`
//! - `examples/timer_recurring.rs`
//! - `examples/timer_restart.rs`
//!
//! ## Async fire-and-forget pattern with `client_ref` (v1.1)
//!
//! `SY.timer` v1.1 supports deterministic client-owned identifiers via `client_ref`.
//! The key property: a client can schedule a timer and immediately cancel or
//! reschedule it by `client_ref` **without waiting for `TIMER_SCHEDULE_RESPONSE`**
//! to obtain the server-generated UUID. This is essential for event-loop clients
//! like `WF.*` nodes that must not block their receive loop.
//!
//! ```rust,ignore
//! // Schedule an SLA timeout. The client_ref is deterministic and known
//! // before the response arrives.
//! let client_ref = format!("wf:{}::sla_timeout", instance_id);
//!
//! let _timer_id = client
//!     .schedule_in(
//!         120_000, // 2 minutes in ms
//!         ScheduleOptions {
//!             client_ref: Some(client_ref.clone()),
//!             payload: serde_json::json!({ "instance_id": instance_id }),
//!             ..Default::default()
//!         },
//!     )
//!     .await?;
//!
//! // Later — e.g. when the step completes before the timeout — cancel by
//! // client_ref without needing to track the UUID.
//! client.cancel_by_client_ref(&client_ref).await?;
//! ```
//!
//! Scheduling with the same `client_ref` while a pending timer already exists
//! is idempotent: `SY.timer` returns the existing timer with `already_existed: true`
//! instead of creating a duplicate. This makes crash-recovery safe: a node that
//! restarts and re-schedules its timers will not accumulate duplicates.
//!
//! The typed client exposes the full `client_ref` surface through:
//! - [`TimerClient::get_by_client_ref`]
//! - [`TimerClient::cancel_by_client_ref`]
//! - [`TimerClient::reschedule_by_client_ref`]
//!
//! For unit tests that should not depend on a real hive, use [`TimerTestHarness`]
//! to drive a real [`TimerClient`] over a scripted in-memory transport.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant as TokioInstant};
use uuid::Uuid;

use crate::protocol::{
    build_system_message, Destination, Message, Meta, Routing, TtlExceededPayload,
    UnreachablePayload, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::split::{ConnectionInfo, ConnectionState};
use crate::{NodeError, NodeReceiver, NodeSender};

pub const TIMER_NODE_KIND: &str = "SY.timer";
pub const TIMER_NODE_FAMILY: &str = "SY";

pub const MSG_TIMER_HELP: &str = "TIMER_HELP";
pub const MSG_TIMER_NOW: &str = "TIMER_NOW";
pub const MSG_TIMER_NOW_IN: &str = "TIMER_NOW_IN";
pub const MSG_TIMER_CONVERT: &str = "TIMER_CONVERT";
pub const MSG_TIMER_PARSE: &str = "TIMER_PARSE";
pub const MSG_TIMER_FORMAT: &str = "TIMER_FORMAT";
pub const MSG_TIMER_SCHEDULE: &str = "TIMER_SCHEDULE";
pub const MSG_TIMER_SCHEDULE_RECURRING: &str = "TIMER_SCHEDULE_RECURRING";
pub const MSG_TIMER_GET: &str = "TIMER_GET";
pub const MSG_TIMER_LIST: &str = "TIMER_LIST";
pub const MSG_TIMER_CANCEL: &str = "TIMER_CANCEL";
pub const MSG_TIMER_RESCHEDULE: &str = "TIMER_RESCHEDULE";
pub const MSG_TIMER_PURGE_OWNER: &str = "TIMER_PURGE_OWNER";
pub const MSG_TIMER_FIRED: &str = "TIMER_FIRED";
pub const MSG_TIMER_RESPONSE: &str = "TIMER_RESPONSE";

pub const TIMER_MIN_DURATION_MS: u64 = 60_000;
pub const TIMER_LIST_DEFAULT_LIMIT: u32 = 100;
pub const TIMER_LIST_MAX_LIMIT: u32 = 1000;
pub const TIMER_DEFAULT_RPC_TIMEOUT_MS: u64 = 5_000;
pub const TIMER_DEFAULT_TIME_RETRY_SCHEDULE_MS: [u64; 3] = [100, 300, 1_000];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct TimerId(pub String);

impl TimerId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl From<String> for TimerId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for TimerId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl AsRef<str> for TimerId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerKind {
    Oneshot,
    Recurring,
}

impl Default for TimerKind {
    fn default() -> Self {
        Self::Oneshot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    Pending,
    Fired,
    Canceled,
}

impl Default for TimerStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatusFilter {
    Pending,
    Fired,
    Canceled,
    All,
}

impl Default for TimerStatusFilter {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    Fire,
    Drop,
    FireIfWithin,
}

impl Default for MissedPolicy {
    fn default() -> Self {
        Self::Fire
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerInfo {
    pub uuid: TimerId,
    pub owner_l2_name: String,
    pub target_l2_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    pub kind: TimerKind,
    pub fire_at_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_tz: Option<String>,
    pub missed_policy: MissedPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_within_ms: Option<u64>,
    pub status: TimerStatus,
    pub created_at_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at_utc_ms: Option<i64>,
    pub fire_count: u64,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct FiredEvent {
    pub timer_uuid: TimerId,
    pub owner_l2_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    pub kind: TimerKind,
    pub scheduled_fire_at_utc_ms: i64,
    pub actual_fire_at_utc_ms: i64,
    pub fire_count: u64,
    pub is_last_fire: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub user_payload: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerListFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_l2_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_filter: Option<TimerStatusFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerHelpOperationDescriptor {
    pub verb: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_request: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example_response: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerHelpErrorDescriptor {
    pub code: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerHelpDescriptor {
    #[serde(default)]
    pub ok: bool,
    pub verb: String,
    pub node_family: String,
    pub node_kind: String,
    pub version: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<TimerHelpOperationDescriptor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_codes: Vec<TimerHelpErrorDescriptor>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerNowInPayload {
    pub tz: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerNowResult {
    pub now_utc_ms: i64,
    pub now_utc_iso: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerNowInResult {
    pub now_utc_ms: i64,
    pub tz: String,
    pub now_local_iso: String,
    pub tz_offset_seconds: i32,
    pub tz_abbrev: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerConvertPayload {
    pub instant_utc_ms: i64,
    pub to_tz: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerConvertResult {
    pub instant_utc_ms: i64,
    pub to_tz: String,
    pub local_iso: String,
    pub tz_offset_seconds: i32,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerParsePayload {
    pub input: String,
    pub layout: String,
    pub tz: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerParseResult {
    pub instant_utc_ms: i64,
    pub resolved_iso_utc: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerFormatPayload {
    pub instant_utc_ms: i64,
    pub layout: String,
    pub tz: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerFormatResult {
    pub formatted: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerSchedulePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_at_utc_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_in_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_l2_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_policy: Option<MissedPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_within_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerScheduleRecurringPayload {
    pub cron_spec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_tz: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_l2_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_policy: Option<MissedPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missed_within_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metadata: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerGetPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_uuid: Option<TimerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerListPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_l2_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_filter: Option<TimerStatusFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerCancelPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_uuid: Option<TimerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerReschedulePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_uuid: Option<TimerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_fire_at_utc_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_fire_in_ms: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerPurgeOwnerPayload {
    pub owner_l2_name: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_uuid: Option<TimerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_at_utc_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TimerStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub already_existed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TimerErrorDetail>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerGetResponse {
    #[serde(default)]
    pub ok: bool,
    pub verb: String,
    pub timer: TimerInfo,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerListResponse {
    #[serde(default)]
    pub ok: bool,
    pub verb: String,
    pub count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timers: Vec<TimerInfo>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum TimerClientError {
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("timer transport unreachable: reason={reason}, original_dst={original_dst}")]
    Unreachable {
        reason: String,
        original_dst: String,
    },
    #[error("timer transport ttl exceeded: original_dst={original_dst}, last_hop={last_hop}")]
    TtlExceeded {
        original_dst: String,
        last_hop: String,
    },
    #[error("timeout waiting {response_msg} trace_id={trace_id} target={target} verb={verb} timeout_ms={timeout_ms}")]
    Timeout {
        response_msg: String,
        trace_id: String,
        target: String,
        verb: String,
        timeout_ms: u64,
    },
    #[error("timer service error: verb={verb}, code={code}, message={message}")]
    ServiceError {
        verb: String,
        code: String,
        message: String,
    },
    #[error("timer contract violation: {0}")]
    ContractViolation(String),
}

#[derive(Debug, Clone)]
pub struct TimerClientConfig {
    pub timer_target: Option<String>,
    pub rpc_timeout: Duration,
    pub time_retry_schedule: Vec<Duration>,
}

impl Default for TimerClientConfig {
    fn default() -> Self {
        Self {
            timer_target: None,
            rpc_timeout: Duration::from_millis(TIMER_DEFAULT_RPC_TIMEOUT_MS),
            time_retry_schedule: TIMER_DEFAULT_TIME_RETRY_SCHEDULE_MS
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
        }
    }
}

pub struct TimerClient<'a> {
    sender: &'a NodeSender,
    receiver: &'a mut NodeReceiver,
    timer_target: String,
    rpc_timeout: Duration,
    time_retry_schedule: Vec<Duration>,
}

/// Scripted in-memory transport for testing [`TimerClient`] and timer-aware Rust nodes
/// without a live hive or a running `SY.timer`.
pub struct TimerTestHarness {
    outbound_rx: mpsc::Receiver<Vec<u8>>,
    inbound_tx: mpsc::Sender<Result<Message, NodeError>>,
}

impl<'a> TimerClient<'a> {
    pub fn new(
        sender: &'a NodeSender,
        receiver: &'a mut NodeReceiver,
        config: TimerClientConfig,
    ) -> Result<Self, TimerClientError> {
        let timer_target = match config.timer_target {
            Some(target) if !target.trim().is_empty() => target.trim().to_string(),
            _ => local_timer_node_name(sender.full_name())?,
        };
        Ok(Self {
            sender,
            receiver,
            timer_target,
            rpc_timeout: default_rpc_timeout(config.rpc_timeout),
            time_retry_schedule: normalized_time_retry_schedule(config.time_retry_schedule),
        })
    }

    pub fn target(&self) -> &str {
        self.timer_target.as_str()
    }

    pub fn sender(&self) -> &NodeSender {
        self.sender
    }

    pub async fn help(&mut self) -> Result<TimerHelpDescriptor, TimerClientError> {
        self.call_decode(MSG_TIMER_HELP, Value::Object(Map::new()), self.rpc_timeout)
            .await
    }

    pub async fn now(&mut self) -> Result<TimerNowResult, TimerClientError> {
        self.call_time_operation(MSG_TIMER_NOW, Value::Object(Map::new()))
            .await
    }

    pub async fn now_in(
        &mut self,
        mut payload: TimerNowInPayload,
    ) -> Result<TimerNowInResult, TimerClientError> {
        payload.tz = trim_owned(payload.tz);
        if payload.tz.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "tz must be non-empty".to_string(),
            ));
        }
        self.call_time_operation(MSG_TIMER_NOW_IN, serde_json::to_value(payload)?)
            .await
    }

    pub async fn convert(
        &mut self,
        mut payload: TimerConvertPayload,
    ) -> Result<TimerConvertResult, TimerClientError> {
        payload.to_tz = trim_owned(payload.to_tz);
        if payload.to_tz.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "to_tz must be non-empty".to_string(),
            ));
        }
        self.call_time_operation(MSG_TIMER_CONVERT, serde_json::to_value(payload)?)
            .await
    }

    pub async fn parse(
        &mut self,
        mut payload: TimerParsePayload,
    ) -> Result<TimerParseResult, TimerClientError> {
        payload.input = trim_owned(payload.input);
        payload.layout = trim_owned(payload.layout);
        payload.tz = trim_owned(payload.tz);
        if payload.input.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "input must be non-empty".to_string(),
            ));
        }
        if payload.layout.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "layout must be non-empty".to_string(),
            ));
        }
        if payload.tz.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "tz must be non-empty".to_string(),
            ));
        }
        self.call_time_operation(MSG_TIMER_PARSE, serde_json::to_value(payload)?)
            .await
    }

    pub async fn format(
        &mut self,
        mut payload: TimerFormatPayload,
    ) -> Result<TimerFormatResult, TimerClientError> {
        payload.layout = trim_owned(payload.layout);
        payload.tz = trim_owned(payload.tz);
        if payload.layout.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "layout must be non-empty".to_string(),
            ));
        }
        if payload.tz.is_empty() {
            return Err(TimerClientError::InvalidRequest(
                "tz must be non-empty".to_string(),
            ));
        }
        self.call_time_operation(MSG_TIMER_FORMAT, serde_json::to_value(payload)?)
            .await
    }

    pub async fn schedule(
        &mut self,
        mut payload: TimerSchedulePayload,
    ) -> Result<TimerId, TimerClientError> {
        normalize_schedule_payload(&mut payload);
        validate_schedule_payload(&payload, MSG_TIMER_SCHEDULE)?;
        let response = self
            .call_ok_response(
                MSG_TIMER_SCHEDULE,
                serde_json::to_value(payload)?,
                self.rpc_timeout,
            )
            .await?;
        response.timer_uuid.ok_or_else(|| {
            TimerClientError::ContractViolation("timer response missing timer_uuid".to_string())
        })
    }

    pub async fn schedule_in(
        &mut self,
        duration: Duration,
        mut payload: TimerSchedulePayload,
    ) -> Result<TimerId, TimerClientError> {
        payload.fire_at_utc_ms = None;
        payload.fire_in_ms = Some(duration.as_millis() as u64);
        self.schedule(payload).await
    }

    pub async fn schedule_recurring(
        &mut self,
        mut payload: TimerScheduleRecurringPayload,
    ) -> Result<TimerId, TimerClientError> {
        normalize_recurring_payload(&mut payload);
        validate_recurring_payload(&payload)?;
        let response = self
            .call_ok_response(
                MSG_TIMER_SCHEDULE_RECURRING,
                serde_json::to_value(payload)?,
                self.rpc_timeout,
            )
            .await?;
        response.timer_uuid.ok_or_else(|| {
            TimerClientError::ContractViolation("timer response missing timer_uuid".to_string())
        })
    }

    pub async fn get(
        &mut self,
        timer_id: impl Into<TimerId>,
    ) -> Result<TimerInfo, TimerClientError> {
        let timer_uuid = validate_timer_id(timer_id.into())?;
        let payload = TimerGetPayload {
            timer_uuid: Some(timer_uuid),
            ..TimerGetPayload::default()
        };
        let response: TimerGetResponse = self
            .call_decode(
                MSG_TIMER_GET,
                serde_json::to_value(payload)?,
                self.rpc_timeout,
            )
            .await?;
        Ok(response.timer)
    }

    pub async fn get_by_client_ref(
        &mut self,
        client_ref: impl Into<String>,
    ) -> Result<TimerInfo, TimerClientError> {
        let payload = TimerGetPayload {
            client_ref: Some(validate_client_ref(client_ref.into())?),
            ..TimerGetPayload::default()
        };
        let response: TimerGetResponse = self
            .call_decode(
                MSG_TIMER_GET,
                serde_json::to_value(payload)?,
                self.rpc_timeout,
            )
            .await?;
        Ok(response.timer)
    }

    pub async fn list(
        &mut self,
        mut filter: TimerListFilter,
    ) -> Result<TimerListResponse, TimerClientError> {
        normalize_list_filter(&mut filter);
        validate_list_filter(&filter)?;
        self.call_decode(
            MSG_TIMER_LIST,
            serde_json::to_value(filter)?,
            self.rpc_timeout,
        )
        .await
    }

    pub async fn list_mine(
        &mut self,
        mut filter: TimerListFilter,
    ) -> Result<TimerListResponse, TimerClientError> {
        if filter.owner_l2_name.is_none() {
            filter.owner_l2_name = Some(self.sender.full_name().to_string());
        }
        self.list(filter).await
    }

    pub async fn cancel(
        &mut self,
        timer_id: impl Into<TimerId>,
    ) -> Result<TimerResponse, TimerClientError> {
        let payload = TimerCancelPayload {
            timer_uuid: Some(validate_timer_id(timer_id.into())?),
            ..TimerCancelPayload::default()
        };
        self.call_ok_response(
            MSG_TIMER_CANCEL,
            serde_json::to_value(payload)?,
            self.rpc_timeout,
        )
        .await
    }

    pub async fn cancel_by_client_ref(
        &mut self,
        client_ref: impl Into<String>,
    ) -> Result<TimerResponse, TimerClientError> {
        let payload = TimerCancelPayload {
            client_ref: Some(validate_client_ref(client_ref.into())?),
            ..TimerCancelPayload::default()
        };
        self.call_ok_response(
            MSG_TIMER_CANCEL,
            serde_json::to_value(payload)?,
            self.rpc_timeout,
        )
        .await
    }

    pub async fn reschedule(
        &mut self,
        mut payload: TimerReschedulePayload,
    ) -> Result<TimerResponse, TimerClientError> {
        normalize_reschedule_payload(&mut payload);
        validate_reschedule_payload(&payload)?;
        self.call_ok_response(
            MSG_TIMER_RESCHEDULE,
            serde_json::to_value(payload)?,
            self.rpc_timeout,
        )
        .await
    }

    pub async fn reschedule_by_client_ref(
        &mut self,
        client_ref: impl Into<String>,
        new_fire_at_utc_ms: i64,
    ) -> Result<TimerResponse, TimerClientError> {
        let mut payload = TimerReschedulePayload {
            client_ref: Some(validate_client_ref(client_ref.into())?),
            new_fire_at_utc_ms: Some(new_fire_at_utc_ms),
            ..TimerReschedulePayload::default()
        };
        normalize_reschedule_payload(&mut payload);
        validate_reschedule_payload(&payload)?;
        self.call_ok_response(
            MSG_TIMER_RESCHEDULE,
            serde_json::to_value(payload)?,
            self.rpc_timeout,
        )
        .await
    }

    pub async fn purge_owner(
        &mut self,
        owner_l2_name: &str,
    ) -> Result<TimerResponse, TimerClientError> {
        let owner_l2_name = trim_owned(owner_l2_name.to_string());
        validate_canonical_l2_name("owner_l2_name", &owner_l2_name)?;
        let payload = TimerPurgeOwnerPayload {
            owner_l2_name,
            ..TimerPurgeOwnerPayload::default()
        };
        self.call_ok_response(
            MSG_TIMER_PURGE_OWNER,
            serde_json::to_value(payload)?,
            self.rpc_timeout,
        )
        .await
    }

    async fn call_time_operation<T>(
        &mut self,
        verb: &str,
        payload: Value,
    ) -> Result<T, TimerClientError>
    where
        T: DeserializeOwned,
    {
        let mut last_error = None;
        for timeout in self.time_retry_schedule.clone() {
            match self.call_decode::<T>(verb, payload.clone(), timeout).await {
                Ok(value) => return Ok(value),
                Err(err) if should_retry_time_operation(&err) => last_error = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(
            last_error.unwrap_or_else(|| TimerClientError::ServiceError {
                verb: verb.to_string(),
                code: "TIMER_UNREACHABLE".to_string(),
                message: "SY.timer did not respond after retry schedule".to_string(),
            }),
        )
    }

    async fn call_decode<T>(
        &mut self,
        verb: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<T, TimerClientError>
    where
        T: DeserializeOwned,
    {
        let message = self.request_once(verb, payload, timeout).await?;
        let response = parse_timer_response(&message)?;
        if let Some(err) = timer_response_service_error(&response) {
            return Err(err);
        }
        Ok(serde_json::from_value(message.payload)?)
    }

    async fn call_ok_response(
        &mut self,
        verb: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<TimerResponse, TimerClientError> {
        let message = self.request_once(verb, payload, timeout).await?;
        let response = parse_timer_response(&message)?;
        if let Some(err) = timer_response_service_error(&response) {
            return Err(err);
        }
        Ok(response)
    }

    async fn request_once(
        &mut self,
        verb: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Message, TimerClientError> {
        let request = build_timer_system_request_with_target(
            self.sender.uuid(),
            self.timer_target.as_str(),
            verb,
            payload,
        )?;
        let trace_id = request.routing.trace_id.clone();
        self.sender.send(request).await?;
        self.wait_timer_response(verb, &trace_id, timeout).await
    }

    async fn wait_timer_response(
        &mut self,
        verb: &str,
        trace_id: &str,
        timeout: Duration,
    ) -> Result<Message, TimerClientError> {
        let timeout = default_rpc_timeout(timeout);
        let deadline = TokioInstant::now() + timeout;
        loop {
            let now = TokioInstant::now();
            if now >= deadline {
                return Err(TimerClientError::Timeout {
                    response_msg: MSG_TIMER_RESPONSE.to_string(),
                    trace_id: trace_id.to_string(),
                    target: self.timer_target.clone(),
                    verb: verb.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            let remaining = deadline - now;
            let incoming = match self.receiver.recv_timeout(remaining).await {
                Ok(message) => message,
                Err(NodeError::Timeout) => {
                    return Err(TimerClientError::Timeout {
                        response_msg: MSG_TIMER_RESPONSE.to_string(),
                        trace_id: trace_id.to_string(),
                        target: self.timer_target.clone(),
                        verb: verb.to_string(),
                        timeout_ms: timeout.as_millis() as u64,
                    })
                }
                Err(err) => return Err(TimerClientError::Node(err)),
            };
            if incoming.routing.trace_id != trace_id {
                continue;
            }
            if let Some(err) = map_timer_transport_message(&incoming) {
                return Err(err);
            }
            if is_timer_response_message(&incoming) {
                return Ok(incoming);
            }
        }
    }
}

impl TimerTestHarness {
    pub fn new(full_name: &str) -> (NodeSender, NodeReceiver, Self) {
        let (outbound_tx, outbound_rx) = mpsc::channel(16);
        let (inbound_tx, inbound_rx) = mpsc::channel(16);
        let state = Arc::new(ConnectionState::new_connected());
        let info = Arc::new(ConnectionInfo::new(
            "src-uuid".to_string(),
            full_name.to_string(),
            7,
            state,
        ));
        let sender = NodeSender::new(outbound_tx, Arc::clone(&info));
        let receiver = NodeReceiver::new(inbound_rx, info);
        (
            sender,
            receiver,
            Self {
                outbound_rx,
                inbound_tx,
            },
        )
    }

    pub async fn next_request(&mut self) -> Result<Message, TimerClientError> {
        let frame = self.outbound_rx.recv().await.ok_or_else(|| {
            TimerClientError::ContractViolation(
                "timer test harness outbound queue closed".to_string(),
            )
        })?;
        Ok(serde_json::from_slice(&frame)?)
    }

    pub async fn send_message(&self, message: Message) -> Result<(), NodeError> {
        self.inbound_tx
            .send(Ok(message))
            .await
            .map_err(|_| NodeError::Disconnected)
    }

    pub async fn send_timer_response(
        &self,
        request: &Message,
        payload: Value,
    ) -> Result<(), NodeError> {
        self.send_message(Message {
            routing: Routing {
                src: "timer-node-uuid".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(request.routing.src.clone()),
                ttl: 16,
                trace_id: request.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_TIMER_RESPONSE.to_string()),
                action: Some(MSG_TIMER_RESPONSE.to_string()),
                ..Meta::default()
            },
            payload,
        })
        .await
    }

    pub async fn send_unreachable(&self, request: &Message, reason: &str) -> Result<(), NodeError> {
        self.send_message(Message {
            routing: Routing {
                src: "router".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(request.routing.src.clone()),
                ttl: 1,
                trace_id: request.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_UNREACHABLE.to_string()),
                ..Meta::default()
            },
            payload: serde_json::json!({
                "original_dst": request.routing.dst,
                "reason": reason,
            }),
        })
        .await
    }

    pub async fn send_timer_fired(
        &self,
        dst_uuid: &str,
        trace_id: &str,
        event: &FiredEvent,
    ) -> Result<(), NodeError> {
        self.send_message(Message {
            routing: Routing {
                src: "SY.timer@motherbee".to_string(),
                src_l2_name: Some("SY.timer@motherbee".to_string()),
                dst: Destination::Unicast(dst_uuid.to_string()),
                ttl: 16,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_TIMER_FIRED.to_string()),
                action: Some(MSG_TIMER_FIRED.to_string()),
                ..Meta::default()
            },
            payload: serde_json::to_value(event).map_err(NodeError::Json)?,
        })
        .await
    }
}

pub fn timer_node_name(hive_id: &str) -> Result<String, TimerClientError> {
    let hive_id = hive_id.trim();
    if hive_id.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "hive_id must be non-empty".to_string(),
        ));
    }
    Ok(format!("{TIMER_NODE_KIND}@{hive_id}"))
}

pub fn build_timer_system_request(
    src_uuid: &str,
    hive_id: &str,
    verb: &str,
    payload: Value,
) -> Result<Message, TimerClientError> {
    build_timer_system_request_with_target(src_uuid, &timer_node_name(hive_id)?, verb, payload)
}

pub fn build_timer_system_request_with_target(
    src_uuid: &str,
    target_node: &str,
    verb: &str,
    payload: Value,
) -> Result<Message, TimerClientError> {
    let src_uuid = src_uuid.trim();
    if src_uuid.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "src_uuid must be non-empty".to_string(),
        ));
    }
    let target_node = target_node.trim();
    if target_node.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "target_node must be non-empty".to_string(),
        ));
    }
    let verb = verb.trim();
    if verb.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "verb must be non-empty".to_string(),
        ));
    }
    let trace_id = Uuid::new_v4().to_string();
    let mut message = build_system_message(
        src_uuid,
        Destination::Unicast(target_node.to_string()),
        1,
        &trace_id,
        verb,
        payload,
    );
    message.meta.action = Some(verb.to_string());
    Ok(message)
}

pub fn is_timer_response_message(msg: &Message) -> bool {
    msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some(MSG_TIMER_RESPONSE)
}

pub fn parse_timer_response(msg: &Message) -> Result<TimerResponse, TimerClientError> {
    if !is_timer_response_message(msg) {
        return Err(TimerClientError::InvalidResponse(
            "message is not TIMER_RESPONSE".to_string(),
        ));
    }
    Ok(serde_json::from_value(msg.payload.clone())?)
}

pub fn parse_timer_get_response(msg: &Message) -> Result<TimerGetResponse, TimerClientError> {
    if !is_timer_response_message(msg) {
        return Err(TimerClientError::InvalidResponse(
            "message is not TIMER_RESPONSE".to_string(),
        ));
    }
    Ok(serde_json::from_value(msg.payload.clone())?)
}

pub fn parse_timer_list_response(msg: &Message) -> Result<TimerListResponse, TimerClientError> {
    if !is_timer_response_message(msg) {
        return Err(TimerClientError::InvalidResponse(
            "message is not TIMER_RESPONSE".to_string(),
        ));
    }
    Ok(serde_json::from_value(msg.payload.clone())?)
}

pub fn parse_timer_help_response(msg: &Message) -> Result<TimerHelpDescriptor, TimerClientError> {
    if !is_timer_response_message(msg) {
        return Err(TimerClientError::InvalidResponse(
            "message is not TIMER_RESPONSE".to_string(),
        ));
    }
    Ok(serde_json::from_value(msg.payload.clone())?)
}

pub fn parse_timer_fired_event(msg: &Message) -> Result<FiredEvent, TimerClientError> {
    if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_TIMER_FIRED) {
        return Err(TimerClientError::InvalidResponse(
            "message is not TIMER_FIRED".to_string(),
        ));
    }
    Ok(serde_json::from_value(msg.payload.clone())?)
}

pub fn timer_response_service_error(response: &TimerResponse) -> Option<TimerClientError> {
    if response.ok {
        return None;
    }
    if let Some(error) = &response.error {
        return Some(TimerClientError::ServiceError {
            verb: response.verb.clone().unwrap_or_default(),
            code: error.code.clone(),
            message: error.message.clone(),
        });
    }
    Some(TimerClientError::ContractViolation(
        "timer response marked not ok without error detail".to_string(),
    ))
}

pub fn map_timer_transport_message(msg: &Message) -> Option<TimerClientError> {
    if msg.meta.msg_type != SYSTEM_KIND {
        return None;
    }
    match msg.meta.msg.as_deref() {
        Some(MSG_UNREACHABLE) => {
            let payload = serde_json::from_value::<UnreachablePayload>(msg.payload.clone()).ok();
            Some(TimerClientError::Unreachable {
                reason: payload
                    .as_ref()
                    .map(|value| value.reason.clone())
                    .unwrap_or_default(),
                original_dst: payload
                    .as_ref()
                    .map(|value| value.original_dst.clone())
                    .unwrap_or_default(),
            })
        }
        Some(MSG_TTL_EXCEEDED) => {
            let payload = serde_json::from_value::<TtlExceededPayload>(msg.payload.clone()).ok();
            Some(TimerClientError::TtlExceeded {
                original_dst: payload
                    .as_ref()
                    .map(|value| value.original_dst.clone())
                    .unwrap_or_default(),
                last_hop: payload
                    .as_ref()
                    .map(|value| value.last_hop.clone())
                    .unwrap_or_default(),
            })
        }
        _ => None,
    }
}

fn local_timer_node_name(full_name: &str) -> Result<String, TimerClientError> {
    let full_name = full_name.trim();
    if full_name.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "cannot derive local timer node name from empty sender full name".to_string(),
        ));
    }
    let (_, hive_id) = full_name.split_once('@').ok_or_else(|| {
        TimerClientError::InvalidRequest("sender full name must include hive suffix".to_string())
    })?;
    timer_node_name(hive_id)
}

fn default_rpc_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        Duration::from_millis(TIMER_DEFAULT_RPC_TIMEOUT_MS)
    } else {
        timeout
    }
}

fn normalized_time_retry_schedule(schedule: Vec<Duration>) -> Vec<Duration> {
    let normalized: Vec<_> = schedule
        .into_iter()
        .filter(|timeout| !timeout.is_zero())
        .collect();
    if normalized.is_empty() {
        TIMER_DEFAULT_TIME_RETRY_SCHEDULE_MS
            .into_iter()
            .map(Duration::from_millis)
            .collect()
    } else {
        normalized
    }
}

fn should_retry_time_operation(error: &TimerClientError) -> bool {
    matches!(
        error,
        TimerClientError::Timeout { .. }
            | TimerClientError::Unreachable { .. }
            | TimerClientError::TtlExceeded { .. }
    )
}

fn trim_owned(value: String) -> String {
    value.trim().to_string()
}

fn normalize_schedule_payload(payload: &mut TimerSchedulePayload) {
    payload.target_l2_name = payload
        .target_l2_name
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
    payload.client_ref = payload
        .client_ref
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
}

fn normalize_recurring_payload(payload: &mut TimerScheduleRecurringPayload) {
    payload.cron_spec = trim_owned(payload.cron_spec.clone());
    payload.cron_tz = payload
        .cron_tz
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
    payload.target_l2_name = payload
        .target_l2_name
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
    payload.client_ref = payload
        .client_ref
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
}

fn normalize_reschedule_payload(payload: &mut TimerReschedulePayload) {
    payload.timer_uuid = payload
        .timer_uuid
        .take()
        .map(|timer_id| TimerId(trim_owned(timer_id.0)))
        .filter(|timer_id| !timer_id.is_empty());
    payload.client_ref = payload
        .client_ref
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
}

fn normalize_list_filter(filter: &mut TimerListFilter) {
    filter.owner_l2_name = filter
        .owner_l2_name
        .take()
        .map(trim_owned)
        .filter(|value| !value.is_empty());
}

fn validate_schedule_payload(
    payload: &TimerSchedulePayload,
    verb: &str,
) -> Result<(), TimerClientError> {
    let has_absolute = payload.fire_at_utc_ms.is_some();
    let has_relative = payload.fire_in_ms.is_some();
    match (has_absolute, has_relative) {
        (true, true) | (false, false) => {
            return Err(TimerClientError::InvalidRequest(
                "exactly one of fire_at_utc_ms or fire_in_ms must be set".to_string(),
            ))
        }
        _ => {}
    }
    if let Some(target_l2_name) = payload.target_l2_name.as_deref() {
        validate_canonical_l2_name("target_l2_name", target_l2_name)?;
    }
    validate_missed_policy(verb, payload.missed_policy, payload.missed_within_ms)?;
    if let Some(fire_at_utc_ms) = payload.fire_at_utc_ms {
        validate_absolute_schedule_time(verb, fire_at_utc_ms)?;
    }
    if let Some(fire_in_ms) = payload.fire_in_ms {
        validate_relative_schedule_duration(verb, fire_in_ms)?;
    }
    Ok(())
}

fn validate_recurring_payload(
    payload: &TimerScheduleRecurringPayload,
) -> Result<(), TimerClientError> {
    if payload.cron_spec.trim().is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "cron_spec must be non-empty".to_string(),
        ));
    }
    if payload.cron_spec.split_whitespace().count() != 5 {
        return Err(service_error(
            MSG_TIMER_SCHEDULE_RECURRING,
            "TIMER_INVALID_CRON",
            "cron_spec must be a 5-field cron expression",
        ));
    }
    if let Some(target_l2_name) = payload.target_l2_name.as_deref() {
        validate_canonical_l2_name("target_l2_name", target_l2_name)?;
    }
    validate_missed_policy(
        MSG_TIMER_SCHEDULE_RECURRING,
        payload.missed_policy,
        payload.missed_within_ms,
    )?;
    Ok(())
}

fn validate_reschedule_payload(payload: &TimerReschedulePayload) -> Result<(), TimerClientError> {
    validate_timer_selector(
        payload.timer_uuid.clone(),
        payload.client_ref.clone(),
        "timer_uuid",
        "client_ref",
    )?;
    let has_absolute = payload.new_fire_at_utc_ms.is_some();
    let has_relative = payload.new_fire_in_ms.is_some();
    match (has_absolute, has_relative) {
        (true, true) | (false, false) => {
            return Err(TimerClientError::InvalidRequest(
                "exactly one of new_fire_at_utc_ms or new_fire_in_ms must be set".to_string(),
            ))
        }
        _ => {}
    }
    if let Some(fire_at_utc_ms) = payload.new_fire_at_utc_ms {
        validate_absolute_schedule_time(MSG_TIMER_RESCHEDULE, fire_at_utc_ms)?;
    }
    if let Some(fire_in_ms) = payload.new_fire_in_ms {
        validate_relative_schedule_duration(MSG_TIMER_RESCHEDULE, fire_in_ms)?;
    }
    Ok(())
}

fn validate_list_filter(filter: &TimerListFilter) -> Result<(), TimerClientError> {
    if let Some(owner_l2_name) = filter.owner_l2_name.as_deref() {
        validate_canonical_l2_name("owner_l2_name", owner_l2_name)?;
    }
    if let Some(limit) = filter.limit {
        if limit == 0 || limit > TIMER_LIST_MAX_LIMIT {
            return Err(TimerClientError::InvalidRequest(format!(
                "limit must be between 1 and {}",
                TIMER_LIST_MAX_LIMIT
            )));
        }
    }
    Ok(())
}

fn validate_timer_id(timer_id: TimerId) -> Result<TimerId, TimerClientError> {
    let timer_id = TimerId(trim_owned(timer_id.0));
    if timer_id.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "timer_uuid must be non-empty".to_string(),
        ));
    }
    Ok(timer_id)
}

fn validate_client_ref(client_ref: String) -> Result<String, TimerClientError> {
    let client_ref = trim_owned(client_ref);
    if client_ref.is_empty() {
        return Err(TimerClientError::InvalidRequest(
            "client_ref must be non-empty".to_string(),
        ));
    }
    if client_ref.len() > 255 {
        return Err(TimerClientError::InvalidRequest(
            "client_ref must be at most 255 characters".to_string(),
        ));
    }
    Ok(client_ref)
}

fn validate_timer_selector(
    timer_uuid: Option<TimerId>,
    client_ref: Option<String>,
    timer_uuid_field: &str,
    client_ref_field: &str,
) -> Result<(), TimerClientError> {
    if let Some(timer_id) = timer_uuid {
        validate_timer_id(timer_id)?;
        return Ok(());
    }
    if let Some(client_ref) = client_ref {
        validate_client_ref(client_ref)?;
        return Ok(());
    }
    Err(TimerClientError::InvalidRequest(format!(
        "either {timer_uuid_field} or {client_ref_field} must be set"
    )))
}

fn validate_canonical_l2_name(field: &str, value: &str) -> Result<(), TimerClientError> {
    let value = value.trim();
    let Some((node, hive)) = value.split_once('@') else {
        return Err(TimerClientError::InvalidRequest(format!(
            "{field} must be a canonical L2 name"
        )));
    };
    if node.trim().is_empty() || hive.trim().is_empty() {
        return Err(TimerClientError::InvalidRequest(format!(
            "{field} must be a canonical L2 name"
        )));
    }
    Ok(())
}

fn validate_missed_policy(
    verb: &str,
    missed_policy: Option<MissedPolicy>,
    missed_within_ms: Option<u64>,
) -> Result<(), TimerClientError> {
    match missed_policy.unwrap_or_default() {
        MissedPolicy::Fire | MissedPolicy::Drop => {
            if missed_within_ms.is_some() {
                return Err(TimerClientError::InvalidRequest(
                    "missed_within_ms is only valid with missed_policy=fire_if_within".to_string(),
                ));
            }
        }
        MissedPolicy::FireIfWithin => {
            if missed_within_ms.unwrap_or_default() == 0 {
                return Err(TimerClientError::InvalidRequest(
                    "missed_within_ms must be set when missed_policy=fire_if_within".to_string(),
                ));
            }
        }
    }
    if verb == MSG_TIMER_SCHEDULE || verb == MSG_TIMER_SCHEDULE_RECURRING {
        return Ok(());
    }
    Ok(())
}

fn validate_absolute_schedule_time(
    verb: &str,
    fire_at_utc_ms: i64,
) -> Result<(), TimerClientError> {
    let min_fire_at = now_epoch_ms() + TIMER_MIN_DURATION_MS as i64;
    if fire_at_utc_ms < min_fire_at {
        return Err(service_error(
            verb,
            "TIMER_BELOW_MINIMUM",
            &format!(
                "minimum timer duration is {} seconds",
                TIMER_MIN_DURATION_MS / 1000
            ),
        ));
    }
    Ok(())
}

fn validate_relative_schedule_duration(
    verb: &str,
    fire_in_ms: u64,
) -> Result<(), TimerClientError> {
    if fire_in_ms < TIMER_MIN_DURATION_MS {
        return Err(service_error(
            verb,
            "TIMER_BELOW_MINIMUM",
            &format!(
                "minimum timer duration is {} seconds",
                TIMER_MIN_DURATION_MS / 1000
            ),
        ));
    }
    Ok(())
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn service_error(verb: &str, code: &str, message: &str) -> TimerClientError {
    TimerClientError::ServiceError {
        verb: verb.to_string(),
        code: code.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Meta, Routing};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn timer_id_serializes_transparently() {
        let encoded = serde_json::to_string(&TimerId::from("timer-1")).expect("serialize timer id");
        assert_eq!(encoded, "\"timer-1\"");
    }

    #[test]
    fn timer_status_filter_serializes_as_wire_values() {
        let encoded =
            serde_json::to_string(&TimerStatusFilter::Pending).expect("serialize status filter");
        assert_eq!(encoded, "\"pending\"");
    }

    #[test]
    fn timer_default_time_retry_schedule_matches_go_semantics() {
        let schedule = normalized_time_retry_schedule(vec![]);
        assert_eq!(
            schedule,
            vec![
                Duration::from_millis(100),
                Duration::from_millis(300),
                Duration::from_millis(1_000),
            ]
        );
    }

    #[test]
    fn timer_response_deserializes_error_detail() {
        let raw = serde_json::json!({
            "ok": false,
            "verb": "TIMER_CANCEL",
            "error": {
                "code": "TIMER_NOT_FOUND",
                "message": "timer does not exist"
            }
        });
        let decoded: TimerResponse = serde_json::from_value(raw).expect("decode timer response");
        assert!(!decoded.ok);
        assert_eq!(decoded.verb.as_deref(), Some("TIMER_CANCEL"));
        assert_eq!(
            decoded.error.as_ref().map(|err| err.code.as_str()),
            Some("TIMER_NOT_FOUND")
        );
    }

    #[test]
    fn timer_node_name_uses_canonical_l2() {
        assert_eq!(
            timer_node_name("motherbee").expect("timer node name"),
            "SY.timer@motherbee"
        );
    }

    #[test]
    fn build_timer_system_request_uses_system_envelope() {
        let msg = build_timer_system_request("src-uuid", "motherbee", MSG_TIMER_NOW, json!({}))
            .expect("build timer request");
        assert_eq!(msg.routing.src, "src-uuid");
        assert!(
            matches!(msg.routing.dst, Destination::Unicast(ref dst) if dst == "SY.timer@motherbee")
        );
        assert_eq!(msg.routing.ttl, 1);
        assert_eq!(msg.meta.msg_type, SYSTEM_KIND);
        assert_eq!(msg.meta.msg.as_deref(), Some(MSG_TIMER_NOW));
        assert_eq!(msg.meta.action.as_deref(), Some(MSG_TIMER_NOW));
    }

    #[test]
    fn parse_timer_fired_event_rejects_non_timer_fired() {
        let message = Message {
            routing: Routing {
                src: "src".to_string(),
                src_l2_name: None,
                dst: Destination::Broadcast,
                ttl: 1,
                trace_id: "trace".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_TIMER_RESPONSE.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        };
        let err = parse_timer_fired_event(&message).expect_err("should reject non TIMER_FIRED");
        assert!(matches!(err, TimerClientError::InvalidResponse(_)));
    }

    #[test]
    fn timer_client_new_derives_local_target() {
        let (sender, mut receiver, _) = TimerTestHarness::new("WF.demo@motherbee");
        let client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");
        assert_eq!(client.target(), "SY.timer@motherbee");
    }

    #[tokio::test]
    async fn timer_client_now_retries_after_unreachable() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(
            &sender,
            &mut receiver,
            TimerClientConfig {
                time_retry_schedule: vec![Duration::from_millis(10), Duration::from_millis(20)],
                ..TimerClientConfig::default()
            },
        )
        .expect("new timer client");

        tokio::spawn(async move {
            let first = harness.next_request().await.expect("first request");
            harness
                .send_unreachable(&first, "temporary")
                .await
                .expect("send unreachable");

            let second = harness.next_request().await.expect("second request");
            harness
                .send_timer_response(
                    &second,
                    json!({
                        "ok": true,
                        "verb": "TIMER_NOW",
                        "now_utc_ms": 1775577600000_i64,
                        "now_utc_iso": "2026-04-06T12:00:00Z"
                    }),
                )
                .await
                .expect("send timer response");
        });

        let result = client.now().await.expect("timer now");
        assert_eq!(result.now_utc_ms, 1_775_577_600_000);
        assert_eq!(result.now_utc_iso, "2026-04-06T12:00:00Z");
    }

    #[tokio::test]
    async fn timer_client_schedule_in_rejects_below_minimum() {
        let (sender, mut receiver, _) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        let err = client
            .schedule_in(Duration::from_secs(30), TimerSchedulePayload::default())
            .await
            .expect_err("below minimum should fail");
        assert!(matches!(
            err,
            TimerClientError::ServiceError { ref code, .. } if code == "TIMER_BELOW_MINIMUM"
        ));
    }

    #[tokio::test]
    async fn timer_client_get_parses_timer_info() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        tokio::spawn(async move {
            let request = harness.next_request().await.expect("request");
            harness
                .send_timer_response(
                    &request,
                    json!({
                        "ok": true,
                        "verb": "TIMER_GET",
                        "timer": {
                            "uuid": "timer-1",
                            "owner_l2_name": "WF.demo@motherbee",
                            "target_l2_name": "WF.demo@motherbee",
                            "kind": "oneshot",
                            "fire_at_utc_ms": 1775577600000_i64,
                            "missed_policy": "fire",
                            "status": "pending",
                            "created_at_utc_ms": 1775574000000_i64,
                            "fire_count": 0_u64
                        }
                    }),
                )
                .await
                .expect("send timer response");
        });

        let timer = client.get("timer-1").await.expect("get timer");
        assert_eq!(timer.uuid.as_str(), "timer-1");
        assert_eq!(timer.owner_l2_name, "WF.demo@motherbee");
    }

    #[tokio::test]
    async fn timer_client_list_mine_injects_sender_owner() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        tokio::spawn(async move {
            let request = harness.next_request().await.expect("request");
            assert_eq!(
                request.payload.get("owner_l2_name").and_then(Value::as_str),
                Some("WF.demo@motherbee")
            );
            harness
                .send_timer_response(
                    &request,
                    json!({
                        "ok": true,
                        "verb": "TIMER_LIST",
                        "count": 1_u64,
                        "timers": [{
                            "uuid": "timer-1",
                            "owner_l2_name": "WF.demo@motherbee",
                            "target_l2_name": "WF.demo@motherbee",
                            "kind": "oneshot",
                            "fire_at_utc_ms": 1775577600000_i64,
                            "missed_policy": "fire",
                            "status": "pending",
                            "created_at_utc_ms": 1775574000000_i64,
                            "fire_count": 0_u64
                        }]
                    }),
                )
                .await
                .expect("send timer list");
        });

        let response = client
            .list_mine(TimerListFilter {
                status_filter: Some(TimerStatusFilter::Pending),
                limit: Some(100),
                ..TimerListFilter::default()
            })
            .await
            .expect("list mine");
        assert_eq!(response.count, 1);
        assert_eq!(response.timers[0].uuid.as_str(), "timer-1");
    }

    #[test]
    fn timer_schedule_request_matches_go_fixture() {
        let fixture = read_fixture_message("request_schedule_in.json");
        let payload: TimerSchedulePayload =
            serde_json::from_value(fixture.payload.clone()).expect("decode schedule payload");
        let mut request = build_timer_system_request_with_target(
            "src-uuid-1",
            "SY.timer@motherbee",
            MSG_TIMER_SCHEDULE,
            serde_json::to_value(payload).expect("re-encode schedule payload"),
        )
        .expect("build timer request");
        request.routing.trace_id = "trace-schedule-1".to_string();
        assert_eq!(
            serde_json::to_value(request).expect("encode request"),
            read_fixture_value("request_schedule_in.json")
        );
    }

    #[test]
    fn timer_list_response_matches_go_fixture() {
        let fixture = read_fixture_message("response_timer_list_ok.json");
        let response = parse_timer_list_response(&fixture).expect("parse list response");
        assert!(response.ok);
        assert_eq!(response.verb, "TIMER_LIST");
        assert_eq!(response.count, 1);
        assert_eq!(response.timers[0].uuid.as_str(), "timer-1");
        assert_eq!(response.timers[0].status, TimerStatus::Pending);
    }

    #[test]
    fn timer_error_response_matches_go_fixture() {
        let fixture = read_fixture_message("response_timer_error.json");
        let response = parse_timer_response(&fixture).expect("parse timer response");
        let err = timer_response_service_error(&response).expect("service error");
        assert!(matches!(
            err,
            TimerClientError::ServiceError { ref verb, ref code, ref message }
            if verb == "TIMER_CANCEL" && code == "TIMER_NOT_FOUND" && message == "timer does not exist"
        ));
    }

    #[test]
    fn timer_fired_event_matches_go_fixture() {
        let fixture = read_fixture_message("event_timer_fired.json");
        let event = parse_timer_fired_event(&fixture).expect("parse fired event");
        assert_eq!(event.timer_uuid.as_str(), "timer-1");
        assert_eq!(event.kind, TimerKind::Oneshot);
        assert!(event.is_last_fire);
    }

    #[tokio::test]
    async fn timer_test_harness_drives_timer_client_without_hive() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        tokio::spawn(async move {
            let request = harness.next_request().await.expect("request");
            harness
                .send_timer_response(
                    &request,
                    json!({
                        "ok": true,
                        "verb": "TIMER_NOW",
                        "now_utc_ms": 1775577600000_i64,
                        "now_utc_iso": "2026-04-06T12:00:00Z"
                    }),
                )
                .await
                .expect("send response");
        });

        let result = client.now().await.expect("timer now");
        assert_eq!(result.now_utc_iso, "2026-04-06T12:00:00Z");
    }

    #[tokio::test]
    async fn timer_client_get_by_client_ref_uses_client_ref_payload() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        tokio::spawn(async move {
            let request = harness.next_request().await.expect("request");
            assert_eq!(request.meta.msg.as_deref(), Some(MSG_TIMER_GET));
            assert_eq!(
                request.payload.get("client_ref").and_then(Value::as_str),
                Some("wf:demo::timeout")
            );
            harness
                .send_timer_response(
                    &request,
                    json!({
                        "ok": true,
                        "verb": "TIMER_GET",
                        "timer": {
                            "uuid": "timer-1",
                            "owner_l2_name": "WF.demo@motherbee",
                            "target_l2_name": "WF.demo@motherbee",
                            "kind": "oneshot",
                            "fire_at_utc_ms": 1775577600000_i64,
                            "missed_policy": "fire",
                            "status": "pending",
                            "created_at_utc_ms": 1775574000000_i64,
                            "fire_count": 0_u64,
                            "payload": {}
                        }
                    }),
                )
                .await
                .expect("send response");
        });

        let timer = client
            .get_by_client_ref("wf:demo::timeout")
            .await
            .expect("get by client_ref");
        assert_eq!(timer.uuid.as_str(), "timer-1");
    }

    #[tokio::test]
    async fn timer_client_cancel_by_client_ref_uses_client_ref_payload() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        tokio::spawn(async move {
            let request = harness.next_request().await.expect("request");
            assert_eq!(request.meta.msg.as_deref(), Some(MSG_TIMER_CANCEL));
            assert_eq!(
                request.payload.get("client_ref").and_then(Value::as_str),
                Some("wf:demo::timeout")
            );
            harness
                .send_timer_response(
                    &request,
                    json!({
                        "ok": true,
                        "verb": "TIMER_CANCEL",
                        "found": true
                    }),
                )
                .await
                .expect("send response");
        });

        let response = client
            .cancel_by_client_ref("wf:demo::timeout")
            .await
            .expect("cancel by client_ref");
        assert_eq!(response.found, Some(true));
    }

    #[tokio::test]
    async fn timer_client_reschedule_by_client_ref_uses_client_ref_payload() {
        let (sender, mut receiver, mut harness) = TimerTestHarness::new("WF.demo@motherbee");
        let mut client = TimerClient::new(&sender, &mut receiver, TimerClientConfig::default())
            .expect("new timer client");

        tokio::spawn(async move {
            let request = harness.next_request().await.expect("request");
            assert_eq!(request.meta.msg.as_deref(), Some(MSG_TIMER_RESCHEDULE));
            assert_eq!(
                request.payload.get("client_ref").and_then(Value::as_str),
                Some("wf:demo::timeout")
            );
            harness
                .send_timer_response(
                    &request,
                    json!({
                        "ok": true,
                        "verb": "TIMER_RESCHEDULE",
                        "timer_uuid": "timer-1",
                        "fire_at_utc_ms": 1775577600000_i64,
                        "found": true
                    }),
                )
                .await
                .expect("send response");
        });

        let response = client
            .reschedule_by_client_ref("wf:demo::timeout", now_epoch_ms() + 120_000)
            .await
            .expect("reschedule by client_ref");
        assert_eq!(response.found, Some(true));
    }

    fn read_fixture_message(name: &str) -> Message {
        serde_json::from_value(read_fixture_value(name)).expect("decode fixture message")
    }

    fn read_fixture_value(name: &str) -> Value {
        let raw = std::fs::read_to_string(fixture_path(name)).expect("read fixture");
        serde_json::from_str(&raw).expect("decode fixture json")
    }

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../go/fluxbee-go-sdk/testdata/timer_wire")
            .join(name)
    }
}
