use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::{compute_thread_id, ThreadIdInput};
use io_common::identity::ResolveOrCreateInput;
use io_common::io_context::{ConversationRef, IoContext, MessageRef, PartyRef, ReplyTarget};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiIngressError {
    pub(crate) code: &'static str,
    pub(crate) detail: String,
}

impl ApiIngressError {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EndpointPrincipal {
    pub(crate) tenant_id: String,
    pub(crate) caller_identity: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExplicitSubjectMode {
    ByIlk { ilk: String },
    ByData,
}

struct ParsedSubject {
    external_user_id: String,
    display_name: Option<Value>,
    email: Option<Value>,
    explicit_mode: Option<ExplicitSubjectMode>,
}

#[derive(Debug)]
pub(crate) struct ParsedApiMessage {
    pub(crate) request_id: String,
    pub(crate) identity_input: ResolveOrCreateInput,
    pub(crate) io_context: IoContext,
    pub(crate) payload: Value,
    pub(crate) relay_final: bool,
    pub(crate) explicit_subject_mode: Option<ExplicitSubjectMode>,
}

pub(crate) fn parse_api_message_request(
    envelope: &Value,
    effective: &Value,
    principal: &EndpointPrincipal,
) -> Result<ParsedApiMessage, ApiIngressError> {
    let envelope = envelope.as_object().ok_or_else(|| {
        ApiIngressError::new("invalid_payload", "request body must be a JSON object")
    })?;
    reject_routing_override(envelope.get("options"))?;

    let subject_mode = effective
        .get("ingress")
        .and_then(|ingress| ingress.get("subject_mode"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let message = envelope
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiIngressError::new("invalid_payload", "field 'message' is required"))?;
    if message.contains_key("attachments") || envelope.contains_key("attachments") {
        return Err(ApiIngressError::new(
            "attachments_not_supported",
            "IO.api Edge ingress accepts inline JSON only; publish large data through Blob",
        ));
    }
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiIngressError::new("invalid_payload", "field 'message.text' is required")
        })?;

    let subject = envelope.get("subject");
    let parsed_subject = if subject_mode == "explicit_subject" {
        parse_explicit_subject(subject)?
    } else {
        if subject.is_some() {
            return Err(ApiIngressError::new(
                "invalid_payload",
                "field 'subject' is not allowed for subject_mode=caller_is_subject",
            ));
        }
        parse_configured_caller(principal.caller_identity.as_ref())?
    };
    let ParsedSubject {
        external_user_id,
        display_name,
        email,
        explicit_mode: explicit_subject_mode,
    } = parsed_subject;

