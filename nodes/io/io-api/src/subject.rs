use axum::http::StatusCode;
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::{compute_thread_id, ThreadIdInput};
use io_common::identity::ResolveOrCreateInput;
use io_common::io_context::{ConversationRef, IoContext, MessageRef, PartyRef, ReplyTarget};
use serde_json::Value;
use uuid::Uuid;

use crate::{AuthMatch, ParsedHttpMessage};

pub(crate) fn parse_json_message_request(
    envelope: &Value,
    effective: &Value,
    auth_match: &AuthMatch,
    allow_empty_text_if_attachments: bool,
) -> std::result::Result<ParsedHttpMessage, (StatusCode, &'static str, String)> {
    let subject_mode = effective
        .get("ingress")
        .and_then(|ingress| ingress.get("subject_mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let message = envelope
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Field 'message' is required".to_string(),
            )
        })?;
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if text.is_empty() && !allow_empty_text_if_attachments {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            "Field 'message.text' is required in JSON ingress until attachments are implemented"
                .to_string(),
        ));
    }

    let subject = envelope.get("subject");
    let caller_identity = auth_match.caller_identity.as_ref();
    let (external_user_id, display_name, email, tenant_hint) = if subject_mode == "explicit_subject"
    {
        let subject_obj = subject.and_then(Value::as_object).ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Field 'subject' is required for subject_mode=explicit_subject".to_string(),
            )
        })?;
        let external_user_id = subject_obj
            .get("external_user_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_payload",
                    "Field 'subject.external_user_id' is required".to_string(),
                )
            })?
            .to_string();
        (
            external_user_id,
            subject_obj.get("display_name").cloned(),
            subject_obj.get("email").cloned(),
            subject_obj.get("tenant_hint").cloned(),
        )
    } else {
        if subject.is_some() {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Field 'subject' is not allowed for subject_mode=caller_is_subject".to_string(),
            ));
        }
        let caller = caller_identity.and_then(Value::as_object).ok_or_else(|| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_payload",
                "Authenticated caller does not define caller_identity".to_string(),
            )
        })?;
        let external_user_id = caller
            .get("external_user_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_payload",
                    "Authenticated caller is missing external_user_id".to_string(),
                )
            })?
            .to_string();
        (
            external_user_id,
            caller.get("display_name").cloned(),
            caller.get("email").cloned(),
            caller.get("tenant_hint").cloned(),
        )
    };

    let request_id = format!("req_{}", Uuid::new_v4().simple());
    let external_message_id = message
        .get("external_message_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| request_id.clone());
    let timestamp = message
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let conversation_seed = envelope
        .get("options")
        .and_then(|options| options.get("metadata"))
        .and_then(|metadata| metadata.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| external_user_id.clone());
    let thread_id = compute_thread_id(ThreadIdInput::PersistentChannel {
        channel_type: "api",
        entrypoint_id: Some(
            effective
                .get("listen")
                .and_then(|listen| listen.get("address"))
                .and_then(Value::as_str)
                .unwrap_or("api"),
        ),
        conversation_id: conversation_seed.as_str(),
    })
    .map_err(|err| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            format!("Failed to build thread_id: {err}"),
        )
    })?;

    let mut attributes = serde_json::Map::new();
    attributes.insert(
        "auth_key_id".to_string(),
        Value::String(auth_match.key_id.clone()),
    );
    if let Some(value) = display_name.clone() {
        attributes.insert("display_name".to_string(), value);
    }
    if let Some(value) = email.clone() {
        attributes.insert("email".to_string(), value);
    }
    if let Some(metadata) = envelope
        .get("options")
        .and_then(|options| options.get("metadata"))
        .cloned()
    {
        attributes.insert("request_metadata".to_string(), metadata);
    }

    let text_payload = TextV1Payload::new(text, vec![]).to_value().map_err(|err| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_payload",
            format!("Unable to build text/v1 payload: {err}"),
        )
    })?;

    Ok(ParsedHttpMessage {
        request_id,
        identity_input: ResolveOrCreateInput {
            channel: "api".to_string(),
            external_id: external_user_id.clone(),
            tenant_hint: tenant_hint
                .as_ref()
                .and_then(Value::as_str)
                .map(ToString::to_string),
            attributes: Value::Object(attributes),
        },
        io_context: IoContext {
            channel: "api".to_string(),
            entrypoint: PartyRef {
                kind: "io_api_instance".to_string(),
                id: effective
                    .get("listen")
                    .and_then(|listen| listen.get("address"))
                    .and_then(Value::as_str)
                    .unwrap_or("api")
                    .to_string(),
            },
            sender: PartyRef {
                kind: "api_subject".to_string(),
                id: external_user_id,
            },
            conversation: ConversationRef {
                kind: "api_conversation".to_string(),
                id: conversation_seed,
                thread_id: Some(thread_id),
            },
            message: MessageRef {
                id: external_message_id,
                timestamp,
            },
            reply_target: ReplyTarget {
                kind: "io_api_noop".to_string(),
                address: effective
                    .get("listen")
                    .and_then(|listen| listen.get("address"))
                    .and_then(Value::as_str)
                    .unwrap_or("api")
                    .to_string(),
                params: serde_json::json!({}),
            },
        },
        payload: text_payload,
        relay_final: envelope
            .get("options")
            .and_then(|options| options.get("relay"))
            .and_then(|relay| relay.get("final"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub(crate) fn api_relay_key(
    node_name: &str,
    conversation_id: &str,
    external_user_id: &str,
) -> String {
    format!("api:{node_name}:{conversation_id}:{external_user_id}")
}
