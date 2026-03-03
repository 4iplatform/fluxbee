use serde_json::Value;

pub use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};

pub fn build_reply_routing(msg: &Message, src_uuid: &str) -> Routing {
    let dst = match &msg.routing.src {
        value if value.trim().is_empty() => Destination::Broadcast,
        value => Destination::Unicast(value.to_string()),
    };

    Routing {
        src: src_uuid.to_string(),
        dst,
        ttl: msg.routing.ttl.max(1),
        trace_id: msg.routing.trace_id.clone(),
    }
}

pub fn build_reply_message(msg: &Message, src_uuid: &str, payload: Value) -> Message {
    Message {
        routing: build_reply_routing(msg, src_uuid),
        meta: msg.meta.clone(),
        payload,
    }
}
