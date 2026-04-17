use serde_json::Value;

use crate::errors::{AiSdkError, Result};
use crate::llm::OutputSchemaSpec;

pub use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplyContextOptions {
    pub preserve_final_response_contract: bool,
}

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
    build_reply_message_with_options(msg, src_uuid, payload, ReplyContextOptions::default())
}

pub fn build_reply_message_with_options(
    msg: &Message,
    src_uuid: &str,
    payload: Value,
    options: ReplyContextOptions,
) -> Message {
    Message {
        routing: build_reply_routing(msg, src_uuid),
        meta: build_reply_meta(&msg.meta, options),
        payload,
    }
}

/// Builds a reply message where routing.src is intentionally left for runtime assignment.
/// NodeRuntime overwrites routing.src with the connected node UUID before sending.
pub fn build_reply_message_runtime_src(msg: &Message, payload: Value) -> Message {
    build_reply_message_runtime_src_with_options(msg, payload, ReplyContextOptions::default())
}

pub fn build_reply_message_runtime_src_with_options(
    msg: &Message,
    payload: Value,
    options: ReplyContextOptions,
) -> Message {
    Message {
        routing: build_reply_routing(msg, ""),
        meta: build_reply_meta(&msg.meta, options),
        payload,
    }
}

fn build_reply_meta(meta: &Meta, options: ReplyContextOptions) -> Meta {
    let mut reply = meta.clone();
    reply.thread_seq = None;
    reply.ctx = None;
    reply.ctx_seq = None;
    reply.ctx_window = None;
    reply.memory_package = None;
    reply.context = sanitize_reply_context(reply.context, options);
    reply
}

fn sanitize_reply_context(context: Option<Value>, options: ReplyContextOptions) -> Option<Value> {
    let Some(Value::Object(mut obj)) = context else {
        return context;
    };
    obj.remove("thread_id");
    obj.remove("src_ilk");
    obj.remove("response_envelope");
    if !options.preserve_final_response_contract {
        obj.remove("final_response_contract");
    }
    Some(Value::Object(obj))
}

pub fn extract_response_envelope(meta: &Meta) -> Result<Option<Value>> {
    extract_context_object_field(meta, "response_envelope")
}

pub fn extract_final_response_contract(meta: &Meta) -> Result<Option<Value>> {
    extract_context_object_field(meta, "final_response_contract")
}

pub fn resolve_response_envelope_output_schema(meta: &Meta) -> Result<Option<OutputSchemaSpec>> {
    extract_response_envelope(meta)?
        .as_ref()
        .map(response_contract_to_output_schema_spec)
        .transpose()
}

pub fn resolve_final_response_contract_output_schema(
    meta: &Meta,
) -> Result<Option<OutputSchemaSpec>> {
    extract_final_response_contract(meta)?
        .as_ref()
        .map(response_contract_to_output_schema_spec)
        .transpose()
}