    let request_id = envelope
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("req_{}", Uuid::new_v4().simple()));
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
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
    let api_channel_id = effective
        .get("io")
        .and_then(|io| io.get("api_channel_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiIngressError::new("node_not_configured", "missing io.api_channel_id"))?;
    let thread_id = compute_thread_id(ThreadIdInput::PersistentChannel {
        channel_type: "api",
        entrypoint_id: Some(api_channel_id),
        conversation_id: conversation_seed.as_str(),
    })
    .map_err(|err| {
        ApiIngressError::new(
            "invalid_payload",
            format!("failed to build thread_id: {err}"),
        )
    })?;

    let mut attributes = serde_json::Map::new();
    attributes.insert(
        "api_channel_id".to_string(),
        Value::String(api_channel_id.to_string()),
    );
    if let Some(value) = display_name {
        attributes.insert("display_name".to_string(), value);
    }
    if let Some(value) = email {
        attributes.insert("email".to_string(), value);
    }
    if let Some(subject) = subject.and_then(Value::as_object) {
        copy_optional_subject_fields(subject, &mut attributes)?;
    }
    if let Some(metadata) = envelope
        .get("options")
        .and_then(|options| options.get("metadata"))
        .cloned()
    {
        attributes.insert("request_metadata".to_string(), metadata);
    }

    let text_payload = TextV1Payload::new(text, vec![]).to_value().map_err(|err| {
        ApiIngressError::new(
            "invalid_payload",
            format!("unable to build text/v1 payload: {err}"),
        )
    })?;

    Ok(ParsedApiMessage {
        request_id,
        identity_input: ResolveOrCreateInput {
            channel: "api".to_string(),
            external_id: external_user_id.clone(),
            src_ilk_override: explicit_subject_mode.as_ref().and_then(|mode| match mode {
                ExplicitSubjectMode::ByIlk { ilk } => Some(ilk.clone()),
                ExplicitSubjectMode::ByData => None,
            }),
            tenant_id: Some(principal.tenant_id.clone()),
            tenant_hint: None,
            attributes: Value::Object(attributes),
            ilk_type: Some("human".to_string()),
        },
        io_context: IoContext {
            channel: "api".to_string(),
            entrypoint: PartyRef {
                kind: "api_channel".to_string(),
                id: api_channel_id.to_string(),
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
                address: api_channel_id.to_string(),
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
        explicit_subject_mode,
    })
}

fn parse_explicit_subject(subject: Option<&Value>) -> Result<ParsedSubject, ApiIngressError> {
    let subject = subject.and_then(Value::as_object).ok_or_else(|| {
        ApiIngressError::new(
            "invalid_payload",
            "field 'subject' is required for subject_mode=explicit_subject",
        )
    })?;
    if subject.contains_key("tenant_id") || subject.contains_key("tenant_hint") {
        return Err(ApiIngressError::new(
            "invalid_payload",
            "subject tenant fields are not allowed; tenant is fixed by the IO.api instance",
        ));
    }
    if let Some(ilk) = subject
        .get("ilk")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let external_user_id = subject
            .get("external_user_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(ilk)
            .to_string();
        return Ok(ParsedSubject {
            external_user_id,
            display_name: subject.get("display_name").cloned(),
            email: subject.get("email").cloned(),
            explicit_mode: Some(ExplicitSubjectMode::ByIlk {
                ilk: ilk.to_string(),
            }),
        });
    }

    let external_user_id = required_subject_string(subject, "external_user_id")?;
    let display_name = Value::String(required_subject_string(subject, "display_name")?);
    let email = Value::String(required_subject_string(subject, "email")?);
    Ok(ParsedSubject {
        external_user_id,
        display_name: Some(display_name),
        email: Some(email),
        explicit_mode: Some(ExplicitSubjectMode::ByData),
    })
}

fn parse_configured_caller(caller: Option<&Value>) -> Result<ParsedSubject, ApiIngressError> {
    let caller = caller.and_then(Value::as_object).ok_or_else(|| {
        ApiIngressError::new(
            "node_not_configured",
            "ingress.caller_identity is missing for caller_is_subject",
        )
    })?;
    Ok(ParsedSubject {
        external_user_id: required_subject_string(caller, "external_user_id")?,
        display_name: caller.get("display_name").cloned(),
        email: caller.get("email").cloned(),
        explicit_mode: None,
    })
}

fn required_subject_string(
    subject: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<String, ApiIngressError> {
    subject
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            ApiIngressError::new(
                "subject_data_incomplete",
                format!("field 'subject.{field}' is required"),
            )
        })
}

fn copy_optional_subject_fields(
    subject: &serde_json::Map<String, Value>,
    attributes: &mut serde_json::Map<String, Value>,
) -> Result<(), ApiIngressError> {
    for field in ["company_name", "phone"] {
        if let Some(value) = subject.get(field) {
            let value = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    ApiIngressError::new(
                        "invalid_payload",
                        format!("field 'subject.{field}' must be a non-empty string"),
                    )
                })?;
            attributes.insert(field.to_string(), Value::String(value.to_string()));
        }
    }
    if let Some(extra) = subject.get("attributes") {
        let extra = extra.as_object().ok_or_else(|| {
            ApiIngressError::new(
                "invalid_payload",
                "field 'subject.attributes' must be an object",
            )
        })?;
        for (key, value) in extra {
            attributes.insert(key.clone(), value.clone());
        }
    }
    Ok(())
}

fn reject_routing_override(options: Option<&Value>) -> Result<(), ApiIngressError> {
    let Some(options) = options else {
        return Ok(());
    };
    let options = options.as_object().ok_or_else(|| {
        ApiIngressError::new("invalid_payload", "field 'options' must be an object")
    })?;
    if options.contains_key("routing") {
        return Err(ApiIngressError::new(
            "routing_override_forbidden",
            "options.routing is not accepted; destination belongs to instance configuration",
        ));
    }
    Ok(())
}

pub(crate) fn api_relay_key(
    node_name: &str,
    conversation_id: &str,
    external_user_id: &str,
) -> String {
    format!("api:{node_name}:{conversation_id}:{external_user_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn effective(subject_mode: &str) -> Value {
        json!({
            "io": {"api_channel_id":"orders", "dst_node":"AI.orders@worker"},
            "ingress": {
                "subject_mode": subject_mode,
                "caller_identity": {"external_user_id":"partner-service"}
            }
        })
    }

    fn principal() -> EndpointPrincipal {
        EndpointPrincipal {
            tenant_id: "tnt:11111111-1111-4111-8111-111111111111".to_string(),
            caller_identity: Some(json!({"external_user_id":"partner-service"})),
        }
    }

    #[test]
    fn explicit_subject_uses_instance_tenant() {
        let parsed = parse_api_message_request(
            &json!({
                "subject": {
                    "external_user_id":"user-1",
                    "display_name":"User One",
                    "email":"one@example.com"
                },
                "message":{"text":"hello"}
            }),
            &effective("explicit_subject"),
            &principal(),
        )
        .expect("parse");
        assert_eq!(
            parsed.identity_input.tenant_id.as_deref(),
            Some("tnt:11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(parsed.io_context.entrypoint.id, "orders");
    }

    #[test]
    fn rejects_tenant_and_routing_injection() {
        let err = parse_api_message_request(
            &json!({
                "subject": {
                    "external_user_id":"user-1",
                    "display_name":"User One",
                    "email":"one@example.com",
                    "tenant_id":"tnt:other"
                },
                "message":{"text":"hello"}
            }),
            &effective("explicit_subject"),
            &principal(),
        )
        .expect_err("tenant injection");
        assert_eq!(err.code, "invalid_payload");

        let err = parse_api_message_request(
            &json!({
                "subject": {
                    "external_user_id":"user-1",
                    "display_name":"User One",
                    "email":"one@example.com"
                },
                "message":{"text":"hello"},
                "options":{"routing":{"dst_node":"SY.admin@motherbee"}}
            }),
            &effective("explicit_subject"),
            &principal(),
        )
        .expect_err("routing injection");
        assert_eq!(err.code, "routing_override_forbidden");
    }

    #[test]
    fn rejects_attachments_in_edge_inline_contract() {
        let err = parse_api_message_request(
            &json!({"message":{"text":"hello", "attachments":[]}}),
            &effective("caller_is_subject"),
            &principal(),
        )
        .expect_err("attachments");
        assert_eq!(err.code, "attachments_not_supported");
    }
}
