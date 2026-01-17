use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    pub router_id: String,
    pub island_id: String,
    pub shm_name: String,
}

pub const SYSTEM_KIND: &str = "system";
pub const MSG_QUERY: &str = "QUERY";
pub const MSG_ANNOUNCE: &str = "ANNOUNCE";
pub const MSG_WITHDRAW: &str = "WITHDRAW";
pub const MSG_UNREACHABLE: &str = "UNREACHABLE";
pub const MSG_TTL_EXCEEDED: &str = "TTL_EXCEEDED";
pub const MSG_HELLO: &str = "HELLO";

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

pub fn build_hello(router_uuid: Uuid, island_id: &str, shm_name: &str) -> Message<HelloPayload> {
    Message {
        routing: Value::Null,
        meta: Meta {
            kind: SYSTEM_KIND.to_string(),
            msg: MSG_HELLO.to_string(),
        },
        payload: HelloPayload {
            router_id: router_uuid.to_string(),
            island_id: island_id.to_string(),
            shm_name: shm_name.to_string(),
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
