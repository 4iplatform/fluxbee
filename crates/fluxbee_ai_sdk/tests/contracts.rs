use fluxbee_ai_sdk::{
    build_reply_message, build_reply_message_runtime_src, build_reply_routing, build_text_response,
    extract_text, Destination, Message, Meta, Routing,
};
use serde_json::json;

fn sample_message() -> Message {
    Message {
        routing: Routing {
            src: "node-src-123".to_string(),
            dst: Destination::Unicast("AI.echo@dev".to_string()),
            ttl: 5,
            trace_id: "trace-abc".to_string(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: Some("HELLO".to_string()),
            scope: Some("vpn".to_string()),
            target: Some("AI.echo@dev".to_string()),
            src_ilk: None,
            action: None,
            priority: None,
            context: Some(json!({"thread_id":"thr-1"})),
        },
        payload: json!({
            "type": "text",
            "content": "hola mundo",
            "attachments": []
        }),
    }
}

#[test]
fn contract_message_json_roundtrip_preserves_core_fields() {
    let message = sample_message();
    let encoded = serde_json::to_value(&message).expect("serialize message");
    let decoded: Message = serde_json::from_value(encoded).expect("deserialize message");

    assert_eq!(decoded.routing.src, "node-src-123");
    assert_eq!(decoded.routing.ttl, 5);
    assert_eq!(decoded.routing.trace_id, "trace-abc");
    assert!(matches!(decoded.routing.dst, Destination::Unicast(_)));
    assert_eq!(decoded.meta.msg_type, "user");
}

#[test]
fn contract_reply_routing_inverts_source_and_preserves_trace() {
    let message = sample_message();
    let reply_routing = build_reply_routing(&message, "node-ai-999");

    assert_eq!(reply_routing.src, "node-ai-999");
    assert!(matches!(
        reply_routing.dst,
        Destination::Unicast(ref value) if value == "node-src-123"
    ));
    assert_eq!(reply_routing.trace_id, "trace-abc");
    assert_eq!(reply_routing.ttl, 5);
}

#[test]
fn contract_reply_message_reuses_meta_and_sets_payload() {
    let message = sample_message();
    let payload = json!({
        "type": "text",
        "content": "Echo: hola mundo",
        "attachments": []
    });

    let reply = build_reply_message(&message, "node-ai-999", payload.clone());
    assert_eq!(reply.meta.msg_type, "user");
    assert_eq!(reply.payload, payload);
    assert_eq!(reply.routing.trace_id, "trace-abc");
}

#[test]
fn contract_reply_message_runtime_src_leaves_src_for_runtime_assignment() {
    let message = sample_message();
    let payload = json!({
        "type": "text",
        "content": "Echo: hola mundo",
        "attachments": []
    });

    let reply = build_reply_message_runtime_src(&message, payload.clone());
    assert_eq!(reply.meta.msg_type, "user");
    assert_eq!(reply.payload, payload);
    assert_eq!(reply.routing.trace_id, "trace-abc");
    assert_eq!(reply.routing.src, "");
}

#[test]
fn contract_text_payload_build_and_extract() {
    let payload = build_text_response("respuesta").expect("build text payload");
    let text = extract_text(&payload);
    assert_eq!(text.as_deref(), Some("respuesta"));
}

#[test]
fn contract_extract_text_rejects_non_text_payload() {
    let payload = json!({"type":"json","value":{"ok":true}});
    assert!(extract_text(&payload).is_none());
}

