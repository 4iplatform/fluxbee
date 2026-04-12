use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{Duration, Instant as TokioInstant};
use uuid::Uuid;

use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::{classify_admin_action, ActionResult, NodeError, NodeReceiver, NodeSender};

pub const ADMIN_KIND: &str = "admin";
pub const MSG_ADMIN_COMMAND: &str = "ADMIN_COMMAND";
pub const MSG_ADMIN_COMMAND_RESPONSE: &str = "ADMIN_COMMAND_RESPONSE";

#[derive(Debug, Clone)]
pub struct AdminCommandRequest<'a> {
    pub admin_target: &'a str,
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub params: Value,
    pub request_id: Option<&'a str>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct AdminCommandResult {
    pub status: String,
    pub action: String,
    pub payload: Value,
    pub action_result: Option<ActionResult>,
    pub result_origin: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<Value>,
    pub request_id: Option<String>,
    pub trace_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AdminCommandError {
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    #[error("admin transport unreachable: reason={reason}, original_dst={original_dst}")]
    Unreachable {
        reason: String,
        original_dst: String,
    },
    #[error("admin transport ttl exceeded: original_dst={original_dst}, last_hop={last_hop}")]
    TtlExceeded {
        original_dst: String,
        last_hop: String,
    },
    #[error("timeout waiting ADMIN_COMMAND_RESPONSE trace_id={trace_id} target={target} action={action} timeout_ms={timeout_ms}")]
    Timeout {
        trace_id: String,
        target: String,
        action: String,
        timeout_ms: u64,
    },
    #[error("ADMIN_COMMAND rejected: action={action}, error_code={error_code}, message={message}")]
    Rejected {
        action: String,
        error_code: String,
        message: String,
    },
}

#[derive(Debug, Deserialize, Default)]
struct UnreachablePayload {
    #[serde(default)]
    original_dst: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize, Default)]
struct TtlExceededPayload {
    #[serde(default)]
    original_dst: String,
    #[serde(default)]
    last_hop: String,
}

/// Sends `ADMIN_COMMAND` to `SY.admin@<hive>` and waits for
/// `ADMIN_COMMAND_RESPONSE` with matching `trace_id`.
pub async fn admin_command(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    request: AdminCommandRequest<'_>,
) -> Result<AdminCommandResult, AdminCommandError> {
    let admin_target = request.admin_target.trim();
    if admin_target.is_empty() {
        return Err(AdminCommandError::InvalidRequest(
            "admin_target must be non-empty".to_string(),
        ));
    }
    let action = request.action.trim();
    if action.is_empty() {
        return Err(AdminCommandError::InvalidRequest(
            "action must be non-empty".to_string(),
        ));
    }
    if !request.params.is_null() && !request.params.is_object() {
        return Err(AdminCommandError::InvalidRequest(
            "params must be a JSON object or null".to_string(),
        ));
    }
    let payload_target = request
        .target
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let request_id = request
        .request_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let timeout = default_timeout(request.timeout);

    let trace_id = Uuid::new_v4().to_string();
    let mut payload = json!({
        "action": action,
        "params": request.params,
        "request_id": request_id,
    });
    if let Some(target) = payload_target {
        payload["target"] = Value::String(target.to_string());
    }

    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(admin_target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: ADMIN_KIND.to_string(),
            msg: Some(MSG_ADMIN_COMMAND.to_string()),
            src_ilk: None,
            scope: None,
            target: Some(admin_target.to_string()),
            action: Some(action.to_string()),
            action_class: classify_admin_action(action),
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload,
    };
    sender.send(msg).await?;

    let incoming = wait_admin_response(receiver, admin_target, action, &trace_id, timeout).await?;
    parse_admin_response(action, trace_id, incoming)
}

/// Like [`admin_command`], but enforces payload `status="ok"`.
pub async fn admin_command_ok(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    request: AdminCommandRequest<'_>,
) -> Result<AdminCommandResult, AdminCommandError> {
    let out = admin_command(sender, receiver, request).await?;
    if out.status == "ok" {
        return Ok(out);
    }
    Err(AdminCommandError::Rejected {
        action: out.action,
        error_code: out.error_code.unwrap_or_else(|| "UNKNOWN".to_string()),
        message: extract_error_message(&out.payload, out.error_detail.as_ref())
            .unwrap_or("admin returned non-ok status")
            .to_string(),
    })
}

fn default_timeout(timeout: Duration) -> Duration {
    if timeout.is_zero() {
        Duration::from_secs(5)
    } else {
        timeout
    }
}

