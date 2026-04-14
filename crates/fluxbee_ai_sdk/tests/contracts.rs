use fluxbee_ai_sdk::{
    build_reply_message, build_reply_message_runtime_src, build_reply_routing, build_text_response,
    extract_text, Destination, Message, Meta, Routing,
};
use fluxbee_sdk::protocol::{MemoryContextSummary, MemoryPackage, MemoryReasonSummary};
use serde_json::json;

fn sample_message() -> Message {
    Message {
        routing: Routing {
            src: "node-src-123".to_string(),
            src_l2_name: None,
            dst: Destination::Unicast("AI.echo@dev".to_string()),
            ttl: 5,
            trace_id: "trace-abc".to_string(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: Some("HELLO".to_string()),
            scope: Some("vpn".to_string()),
            target: Some("AI.echo@dev".to_string()),
            src_ilk: Some("ilk:11111111-1111-4111-8111-111111111111".to_string()),
            ich: Some("io://probe-1".to_string()),
            thread_id: Some("thread:canonical-1".to_string()),
            thread_seq: Some(7),
            ctx: Some("legacy-ctx-1".to_string()),
            ctx_seq: Some(9),
            ctx_window: Some(vec![]),
            memory_package: Some(MemoryPackage {
                package_version: 2,
                thread_id: "thread:canonical-1".to_string(),
                dominant_context: Some(MemoryContextSummary {
                    context_id: "context:1".to_string(),
                    label: "billing".to_string(),
                    weight: 3.0,
                }),
                dominant_reason: Some(MemoryReasonSummary {
                    reason_id: "reason:1".to_string(),
                    label: "seeking assistance".to_string(),
                    weight: 2.0,
                }),
                contexts: vec![],
                reasons: vec![],
                memories: vec![],
                episodes: vec![],
                truncated: None,
            }),
            action: None,
            priority: None,
            context: Some(json!({
                "thread_id":"thr-1",
                "src_ilk":"ilk:legacy-context-value",
                "probe_id":"probe-1"
            })),
            ..Meta::default()
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
    assert_eq!(reply.meta.thread_id.as_deref(), Some("thread:canonical-1"));
    assert_eq!(
        reply.meta.src_ilk.as_deref(),
        Some("ilk:11111111-1111-4111-8111-111111111111")
    );
    assert_eq!(reply.meta.ich.as_deref(), Some("io://probe-1"));
    assert_eq!(reply.meta.thread_seq, None);
    assert_eq!(reply.meta.ctx, None);
    assert_eq!(reply.meta.ctx_seq, None);
    assert!(reply.meta.ctx_window.is_none());
    assert!(reply.meta.memory_package.is_none());
    assert_eq!(
        reply
            .meta
            .context
            .as_ref()
            .and_then(|v| v.get("probe_id"))
            .and_then(|v| v.as_str()),
        Some("probe-1")
    );
    assert_eq!(
        reply.meta.context.as_ref().and_then(|v| v.get("thread_id")),
        None
    );
    assert_eq!(
        reply.meta.context.as_ref().and_then(|v| v.get("src_ilk")),
        None
    );
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
    assert_eq!(reply.meta.thread_id.as_deref(), Some("thread:canonical-1"));
    assert_eq!(reply.meta.thread_seq, None);
    assert_eq!(reply.meta.ctx, None);
    assert!(reply.meta.memory_package.is_none());
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