pub fn response_contract_to_output_schema_spec(contract: &Value) -> Result<OutputSchemaSpec> {
    let obj = contract
        .as_object()
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract must be a JSON object".to_string(),
        })?;

    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract.kind is required".to_string(),
        })?;
    if kind != "json_object_v1" {
        return Err(AiSdkError::InvalidResponseContract {
            detail: format!("unsupported response contract kind: {kind}"),
        });
    }

    let strict = obj.get("strict").and_then(Value::as_bool).unwrap_or(false);
    let required = obj
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract.required is required".to_string(),
        })?;
    if required.is_empty() {
        return Err(AiSdkError::InvalidResponseContract {
            detail: "response contract.required must not be empty".to_string(),
        });
    }

    let properties = obj
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract.properties is required".to_string(),
        })?;
    if properties.is_empty() {
        return Err(AiSdkError::InvalidResponseContract {
            detail: "response contract.properties must not be empty".to_string(),
        });
    }

    let mut schema_required = Vec::with_capacity(required.len());
    let mut schema_properties = serde_json::Map::with_capacity(properties.len());

    for field in required {
        let field_name = field.as_str().ok_or_else(|| AiSdkError::InvalidResponseContract {
            detail: "response contract.required entries must be strings".to_string(),
        })?;
        if field_name.trim().is_empty() {
            return Err(AiSdkError::InvalidResponseContract {
                detail: "response contract.required must not contain empty names".to_string(),
            });
        }
        if !properties.contains_key(field_name) {
            return Err(AiSdkError::InvalidResponseContract {
                detail: format!(
                    "required field '{field_name}' is missing from response contract.properties"
                ),
            });
        }
        schema_required.push(Value::String(field_name.to_string()));
    }

    for (field_name, field_schema) in properties {
        let field_obj = field_schema
            .as_object()
            .ok_or_else(|| AiSdkError::InvalidResponseContract {
                detail: format!("response contract.properties.{field_name} must be an object"),
            })?;
        let field_type =
            field_obj
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| AiSdkError::InvalidResponseContract {
                    detail: format!(
                        "response contract.properties.{field_name}.type is required"
                    ),
                })?;
        match field_type {
            "string" | "boolean" | "integer" | "number" => {}
            other => {
                return Err(AiSdkError::InvalidResponseContract {
                    detail: format!(
                        "unsupported type '{other}' for response contract property '{field_name}'"
                    ),
                })
            }
        }

        let mut translated = serde_json::Map::new();
        translated.insert("type".to_string(), Value::String(field_type.to_string()));

        if let Some(enum_value) = field_obj.get("enum") {
            let enum_items =
                enum_value
                    .as_array()
                    .ok_or_else(|| AiSdkError::InvalidResponseContract {
                        detail: format!(
                            "response contract.properties.{field_name}.enum must be an array"
                        ),
                    })?;
            if field_type != "string" {
                return Err(AiSdkError::InvalidResponseContract {
                    detail: format!("enum is only supported for string property '{field_name}'"),
                });
            }
            if !enum_items.iter().all(|item| item.as_str().is_some()) {
                return Err(AiSdkError::InvalidResponseContract {
                    detail: format!(
                        "response contract.properties.{field_name}.enum entries must be strings"
                    ),
                });
            }
            translated.insert("enum".to_string(), Value::Array(enum_items.clone()));
        }

        schema_properties.insert(field_name.to_string(), Value::Object(translated));
    }

    OutputSchemaSpec::new(
        "final_output",
        Value::Object(serde_json::Map::from_iter([
            ("type".to_string(), Value::String("object".to_string())),
            ("properties".to_string(), Value::Object(schema_properties)),
            ("required".to_string(), Value::Array(schema_required)),
            ("additionalProperties".to_string(), Value::Bool(false)),
        ])),
        strict,
    )
}

