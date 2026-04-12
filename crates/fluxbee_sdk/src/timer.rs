use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::protocol::{
    build_system_message, Destination, Message, TtlExceededPayload,
    UnreachablePayload, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::NodeError;

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
    pub timer_uuid: TimerId,
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
    pub timer_uuid: TimerId,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TimerReschedulePayload {
    pub timer_uuid: TimerId,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Meta, Routing};
    use serde_json::json;

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
        assert!(matches!(msg.routing.dst, Destination::Unicast(ref dst) if dst == "SY.timer@motherbee"));
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
}
