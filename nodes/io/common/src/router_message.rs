#![forbid(unsafe_code)]

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use serde_json::{Map, Value};
use uuid::Uuid;

pub const DEFAULT_TTL: u32 = 16;

pub fn new_trace_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn build_user_message(
    src_node_uuid: &str,
    dst_node_uuid: Option<String>,
    ttl: u32,
    trace_id: String,
    src_ilk: Option<String>,
    dst_ilk: Option<String>,
    context: Value,
    payload: Value,
) -> Message {
    let mut context_obj: Map<String, Value> = match context {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("io".to_string(), other);
            map
        }
    };
    context_obj.insert(
        "dst_ilk".to_string(),
        dst_ilk.clone().map(Value::String).unwrap_or(Value::Null),
    );
    let thread_id = if context_obj.contains_key("thread_id") {
        context_obj
            .get("thread_id")
            .and_then(Value::as_str)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    } else {
        extract_thread_id_from_context_obj(&context_obj)
    };
    // Keep thread_id only in canonical meta.thread_id carrier.
    context_obj.remove("thread_id");

    Message {
        routing: Routing {
            src: src_node_uuid.to_string(),
            dst: dst_node_uuid
                .map(Destination::Unicast)
                .unwrap_or(Destination::Resolve),
            ttl: ttl.try_into().unwrap_or(u8::MAX),
            trace_id,
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            src_ilk,
            dst_ilk,
            thread_id,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: Some(Value::Object(context_obj)),
            ..Meta::default()
        },
        payload,
    }
}

fn extract_thread_id_from_context_obj(context_obj: &Map<String, Value>) -> Option<String> {
    context_obj
        .get("io")
        .and_then(|io| io.get("conversation"))
        .and_then(|conversation| conversation.get("thread_id"))
        .and_then(Value::as_str)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub fn build_inbound_user_message_to_router(
    src_node_uuid: &str,
    src_ilk: String,
    context: Value,
    payload: Value,
) -> Message {
    build_user_message(
        src_node_uuid,
        None,
        DEFAULT_TTL,
        new_trace_id(),
        Some(src_ilk),
        None,
        context,
        payload,
    )
}