fn extract_context_object_field(meta: &Meta, field: &str) -> Result<Option<Value>> {
    let Some(context) = meta.context.as_ref() else {
        return Ok(None);
    };
    let obj = context.as_object().ok_or_else(|| AiSdkError::InvalidResponseContract {
        detail: "meta.context must be a JSON object".to_string(),
    })?;
    match obj.get(field) {
        None => Ok(None),
        Some(value) if value.is_object() => Ok(Some(value.clone())),
        Some(_) => Err(AiSdkError::InvalidResponseContract {
            detail: format!("meta.context.{field} must be a JSON object"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn extract_response_envelope_returns_none_when_context_missing() {
        let meta = Meta::default();
        assert_eq!(extract_response_envelope(&meta).expect("ok"), None);
    }

    #[test]
    fn extract_response_envelope_rejects_non_object_context() {
        let meta = Meta {
            context: Some(json!("bad")),
            ..Meta::default()
        };
        assert!(matches!(
            extract_response_envelope(&meta),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn extract_response_envelope_rejects_non_object_value() {
        let meta = Meta {
            context: Some(json!({"response_envelope":"bad"})),
            ..Meta::default()
        };
        assert!(matches!(
            extract_response_envelope(&meta),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn extract_final_response_contract_returns_object() {
        let value = json!({"kind":"json_object_v1","properties":{"ok":{"type":"boolean"}},"required":["ok"]});
        let meta = Meta {
            context: Some(json!({"final_response_contract": value})),
            ..Meta::default()
        };
        assert_eq!(
            extract_final_response_contract(&meta).expect("ok"),
            Some(json!({"kind":"json_object_v1","properties":{"ok":{"type":"boolean"}},"required":["ok"]}))
        );
    }

    #[test]
    fn sanitize_reply_context_removes_envelope_and_contract_by_default() {
        let sanitized = sanitize_reply_context(
            Some(json!({
                "response_envelope": {"kind":"json_object_v1"},
                "final_response_contract": {"kind":"json_object_v1"},
                "reply_target": {"kind":"demo"},
            })),
            ReplyContextOptions::default(),
        )
        .expect("context should stay object");
        assert!(sanitized.get("response_envelope").is_none());
        assert!(sanitized.get("final_response_contract").is_none());
        assert!(sanitized.get("reply_target").is_some());
    }

    #[test]
    fn sanitize_reply_context_can_preserve_final_response_contract_explicitly() {
        let sanitized = sanitize_reply_context(
            Some(json!({
                "response_envelope": {"kind":"json_object_v1"},
                "final_response_contract": {"kind":"json_object_v1"},
                "reply_target": {"kind":"demo"},
            })),
            ReplyContextOptions {
                preserve_final_response_contract: true,
            },
        )
        .expect("context should stay object");
        assert!(sanitized.get("response_envelope").is_none());
        assert!(sanitized.get("final_response_contract").is_some());
        assert!(sanitized.get("reply_target").is_some());
    }

    #[test]
    fn build_reply_message_runtime_src_can_preserve_final_response_contract_explicitly() {
        let request = Message {
            routing: Routing {
                src: "IO.api@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("AI.demo@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-123".to_string(),
            },
            meta: Meta {
                context: Some(json!({
                    "response_envelope": {"kind":"json_object_v1"},
                    "final_response_contract": {"kind":"json_object_v1"},
                    "reply_target": {"kind":"demo"},
                })),
                ..Meta::default()
            },
            payload: json!({}),
        };

        let reply = build_reply_message_runtime_src_with_options(
            &request,
            json!({"ok":true}),
            ReplyContextOptions {
                preserve_final_response_contract: true,
            },
        );

        let context = reply.meta.context.expect("reply context");
        assert!(context.get("response_envelope").is_none());
        assert!(context.get("final_response_contract").is_some());
        assert!(context.get("reply_target").is_some());
    }

    #[test]
    fn response_contract_to_output_schema_spec_translates_json_object_v1() {
        let spec = response_contract_to_output_schema_spec(&json!({
            "kind":"json_object_v1",
            "strict": true,
            "required":["success","human_message"],
            "properties":{
                "success":{"type":"boolean"},
                "human_message":{"type":"string"},
                "error_code":{"type":"string","enum":["missing_data","unknown"]}
            }
        }))
        .expect("contract should translate");
        assert_eq!(spec.name(), "final_output");
        assert_eq!(spec.strict(), true);
        assert_eq!(spec.json_schema()["type"], "object");
        assert_eq!(spec.json_schema()["required"], json!(["success", "human_message"]));
        assert_eq!(
            spec.json_schema()["properties"]["error_code"]["enum"],
            json!(["missing_data", "unknown"])
        );
        assert_eq!(spec.json_schema()["additionalProperties"], false);
    }

    #[test]
    fn response_contract_to_output_schema_spec_rejects_unsupported_kind() {
        assert!(matches!(
            response_contract_to_output_schema_spec(&json!({
                "kind":"array_v1",
                "required":["ok"],
                "properties":{"ok":{"type":"boolean"}}
            })),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn response_contract_to_output_schema_spec_rejects_missing_property_for_required() {
        assert!(matches!(
            response_contract_to_output_schema_spec(&json!({
                "kind":"json_object_v1",
                "required":["ok"],
                "properties":{"other":{"type":"boolean"}}
            })),
            Err(AiSdkError::InvalidResponseContract { .. })
        ));
    }

    #[test]
    fn resolve_response_envelope_output_schema_reads_and_translates() {
        let meta = Meta {
            context: Some(json!({
                "response_envelope":{
                    "kind":"json_object_v1",
                    "required":["ok"],
                    "properties":{"ok":{"type":"boolean"}}
                }
            })),
            ..Meta::default()
        };
        let spec = resolve_response_envelope_output_schema(&meta)
            .expect("ok")
            .expect("schema should exist");
        assert_eq!(spec.json_schema()["properties"]["ok"]["type"], "boolean");
    }
}
