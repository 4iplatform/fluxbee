use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "type")]
    pub kind: String,
    pub msg: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message<T = Value> {
    #[serde(default)]
    pub routing: Value,
    pub meta: Meta,
    pub payload: T,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AnnouncePayload {
    pub uuid: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WithdrawPayload {
    pub uuid: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct QueryPayload {}

#[derive(Debug, Serialize, Deserialize)]
pub struct HelloPayload {
    pub timestamp: String,
    pub seq: u64,
    pub protocol: String,
    pub router_id: String,
    pub island_id: String,
    pub shm_name: String,
    pub capabilities: HelloCapabilities,
    pub timers: HelloTimers,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeDescriptor {
    pub uuid: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloCapabilities {
    pub sync: bool,
    pub lsa: bool,
    pub forwarding: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloTimers {
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LsaPayload {
    pub router_id: String,
    pub nodes: Vec<NodeDescriptor>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncRequestPayload {
    pub router_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncReplyPayload {
    pub router_id: String,
    pub nodes: Vec<NodeDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfigSyncRoutePayload {
    pub prefix: String,
    pub match_kind: String,
    pub action: String,
    pub next_hop_island: String,
    pub metric: u32,
    pub priority: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfigSyncVpnPayload {
    pub vpn_id: u32,
    pub vpn_name: String,
    pub remote_island: String,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigSyncPayload {
    pub timestamp: String,
    pub config_version: u64,
    pub source_island: String,
    pub routes: Vec<ConfigSyncRoutePayload>,
    pub vpns: Vec<ConfigSyncVpnPayload>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UplinkAcceptPayload {
    pub timestamp: String,
    pub peer_router_id: String,
    pub negotiated: UplinkNegotiated,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UplinkNegotiated {
    pub protocol: String,
    pub hello_interval_ms: u64,
    pub dead_interval_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UplinkRejectPayload {
    pub timestamp: String,
    pub reason: String,
    pub message: String,
    pub min_version: Option<String>,
}

pub const SYSTEM_KIND: &str = "system";
pub const MSG_QUERY: &str = "QUERY";
pub const MSG_ANNOUNCE: &str = "ANNOUNCE";
pub const MSG_WITHDRAW: &str = "WITHDRAW";
pub const MSG_UNREACHABLE: &str = "UNREACHABLE";
pub const MSG_TTL_EXCEEDED: &str = "TTL_EXCEEDED";
pub const MSG_HELLO: &str = "HELLO";
pub const MSG_LSA: &str = "LSA";
pub const MSG_UPLINK_ACCEPT: &str = "UPLINK_ACCEPT";
pub const MSG_UPLINK_REJECT: &str = "UPLINK_REJECT";
pub const MSG_SYNC_REQUEST: &str = "SYNC_REQUEST";
pub const MSG_SYNC_REPLY: &str = "SYNC_REPLY";
pub const MSG_CONFIG_SYNC: &str = "CONFIG_SYNC";

pub fn build_query() -> Message<QueryPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_QUERY.to_string(),
        },
        payload: QueryPayload::default(),
    }
}

pub fn build_hello(
    router_uuid: Uuid,
    island_id: &str,
    shm_name: &str,
    seq: u64,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
    capabilities: HelloCapabilities,
) -> Message<HelloPayload> {
    let routing = json!({
        "src": router_uuid.to_string(),
        "dst": null,
        "ttl": 16,
        "trace_id": Uuid::new_v4().to_string(),
    });
    Message {
        routing,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_HELLO.to_string(),
        },
        payload: HelloPayload {
            timestamp: chrono::Utc::now().to_rfc3339(),
            seq,
            protocol: "json-router/1".to_string(),
            router_id: router_uuid.to_string(),
            island_id: island_id.to_string(),
            shm_name: shm_name.to_string(),
            capabilities,
            timers: HelloTimers {
                hello_interval_ms,
                dead_interval_ms,
            },
        },
    }
}

pub fn build_lsa(router_uuid: Uuid, nodes: Vec<NodeDescriptor>) -> Message<LsaPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_LSA.to_string(),
        },
        payload: LsaPayload {
            router_id: router_uuid.to_string(),
            nodes,
        },
    }
}

pub fn build_sync_request(router_uuid: Uuid) -> Message<SyncRequestPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_SYNC_REQUEST.to_string(),
        },
        payload: SyncRequestPayload {
            router_id: router_uuid.to_string(),
        },
    }
}

pub fn build_sync_reply(router_uuid: Uuid, nodes: Vec<NodeDescriptor>) -> Message<SyncReplyPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_SYNC_REPLY.to_string(),
        },
        payload: SyncReplyPayload {
            router_id: router_uuid.to_string(),
            nodes,
        },
    }
}

pub fn build_config_sync(
    source_island: &str,
    config_version: u64,
    routes: Vec<ConfigSyncRoutePayload>,
    vpns: Vec<ConfigSyncVpnPayload>,
) -> Message<ConfigSyncPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_CONFIG_SYNC.to_string(),
        },
        payload: ConfigSyncPayload {
            timestamp: chrono::Utc::now().to_rfc3339(),
            config_version,
            source_island: source_island.to_string(),
            routes,
            vpns,
        },
    }
}

pub fn build_uplink_accept(
    peer_router_id: &str,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
) -> Message<UplinkAcceptPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_UPLINK_ACCEPT.to_string(),
        },
        payload: UplinkAcceptPayload {
            timestamp: chrono::Utc::now().to_rfc3339(),
            peer_router_id: peer_router_id.to_string(),
            negotiated: UplinkNegotiated {
                protocol: "json-router/1".to_string(),
                hello_interval_ms,
                dead_interval_ms,
            },
        },
    }
}

pub fn build_uplink_reject(
    reason: &str,
    message: &str,
    min_version: Option<&str>,
) -> Message<UplinkRejectPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_UPLINK_REJECT.to_string(),
        },
        payload: UplinkRejectPayload {
            timestamp: chrono::Utc::now().to_rfc3339(),
            reason: reason.to_string(),
            message: message.to_string(),
            min_version: min_version.map(|s| s.to_string()),
        },
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub reason: String,
    pub detail: String,
}

pub fn build_error(msg: &str, reason: &str, detail: &str, src: &str) -> Message<ErrorPayload> {
    let routing = serde_json::json!({
        "src": null,
        "dst": src,
        "ttl": 1,
        "trace_id": Uuid::new_v4().to_string(),
    });
    Message {
        routing,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: msg.to_string(),
        },
        payload: ErrorPayload {
            reason: reason.to_string(),
            detail: detail.to_string(),
        },
    }
}
