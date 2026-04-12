use serde_json::Value;

pub use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};

pub fn build_reply_routing(msg: &Message, src_uuid: &str) -> Routing {
    let dst = match &msg.routing.src {
        value if value.trim().is_empty() => Destination::Broadcast,
        value => Destination::Unicast(value.to_string()),
    };

    Routing {
        src: src_uuid.to_string(),
        src_l2_name: None,
        dst,
        ttl: msg.routing.ttl.max(1),
        trace_id: msg.routing.trace_id.clone(),
    }
}

pub fn build_reply_message(msg: &Message, src_uuid: &str, payload: Value) -> Message {
    Message {
        routing: build_reply_routing(msg, src_uuid),
        meta: build_reply_meta(&msg.meta),
        payload,
    }
}

/// Builds a reply message where routing.src is intentionally left for runtime assignment.
/// NodeRuntime overwrites routing.src with the connected node UUID before sending.
pub fn build_reply_message_runtime_src(msg: &Message, payload: Value) -> Message {
    Message {
        routing: build_reply_routing(msg, ""),
        meta: build_reply_meta(&msg.meta),
        payload,
    }
}

fn build_reply_meta(meta: &Meta) -> Meta {
    let mut reply = meta.clone();
    reply.thread_seq = None;
    reply.ctx = None;
    reply.ctx_seq = None;
    reply.ctx_window = None;
    reply.memory_package = None;
    reply.context = sanitize_reply_context(reply.context);
    reply
}

fn sanitize_reply_context(context: Option<Value>) -> Option<Value> {
    let Some(Value::Object(mut obj)) = context else {
        return context;
    };
    obj.remove("thread_id");
    obj.remove("src_ilk");
    Some(Value::Object(obj))
}
