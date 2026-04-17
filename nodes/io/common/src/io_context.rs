#![forbid(unsafe_code)]

use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::{compute_thread_id, ThreadIdInput};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const RESPONSE_ENVELOPE_FIELD: &str = "response_envelope";
pub const FINAL_RESPONSE_CONTRACT_FIELD: &str = "final_response_contract";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoContext {
    pub channel: String,
    pub entrypoint: PartyRef,
    pub sender: PartyRef,
    pub conversation: ConversationRef,
    pub message: MessageRef,
    pub reply_target: ReplyTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyRef {
    pub kind: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRef {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRef {
    pub id: String,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyTarget {
    pub kind: String,
    pub address: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IoResponseContractError {
    #[error("invalid meta.context: {0}")]
    InvalidMetaContext(String),
    #[error("invalid response contract: {0}")]
    InvalidResponseContract(String),
    #[error("invalid structured response: {0}")]
    InvalidStructuredResponse(String),
}

pub fn wrap_in_meta_context(io: &IoContext) -> Value {
    serde_json::json!({ "io": io })
}

pub fn set_response_envelope(meta_context: Option<Value>, envelope: Value) -> Result<Value, IoResponseContractError> {
    set_context_contract_field(meta_context, RESPONSE_ENVELOPE_FIELD, envelope)
}

pub fn set_final_response_contract(
    meta_context: Option<Value>,
    contract: Value,
) -> Result<Value, IoResponseContractError> {
    set_context_contract_field(meta_context, FINAL_RESPONSE_CONTRACT_FIELD, contract)
}

pub fn extract_io_context(meta_context: &Value) -> Option<IoContext> {
    let io = meta_context.get("io")?.clone();
    serde_json::from_value(io).ok()
}

pub fn extract_reply_target(meta_context: &Value) -> Option<ReplyTarget> {
    let reply_target = meta_context.get("io")?.get("reply_target")?.clone();
    serde_json::from_value(reply_target).ok()
}

pub fn extract_response_envelope(
    meta_context: &Value,
) -> Result<Option<Value>, IoResponseContractError> {
    extract_context_contract_field(meta_context, RESPONSE_ENVELOPE_FIELD)
}

pub fn extract_final_response_contract(
    meta_context: &Value,
) -> Result<Option<Value>, IoResponseContractError> {
    extract_context_contract_field(meta_context, FINAL_RESPONSE_CONTRACT_FIELD)
}

pub fn validate_response_contract(contract: &Value) -> Result<(), IoResponseContractError> {
    let obj = contract
        .as_object()
        .ok_or_else(|| IoResponseContractError::InvalidResponseContract {
            0: "response contract must be a JSON object".to_string(),
        })?;

    let kind = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| IoResponseContractError::InvalidResponseContract {
            0: "response contract.kind is required".to_string(),
        })?;
    if kind != "json_object_v1" {
        return Err(IoResponseContractError::InvalidResponseContract(format!(
            "unsupported response contract kind: {kind}"
        )));
    }

    let required = obj.get("required").and_then(Value::as_array).ok_or_else(|| {
        IoResponseContractError::InvalidResponseContract(
            "response contract.required is required".to_string(),
        )
    })?;
    if required.is_empty() {
        return Err(IoResponseContractError::InvalidResponseContract(
            "response contract.required must not be empty".to_string(),
        ));
    }

    let properties = obj.get("properties").and_then(Value::as_object).ok_or_else(|| {
        IoResponseContractError::InvalidResponseContract(
            "response contract.properties is required".to_string(),
        )
    })?;
    if properties.is_empty() {
        return Err(IoResponseContractError::InvalidResponseContract(
            "response contract.properties must not be empty".to_string(),
        ));
    }

    for field in required {
        let field_name = field.as_str().ok_or_else(|| {
            IoResponseContractError::InvalidResponseContract(
                "response contract.required entries must be strings".to_string(),
            )
        })?;
        if field_name.trim().is_empty() {
            return Err(IoResponseContractError::InvalidResponseContract(
                "response contract.required must not contain empty names".to_string(),
            ));
        }
        if !properties.contains_key(field_name) {
            return Err(IoResponseContractError::InvalidResponseContract(format!(
                "required field '{field_name}' is missing from response contract.properties"
            )));
        }
    }

    for (field_name, field_schema) in properties {
        let field_obj = field_schema.as_object().ok_or_else(|| {
            IoResponseContractError::InvalidResponseContract(format!(
                "response contract.properties.{field_name} must be an object"
            ))
        })?;
        let field_type = field_obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                IoResponseContractError::InvalidResponseContract(format!(
                    "response contract.properties.{field_name}.type is required"
                ))
            })?;

        match field_type {
            "string" | "boolean" | "integer" | "number" => {}
            other => {
                return Err(IoResponseContractError::InvalidResponseContract(format!(
                    "unsupported type '{other}' for response contract property '{field_name}'"
                )));
            }
        }

        if let Some(enum_value) = field_obj.get("enum") {
            let enum_items = enum_value.as_array().ok_or_else(|| {
                IoResponseContractError::InvalidResponseContract(format!(
                    "response contract.properties.{field_name}.enum must be an array"
                ))
            })?;
            if field_type != "string" {
                return Err(IoResponseContractError::InvalidResponseContract(format!(
                    "enum is only supported for string property '{field_name}'"
                )));
            }
            if !enum_items.iter().all(|item| item.as_str().is_some()) {
                return Err(IoResponseContractError::InvalidResponseContract(format!(
                    "response contract.properties.{field_name}.enum entries must be strings"
                )));
            }
        }
    }

    Ok(())
}

pub fn parse_structured_response_text(
    text: &str,
    contract: &Value,
) -> Result<Map<String, Value>, IoResponseContractError> {
    validate_response_contract(contract)?;
    let parsed: Value = serde_json::from_str(text).map_err(|err| {
        IoResponseContractError::InvalidStructuredResponse(format!(
            "response is not valid JSON: {err}"
        ))
    })?;
    validate_structured_response_value(&parsed, contract)
}

pub fn parse_structured_response_payload(
    payload: &Value,
    contract: &Value,
) -> Result<Map<String, Value>, IoResponseContractError> {
    let parsed = TextV1Payload::from_value(payload).map_err(|err| {
        IoResponseContractError::InvalidStructuredResponse(format!(
            "response payload is not valid text payload: {err}"
        ))
    })?;
    let Some(content) = parsed.content.as_deref() else {
        return Err(IoResponseContractError::InvalidStructuredResponse(
            "response payload uses content_ref; inline content is required for structured response parsing"
                .to_string(),
        ));
    };
    parse_structured_response_text(content, contract)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackPostTarget {
    pub channel_id: String,
    pub thread_ts: Option<String>,
    pub workspace_id: Option<String>,
}

pub fn extract_slack_post_target(meta_context: &Value) -> Option<SlackPostTarget> {
    let rt = extract_reply_target(meta_context)?;
    if rt.kind != "slack_post" {
        return None;
    }
    let thread_ts = rt
        .params
        .get("thread_ts")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let workspace_id = rt
        .params
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(SlackPostTarget {
        channel_id: rt.address,
        thread_ts,
        workspace_id,
    })
}

fn set_context_contract_field(
    meta_context: Option<Value>,
    field: &str,
    contract: Value,
) -> Result<Value, IoResponseContractError> {
    validate_response_contract(&contract)?;
    let mut context_obj = into_context_object(meta_context)?;
    context_obj.insert(field.to_string(), contract);
    Ok(Value::Object(context_obj))
}

fn extract_context_contract_field(
    meta_context: &Value,
    field: &str,
) -> Result<Option<Value>, IoResponseContractError> {
    let context_obj = meta_context.as_object().ok_or_else(|| {
        IoResponseContractError::InvalidMetaContext("meta.context must be a JSON object".to_string())
    })?;
    match context_obj.get(field) {
        None => Ok(None),
        Some(value) => {
            validate_response_contract(value)?;
            Ok(Some(value.clone()))
        }
    }
}

fn into_context_object(meta_context: Option<Value>) -> Result<Map<String, Value>, IoResponseContractError> {
    match meta_context {
        None => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(IoResponseContractError::InvalidMetaContext(
            "meta.context must be a JSON object".to_string(),
        )),
    }
}

fn validate_structured_response_value(
    value: &Value,
    contract: &Value,
) -> Result<Map<String, Value>, IoResponseContractError> {
    let response_obj = value.as_object().ok_or_else(|| {
        IoResponseContractError::InvalidStructuredResponse(
            "response JSON must be an object".to_string(),
        )
    })?;
    let contract_obj = contract.as_object().ok_or_else(|| {
        IoResponseContractError::InvalidResponseContract(
            "response contract must be a JSON object".to_string(),
        )
    })?;
    let required = contract_obj
        .get("required")
        .and_then(Value::as_array)
        .expect("validated required array");
    let properties = contract_obj
        .get("properties")
        .and_then(Value::as_object)
        .expect("validated properties object");

    for required_name in required {
        let field_name = required_name
            .as_str()
            .expect("validated required field name");
        if !response_obj.contains_key(field_name) {
            return Err(IoResponseContractError::InvalidStructuredResponse(format!(
                "response is missing required field '{field_name}'"
            )));
        }
    }

    for field_name in response_obj.keys() {
        if !properties.contains_key(field_name) {
            return Err(IoResponseContractError::InvalidStructuredResponse(format!(
                "response contains unsupported field '{field_name}'"
            )));
        }
    }

    for (field_name, field_value) in response_obj {
        let field_schema = properties
            .get(field_name)
            .and_then(Value::as_object)
            .expect("validated property schema");
        validate_field_value(field_name, field_value, field_schema)?;
    }

    Ok(response_obj.clone())
}

fn validate_field_value(
    field_name: &str,
    field_value: &Value,
    field_schema: &Map<String, Value>,
) -> Result<(), IoResponseContractError> {
    let field_type = field_schema
        .get("type")
        .and_then(Value::as_str)
        .expect("validated field type");

    let type_matches = match field_type {
        "string" => field_value.is_string(),
        "boolean" => field_value.is_boolean(),
        "integer" => field_value.as_i64().is_some() || field_value.as_u64().is_some(),
        "number" => field_value.is_number(),
        _ => false,
    };
    if !type_matches {
        return Err(IoResponseContractError::InvalidStructuredResponse(format!(
            "response field '{field_name}' does not match expected type '{field_type}'"
        )));
    }

    if let Some(enum_items) = field_schema.get("enum").and_then(Value::as_array) {
        let field_str = field_value.as_str().ok_or_else(|| {
            IoResponseContractError::InvalidStructuredResponse(format!(
                "response field '{field_name}' must be a string to match enum"
            ))
        })?;
        if !enum_items.iter().any(|candidate| candidate.as_str() == Some(field_str)) {
            return Err(IoResponseContractError::InvalidStructuredResponse(format!(
                "response field '{field_name}' contains unsupported enum value '{field_str}'"
            )));
        }
    }

    Ok(())
}

pub fn slack_inbound_io_context(
    team_id: &str,
    user_id: &str,
    channel_id: &str,
    thread_ts: Option<&str>,
    message_id: &str,
) -> IoContext {
    let thread_id = match thread_ts {
        Some(native_thread_id) => Some(
            compute_thread_id(ThreadIdInput::NativeThread {
                channel_type: "slack",
                entrypoint_id: Some(team_id),
                conversation_id: channel_id,
                native_thread_id,
            })
            .expect("valid slack native thread input"),
        ),
        None => Some(
            compute_thread_id(ThreadIdInput::PersistentChannel {
                channel_type: "slack",
                entrypoint_id: Some(team_id),
                conversation_id: channel_id,
            })
            .expect("valid slack channel thread input"),
        ),
    };
    let params = match thread_ts {
        Some(t) => serde_json::json!({ "thread_ts": t, "workspace_id": team_id }),
        None => serde_json::json!({ "workspace_id": team_id }),
    };

    IoContext {
        channel: "slack".to_string(),
        entrypoint: PartyRef {
            kind: "slack_workspace".to_string(),
            id: team_id.to_string(),
        },
        sender: PartyRef {
            kind: "slack_user".to_string(),
            id: user_id.to_string(),
        },
        conversation: ConversationRef {
            kind: "slack_channel".to_string(),
            id: channel_id.to_string(),
            thread_id,
        },
        message: MessageRef {
            id: message_id.to_string(),
            timestamp: None,
        },
        reply_target: ReplyTarget {
            kind: "slack_post".to_string(),
            address: channel_id.to_string(),
            params,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::{compute_thread_id, ThreadIdInput};
    use serde_json::json;

    #[test]
    fn extract_reply_target_roundtrip() {
        let io = slack_inbound_io_context("T123", "U456", "C789", Some("171234.567"), "EvABC");
        let meta = wrap_in_meta_context(&io);

        let extracted = extract_reply_target(&meta).unwrap();
        assert_eq!(extracted.kind, "slack_post");
        assert_eq!(extracted.address, "C789");
        assert_eq!(
            extracted.params.get("thread_ts").and_then(|v| v.as_str()),
            Some("171234.567")
        );
    }

    #[test]
    fn extract_slack_post_target_parses() {
        let io = slack_inbound_io_context("T123", "U456", "C789", None, "EvABC");
        let meta = wrap_in_meta_context(&io);
        let t = extract_slack_post_target(&meta).unwrap();
        assert_eq!(
            t,
            SlackPostTarget {
                channel_id: "C789".to_string(),
                thread_ts: None,
                workspace_id: Some("T123".to_string()),
            }
        );
    }

    #[test]
    fn slack_thread_id_falls_back_to_channel_id_when_thread_ts_missing() {
        let io = slack_inbound_io_context("T123", "U456", "C789", None, "EvABC");
        let expected = compute_thread_id(ThreadIdInput::PersistentChannel {
            channel_type: "slack",
            entrypoint_id: Some("T123"),
            conversation_id: "C789",
        })
        .expect("thread id");
        assert_eq!(
            io.conversation.thread_id.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            io.reply_target
                .params
                .get("thread_ts")
                .and_then(|v| v.as_str()),
            None
        );
    }

    #[test]
    fn slack_native_thread_uses_canonical_thread_id() {
        let io = slack_inbound_io_context("T123", "U456", "C789", Some("171234.567"), "EvABC");
        let expected = compute_thread_id(ThreadIdInput::NativeThread {
            channel_type: "slack",
            entrypoint_id: Some("T123"),
            conversation_id: "C789",
            native_thread_id: "171234.567",
        })
        .expect("thread id");
        assert_eq!(
            io.conversation.thread_id.as_deref(),
            Some(expected.as_str())
        );
    }

    fn sample_response_contract() -> Value {
        json!({
            "kind":"json_object_v1",
            "required":["success","human_message"],
            "properties":{
                "success":{"type":"boolean"},
                "human_message":{"type":"string"},
                "error_code":{"type":"string","enum":["missing_data","unknown"]}
            }
        })
    }

    #[test]
    fn set_and_extract_response_envelope_roundtrip() {
        let context = set_response_envelope(Some(wrap_in_meta_context(&slack_inbound_io_context(
            "T123", "U456", "C789", None, "EvABC",
        ))), sample_response_contract())
        .expect("set envelope");
        let extracted = extract_response_envelope(&context)
            .expect("extract ok")
            .expect("envelope present");
        assert_eq!(extracted, sample_response_contract());
        assert!(context.get("io").is_some());
    }

    #[test]
    fn set_final_response_contract_roundtrip() {
        let context = set_final_response_contract(None, sample_response_contract())
            .expect("set contract");
        let extracted = extract_final_response_contract(&context)
            .expect("extract ok")
            .expect("contract present");
        assert_eq!(extracted, sample_response_contract());
    }

    #[test]
    fn set_response_envelope_rejects_non_object_meta_context() {
        let err = set_response_envelope(Some(json!("bad")), sample_response_contract())
            .expect_err("should reject non-object context");
        assert!(matches!(err, IoResponseContractError::InvalidMetaContext(_)));
    }

    #[test]
    fn extract_response_envelope_rejects_invalid_contract_shape() {
        let err = extract_response_envelope(&json!({
            "response_envelope": {
                "kind": "json_object_v1",
                "required": ["ok"]
            }
        }))
        .expect_err("should reject invalid contract");
        assert!(matches!(err, IoResponseContractError::InvalidResponseContract(_)));
    }

    #[test]
    fn parse_structured_response_text_accepts_valid_json() {
        let parsed = parse_structured_response_text(
            r#"{"success":true,"human_message":"ok","error_code":"unknown"}"#,
            &sample_response_contract(),
        )
        .expect("valid response");
        assert_eq!(parsed.get("human_message").and_then(Value::as_str), Some("ok"));
    }

    #[test]
    fn parse_structured_response_text_rejects_missing_required_field() {
        let err = parse_structured_response_text(
            r#"{"success":true}"#,
            &sample_response_contract(),
        )
        .expect_err("missing required field");
        assert!(matches!(err, IoResponseContractError::InvalidStructuredResponse(_)));
    }

    #[test]
    fn parse_structured_response_text_rejects_optional_null_value() {
        let err = parse_structured_response_text(
            r#"{"success":true,"human_message":"ok","error_code":null}"#,
            &sample_response_contract(),
        )
        .expect_err("optional field present as null should fail");
        assert!(matches!(err, IoResponseContractError::InvalidStructuredResponse(_)));
    }

    #[test]
    fn parse_structured_response_text_rejects_unexpected_property() {
        let err = parse_structured_response_text(
            r#"{"success":true,"human_message":"ok","extra":"nope"}"#,
            &sample_response_contract(),
        )
        .expect_err("unexpected property should fail");
        assert!(matches!(err, IoResponseContractError::InvalidStructuredResponse(_)));
    }

    #[test]
    fn parse_structured_response_payload_reads_text_payload() {
        let payload = json!({
            "type": "text",
            "content": r#"{"success":true,"human_message":"ok"}"#,
            "attachments": []
        });
        let parsed = parse_structured_response_payload(&payload, &sample_response_contract())
            .expect("parse payload");
        assert_eq!(parsed.get("success").and_then(Value::as_bool), Some(true));
    }
}
