//! The AI runtime's copy of the frontdesk's handoff, held to gov-common's, which SY.frontdesk.gov
//! parses with (FINDINGS A-257). The runtime runs a handoff at once, beside its model workers
//! (`runs_without_model`), because the frontdesk answers it in code; it must take for one exactly
//! what the frontdesk does. A mark renamed in gov-common, a field that changes type, or a sender
//! rule that changes would otherwise put handoffs behind the LLM turns again, or a turn that calls
//! the model on the lane that has no worker bound. Until the frontdesk says itself which messages it
//! answers without its model, the runtime keeps this copy.

use fluxbee_ai_sdk::{runs_without_model, Message};
use fluxbee_sdk::protocol::{Destination, Meta, Routing};
use gov_common::frontdesk_contract::{
    frontdesk_handoff_sender_allowed, parse_frontdesk_handoff, FrontdeskHandoffPayload,
    FrontdeskHandoffSubject, FRONTDESK_HANDOFF_MSG, FRONTDESK_HANDOFF_PAYLOAD_TYPE,
    FRONTDESK_SCHEMA_VERSION_V1,
};
use serde_json::{json, Value};

fn message(kind: &str, sender: Option<&str>, meta_msg: Option<&str>, payload: Value) -> Message {
    Message {
        routing: Routing {
            src: "uuid-of-the-sender".to_string(),
            src_l2_name: sender.map(str::to_string),
            dst: Destination::Unicast("SY.frontdesk.gov@motherbee".to_string()),
            ttl: 16,
            trace_id: "trace-1".to_string(),
        },
        meta: Meta {
            msg_type: kind.to_string(),
            msg: meta_msg.map(str::to_string),
            ..Meta::default()
        },
        payload,
    }
}

/// A handoff as io.api builds one (`request_frontdesk_handoff`), from gov-common's types.
fn handoff_payload() -> Value {
    serde_json::to_value(FrontdeskHandoffPayload {
        payload_type: FRONTDESK_HANDOFF_PAYLOAD_TYPE.to_string(),
        schema_version: FRONTDESK_SCHEMA_VERSION_V1,
        operation: "complete_registration".to_string(),
        subject: FrontdeskHandoffSubject {
            display_name: Some("Ana".to_string()),
            email: Some("ana@example.com".to_string()),
            ..FrontdeskHandoffSubject::default()
        },
        tenant_id: Some("tnt:acme".to_string()),
        context: None,
    })
    .unwrap()
}

/// Whether the frontdesk runs `msg` as a handoff, in code: a `user` message it parses as one
/// (`on_message`), from an IO node (`handle_frontdesk_handoff` refuses any other sender).
fn frontdesk_runs_as_handoff(msg: &Message) -> bool {
    msg.meta.msg_type.eq_ignore_ascii_case("user")
        && parse_frontdesk_handoff(msg.meta.msg.as_deref(), &msg.payload).is_some()
        && frontdesk_handoff_sender_allowed(msg.routing.src_l2_name.as_deref())
}

#[test]
fn a_handoff_built_with_gov_commons_contract_runs_without_the_model() {
    let msg = message(
        "user",
        Some("IO.api@motherbee"),
        Some(FRONTDESK_HANDOFF_MSG),
        handoff_payload(),
    );
    assert!(frontdesk_runs_as_handoff(&msg));
    assert!(runs_without_model(&msg));
}

#[test]
fn the_runtime_takes_for_a_handoff_exactly_what_the_frontdesk_runs_as_one() {
    let with = |field: &str, value: Value| {
        let mut payload = handoff_payload();
        payload[field] = value;
        payload
    };
    let with_subject = |field: &str, value: Value| {
        let mut payload = handoff_payload();
        payload["subject"][field] = value;
        payload
    };
    let without = |field: &str| {
        let mut payload = handoff_payload();
        payload.as_object_mut().unwrap().remove(field);
        payload
    };
    let payloads = [
        handoff_payload(),
        without("subject"),
        without("tenant_id"),
        without("context"),
        without("schema_version"),
        without("operation"),
        without("type"),
        with("subject", json!({})),
        with("subject", Value::Null),
        with("subject", json!("Ana")),
        with("tenant_id", Value::Null),
        with("tenant_id", json!(7)),
        with("context", json!([1, "two", {"three": 3}])),
        with("schema_version", json!(2)),
        with("schema_version", json!("1")),
        with("schema_version", json!(1.0)),
        with("schema_version", json!(-1)),
        with("schema_version", json!(4_294_967_296_u64)),
        with("operation", json!("rename_person")),
        with("operation", json!(1)),
        with("operation", Value::Null),
        with("type", json!("text")),
        with("type", json!(FRONTDESK_HANDOFF_PAYLOAD_TYPE.to_uppercase())),
        with("extra", json!({"ignored": true})),
        with_subject("phone", json!(5_491_155_550_000_u64)),
        with_subject("phone", json!("+54 9 11 5555 0000")),
        with_subject("email", Value::Null),
        with_subject("email", json!(["ana@example.com"])),
        with_subject("display_name", json!(true)),
        with_subject("company_name", json!("ACME")),
        with_subject("attributes", json!({"plan": 3})),
        with_subject("attributes", json!(["plan"])),
        with_subject("attributes", Value::Null),
        json!({"type": "text", "content": "frontdesk_handoff"}),
        json!(FRONTDESK_HANDOFF_PAYLOAD_TYPE),
        Value::Null,
    ];
    let senders = [
        Some("IO.api@motherbee"),
        Some(" IO.cloud@motherbee "),
        Some("IO.slack.support@worker1"),
        Some("io.api@motherbee"),
        Some("AI.sales@worker1"),
        Some("WF.onboarding@motherbee"),
        Some("SY.admin@motherbee"),
        Some(""),
        None,
    ];
    let marks = [
        Some(FRONTDESK_HANDOFF_MSG),
        Some("FRONTDESK_HANDOFF"),
        Some("text"),
        None,
    ];
    let mut handoffs = 0;
    for kind in ["user", "USER", "event"] {
        for payload in &payloads {
            for sender in senders {
                for mark in marks {
                    let msg = message(kind, sender, mark, payload.clone());
                    let frontdesk = frontdesk_runs_as_handoff(&msg);
                    handoffs += usize::from(frontdesk);
                    assert_eq!(
                        runs_without_model(&msg),
                        frontdesk,
                        "{kind} from {sender:?} marked {mark:?}: {payload}"
                    );
                }
            }
        }
    }
    // The cases hold handoffs as well as what is none.
    assert!(handoffs > 0);
}