async fn wait_admin_response(
    receiver: &mut NodeReceiver,
    target: &str,
    action: &str,
    trace_id: &str,
    timeout: Duration,
) -> Result<Message, AdminCommandError> {
    let deadline = TokioInstant::now() + timeout;
    loop {
        let now = TokioInstant::now();
        if now >= deadline {
            return Err(AdminCommandError::Timeout {
                trace_id: trace_id.to_string(),
                target: target.to_string(),
                action: action.to_string(),
                timeout_ms: timeout.as_millis() as u64,
            });
        }
        let remaining = deadline - now;
        let incoming = match receiver.recv_timeout(remaining).await {
            Ok(message) => message,
            Err(NodeError::Timeout) => {
                return Err(AdminCommandError::Timeout {
                    trace_id: trace_id.to_string(),
                    target: target.to_string(),
                    action: action.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                })
            }
            Err(err) => return Err(AdminCommandError::Node(err)),
        };
        if incoming.routing.trace_id != trace_id {
            continue;
        }
        if incoming.meta.msg_type == SYSTEM_KIND {
            match incoming.meta.msg.as_deref() {
                Some(MSG_UNREACHABLE) => {
                    let payload = serde_json::from_value::<UnreachablePayload>(incoming.payload)
                        .unwrap_or_default();
                    return Err(AdminCommandError::Unreachable {
                        reason: payload.reason,
                        original_dst: payload.original_dst,
                    });
                }
                Some(MSG_TTL_EXCEEDED) => {
                    let payload = serde_json::from_value::<TtlExceededPayload>(incoming.payload)
                        .unwrap_or_default();
                    return Err(AdminCommandError::TtlExceeded {
                        original_dst: payload.original_dst,
                        last_hop: payload.last_hop,
                    });
                }
                _ => {}
            }
        }
        if incoming.meta.msg_type == ADMIN_KIND
            && incoming.meta.msg.as_deref() == Some(MSG_ADMIN_COMMAND_RESPONSE)
        {
            return Ok(incoming);
        }
    }
}

fn parse_admin_response(
    requested_action: &str,
    trace_id: String,
    message: Message,
) -> Result<AdminCommandResult, AdminCommandError> {
    let payload = message.payload;
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| AdminCommandError::InvalidResponse("missing status".to_string()))?
        .to_string();
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(requested_action)
        .to_string();
    let payload_value = admin_response_payload_value(&payload);
    let error_code = payload
        .get("error_code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let error_detail = payload.get("error_detail").cloned().and_then(|value| {
        if value.is_null() {
            None
        } else {
            Some(value)
        }
    });
    let request_id = payload
        .get("request_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(AdminCommandResult {
        status,
        action,
        payload: payload_value,
        action_result: message.meta.action_result,
        result_origin: message.meta.result_origin,
        error_code,
        error_detail,
        request_id,
        trace_id,
    })
}

fn admin_response_payload_value(payload: &Value) -> Value {
    if let Some(value) = payload.get("payload") {
        return value.clone();
    }
    let Some(mut object) = payload.as_object().cloned() else {
        return Value::Null;
    };
    for key in [
        "status",
        "action",
        "error_code",
        "error_detail",
        "request_id",
        "trace_id",
    ] {
        object.remove(key);
    }
    if object.is_empty() {
        Value::Null
    } else {
        Value::Object(object)
    }
}

fn extract_error_message<'a>(
    payload: &'a Value,
    error_detail: Option<&'a Value>,
) -> Option<&'a str> {
    if let Some(message) = error_detail
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return Some(message);
    }
    if let Some(message) = error_detail
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return Some(message);
    }
    payload
        .get("message")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn admin_response_message(payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "src".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("dst".to_string()),
                ttl: 16,
                trace_id: "trace-test".to_string(),
            },
            meta: Meta {
                msg_type: ADMIN_KIND.to_string(),
                msg: Some(MSG_ADMIN_COMMAND_RESPONSE.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    #[test]
    fn parse_admin_response_prefers_payload_field_when_present() {
        let raw = json!({
            "status": "ok",
            "action": "get_hive",
            "payload": {
                "hive_id": "worker-220"
            },
            "error_code": null,
            "error_detail": null
        });

        let parsed = parse_admin_response(
            "get_hive",
            "trace-1".to_string(),
            admin_response_message(raw),
        )
        .unwrap();
        assert_eq!(parsed.action, "get_hive");
        assert_eq!(parsed.payload, json!({ "hive_id": "worker-220" }));
    }

    #[test]
    fn parse_admin_response_falls_back_to_top_level_body_when_payload_missing() {
        let raw = json!({
            "status": "ok",
            "action": "get_status",
            "responses": [
                {
                    "hive": "motherbee",
                    "status": "ok",
                    "payload": {
                        "version": 10
                    }
                }
            ],
            "pending": [],
            "expected_hives_policy": ["motherbee"],
            "expected_hives_topology": ["motherbee"],
            "pending_hives_policy": [],
            "pending_hives_topology": [],
            "error_code": null,
            "error_detail": null
        });

        let parsed = parse_admin_response(
            "opa_get_status",
            "trace-2".to_string(),
            admin_response_message(raw),
        )
        .unwrap();
        assert_eq!(
            parsed.payload,
            json!({
                "responses": [
                    {
                        "hive": "motherbee",
                        "status": "ok",
                        "payload": {
                            "version": 10
                        }
                    }
                ],
                "pending": [],
                "expected_hives_policy": ["motherbee"],
                "expected_hives_topology": ["motherbee"],
                "pending_hives_policy": [],
                "pending_hives_topology": []
            })
        );
    }
}
