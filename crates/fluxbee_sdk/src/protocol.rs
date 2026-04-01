use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub routing: Routing,
    pub meta: Meta,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub src: String,
    #[serde(deserialize_with = "deserialize_dst")]
    pub dst: Destination,
    pub ttl: u8,
    pub trace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Destination {
    Unicast(String),
    Broadcast,
    Resolve,
}

impl Serialize for Destination {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Destination::Unicast(value) => serializer.serialize_str(value),
            Destination::Broadcast => serializer.serialize_str("broadcast"),
            Destination::Resolve => serializer.serialize_none(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Meta {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_ilk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_ilk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ich: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_window: Option<Vec<CtxTurn>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_package: Option<MemoryPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPackage {
    pub package_version: u32,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context: Option<MemoryContextSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason: Option<MemoryReasonSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<MemoryContextSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<MemoryReasonSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<MemorySummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub episodes: Vec<EpisodeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<MemoryPackageTruncated>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryContextSummary {
    pub context_id: String,
    pub label: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReasonSummary {
    pub reason_id: String,
    pub label: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySummary {
    pub memory_id: String,
    pub summary: String,
    pub weight: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeSummary {
    pub episode_id: String,
    pub title: String,
    pub intensity: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPackageTruncated {
    pub applied: bool,
    pub dropped_contexts: u32,
    pub dropped_reasons: u32,
    pub dropped_memories: u32,
    pub dropped_episodes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtxTurn {
    pub seq: u64,
    pub ts: String,
    pub from: String,
    #[serde(rename = "type")]
    pub turn_type: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeHelloPayload {
    pub uuid: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterHelloPayload {
    pub router_id: String,
    pub router_name: String,
    pub shm_name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanHelloPayload {
    pub protocol: String,
    pub router_id: String,
    pub router_name: String,
    pub hive_id: String,
    pub capabilities: Vec<String>,
    pub timers: WanTimers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanTimers {
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanAcceptPayload {
    pub peer_router_id: String,
    pub negotiated: WanNegotiated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanNegotiated {
    pub protocol: String,
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WanRejectPayload {
    pub reason: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeAnnouncePayload {
    pub uuid: String,
    pub name: String,
    pub status: String,
    pub vpn_id: u32,
    pub router_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreachablePayload {
    pub original_dst: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtlExceededPayload {
    pub original_dst: String,
    pub last_hop: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EchoPayload {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EchoReplyPayload {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSyncPayload {
    pub timestamp_utc: String,
    pub epoch_ms: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawPayload {
    pub uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangedPayload {
    pub subsystem: String,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub auto_apply: Option<bool>,
    pub version: u64,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaPayload {
    pub hive: String,
    #[serde(default)]
    pub router_id: String,
    #[serde(default)]
    pub router_name: String,
    pub seq: u64,
    pub timestamp: String,
    pub nodes: Vec<LsaNode>,
    pub routes: Vec<LsaRoute>,
    pub vpns: Vec<LsaVpn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaNode {
    pub uuid: String,
    pub name: String,
    pub vpn_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaRoute {
    pub prefix: String,
    pub match_kind: String,
    pub action: String,
    pub next_hop_hive: String,
    pub metric: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsaVpn {
    pub pattern: String,
    pub match_kind: String,
    pub vpn_id: u32,
}

pub const SYSTEM_KIND: &str = "system";

pub const MSG_HELLO: &str = "HELLO";
pub const MSG_ANNOUNCE: &str = "ANNOUNCE";
pub const MSG_UNREACHABLE: &str = "UNREACHABLE";
pub const MSG_TTL_EXCEEDED: &str = "TTL_EXCEEDED";
pub const MSG_ECHO: &str = "ECHO";
pub const MSG_ECHO_REPLY: &str = "ECHO_REPLY";
pub const MSG_LSA: &str = "LSA";
pub const MSG_WAN_ACCEPT: &str = "WAN_ACCEPT";
pub const MSG_WAN_REJECT: &str = "WAN_REJECT";
pub const MSG_TIME_SYNC: &str = "TIME_SYNC";
pub const MSG_WITHDRAW: &str = "WITHDRAW";
pub const MSG_CONFIG_CHANGED: &str = "CONFIG_CHANGED";
pub const MSG_CONFIG_GET: &str = "CONFIG_GET";
pub const MSG_CONFIG_SET: &str = "CONFIG_SET";
pub const MSG_CONFIG_RESPONSE: &str = "CONFIG_RESPONSE";
pub const MSG_OPA_RELOAD: &str = "OPA_RELOAD";
pub const MSG_NODE_STATUS_GET: &str = "NODE_STATUS_GET";
pub const MSG_NODE_STATUS_GET_RESPONSE: &str = "NODE_STATUS_GET_RESPONSE";

pub const SCOPE_VPN: &str = "vpn";
pub const SCOPE_GLOBAL: &str = "global";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaReloadPayload {
    pub version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

pub fn build_system_message(
    src: &str,
    dst: Destination,
    ttl: u8,
    trace_id: &str,
    msg: &str,
    payload: Value,
) -> Message {
    Message {
        routing: Routing {
            src: src.to_string(),
            dst,
            ttl,
            trace_id: trace_id.to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg.to_string()),
            src_ilk: None,
            dst_ilk: None,
            ich: None,
            thread_id: None,
            thread_seq: None,
            ctx: None,
            ctx_seq: None,
            ctx_window: None,
            memory_package: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload,
    }
}

pub fn build_hello(src: &str, trace_id: &str, payload: NodeHelloPayload) -> Message {
    build_system_message(
        src,
        Destination::Resolve,
        1,
        trace_id,
        MSG_HELLO,
        json!(payload),
    )
}

pub fn build_router_hello(src: &str, trace_id: &str, payload: RouterHelloPayload) -> Message {
    build_system_message(
        src,
        Destination::Resolve,
        1,
        trace_id,
        MSG_HELLO,
        json!(payload),
    )
}

pub fn build_wan_hello(src: &str, trace_id: &str, payload: WanHelloPayload) -> Message {
    build_system_message(
        src,
        Destination::Resolve,
        1,
        trace_id,
        MSG_HELLO,
        json!(payload),
    )
}

pub fn build_wan_accept(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: WanAcceptPayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_WAN_ACCEPT,
        json!(payload),
    )
}

pub fn build_wan_reject(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: WanRejectPayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_WAN_REJECT,
        json!(payload),
    )
}

pub fn build_announce(
    src: &str,
    dst: &str,
    trace_id: &str,
    payload: NodeAnnouncePayload,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_ANNOUNCE,
        json!(payload),
    )
}

pub fn build_unreachable(
    src: &str,
    dst: &str,
    trace_id: &str,
    original_dst: &str,
    reason: &str,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        16,
        trace_id,
        MSG_UNREACHABLE,
        json!(UnreachablePayload {
            original_dst: original_dst.to_string(),
            reason: reason.to_string(),
        }),
    )
}

pub fn build_ttl_exceeded(
    src: &str,
    dst: &str,
    trace_id: &str,
    original_dst: &str,
    last_hop: &str,
) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        16,
        trace_id,
        MSG_TTL_EXCEEDED,
        json!(TtlExceededPayload {
            original_dst: original_dst.to_string(),
            last_hop: last_hop.to_string(),
        }),
    )
}

pub fn build_lsa(src: &str, dst: &str, trace_id: &str, payload: LsaPayload) -> Message {
    build_system_message(
        src,
        Destination::Unicast(dst.to_string()),
        1,
        trace_id,
        MSG_LSA,
        json!(payload),
    )
}

pub fn build_echo(src: &str, dst: Destination, trace_id: &str) -> Message {
    build_system_message(src, dst, 1, trace_id, MSG_ECHO, json!(EchoPayload {}))
}

pub fn build_echo_reply(src: &str, dst: Destination, trace_id: &str) -> Message {
    build_system_message(
        src,
        dst,
        1,
        trace_id,
        MSG_ECHO_REPLY,
        json!(EchoReplyPayload {}),
    )
}

pub fn build_time_sync(
    src: &str,
    dst: Destination,
    trace_id: &str,
    payload: TimeSyncPayload,
) -> Message {
    let mut msg = build_system_message(src, dst, 1, trace_id, MSG_TIME_SYNC, json!(payload));
    msg.meta.scope = Some(SCOPE_GLOBAL.to_string());
    msg
}

pub fn build_withdraw(src: &str, dst: Destination, trace_id: &str, uuid: &str) -> Message {
    build_system_message(
        src,
        dst,
        1,
        trace_id,
        MSG_WITHDRAW,
        json!(WithdrawPayload {
            uuid: uuid.to_string(),
        }),
    )
}

fn deserialize_dst<'de, D>(deserializer: D) -> Result<Destination, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(Destination::Resolve),
        Value::String(s) if s == "broadcast" => Ok(Destination::Broadcast),
        Value::String(s) => Ok(Destination::Unicast(s)),
        _ => Err(serde::de::Error::custom("invalid dst")),
    }
}
