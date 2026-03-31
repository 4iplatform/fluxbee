use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_CONFIG_GET, MSG_CONFIG_RESPONSE, MSG_CONFIG_SET,
    SYSTEM_KIND,
};

pub const NODE_CONFIG_CONTROL_TARGET: &str = "node_config_control";
pub const NODE_CONFIG_APPLY_MODE_REPLACE: &str = "replace";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NodeConfigGetPayload {
    pub node_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeConfigSetPayload {
    pub node_name: String,
    pub schema_version: u32,
    pub config_version: u64,
    pub apply_mode: String,
    #[serde(default)]
    pub config: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for NodeConfigSetPayload {
    fn default() -> Self {
        Self {
            node_name: String::new(),
            schema_version: 1,
            config_version: 0,
            apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
            config: Value::Object(Map::new()),
            request_id: None,
            contract_version: None,
            requested_by: None,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NodeConfigControlResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub node_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeConfigEnvelopeOptions {
    pub ttl: u8,
    pub src_ilk: Option<String>,
    pub scope: Option<String>,
    pub context: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeConfigControlRequest {
    Get(NodeConfigGetPayload),
    Set(NodeConfigSetPayload),
}

#[derive(Debug, thiserror::Error)]
pub enum NodeConfigControlError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid message: {0}")]
    InvalidMessage(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn is_node_config_get_message(msg: &Message) -> bool {
    msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some(MSG_CONFIG_GET)
}

pub fn is_node_config_set_message(msg: &Message) -> bool {
    msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some(MSG_CONFIG_SET)
}

pub fn is_node_config_response_message(msg: &Message) -> bool {
    msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some(MSG_CONFIG_RESPONSE)
}

pub fn parse_node_config_request(
    msg: &Message,
) -> Result<NodeConfigControlRequest, NodeConfigControlError> {
    match msg.meta.msg.as_deref() {
        Some(MSG_CONFIG_GET) if msg.meta.msg_type == SYSTEM_KIND => Ok(
            NodeConfigControlRequest::Get(serde_json::from_value(msg.payload.clone())?),
        ),
        Some(MSG_CONFIG_SET) if msg.meta.msg_type == SYSTEM_KIND => Ok(
            NodeConfigControlRequest::Set(serde_json::from_value(msg.payload.clone())?),
        ),
        Some(other) if msg.meta.msg_type == SYSTEM_KIND => {
            Err(NodeConfigControlError::InvalidMessage(format!(
                "unsupported system msg '{other}' for node config control"
            )))
        }
        _ => Err(NodeConfigControlError::InvalidMessage(
            "message is not a node config control request".to_string(),
        )),
    }
}

pub fn parse_node_config_response(
    msg: &Message,
) -> Result<NodeConfigControlResponse, NodeConfigControlError> {
    if !is_node_config_response_message(msg) {
        return Err(NodeConfigControlError::InvalidMessage(
            "message is not CONFIG_RESPONSE".to_string(),
        ));
    }
    Ok(serde_json::from_value(msg.payload.clone())?)
}

pub fn build_node_config_get_message(
    src_uuid: &str,
    target_node: &str,
    payload: &NodeConfigGetPayload,
    options: &NodeConfigEnvelopeOptions,
    trace_id: &str,
) -> Result<Message, NodeConfigControlError> {
    build_node_config_message(
        src_uuid,
        target_node,
        MSG_CONFIG_GET,
        serde_json::to_value(payload)?,
        options,
        trace_id,
    )
}

pub fn build_node_config_set_message(
    src_uuid: &str,
    target_node: &str,
    payload: &NodeConfigSetPayload,
    options: &NodeConfigEnvelopeOptions,
    trace_id: &str,
) -> Result<Message, NodeConfigControlError> {
    build_node_config_message(
        src_uuid,
        target_node,
        MSG_CONFIG_SET,
        serde_json::to_value(payload)?,
        options,
        trace_id,
    )
}

pub fn build_node_config_response_message(
    incoming: &Message,
    src_uuid: &str,
    payload: Value,
) -> Message {
    Message {
        routing: build_reply_routing(incoming, src_uuid),
        meta: build_response_meta(incoming),
        payload,
    }
}

pub fn build_node_config_response_message_runtime_src(
    incoming: &Message,
    payload: Value,
) -> Message {
    Message {
        routing: build_reply_routing(incoming, ""),
        meta: build_response_meta(incoming),
        payload,
    }
}

fn build_node_config_message(
    src_uuid: &str,
    target_node: &str,
    request_msg: &str,
    payload: Value,
    options: &NodeConfigEnvelopeOptions,
    trace_id: &str,
) -> Result<Message, NodeConfigControlError> {
    let src_uuid = src_uuid.trim();
    if src_uuid.is_empty() {
        return Err(NodeConfigControlError::InvalidRequest(
            "src_uuid must be non-empty".to_string(),
        ));
    }
    let target_node = target_node.trim();
    if target_node.is_empty() {
        return Err(NodeConfigControlError::InvalidRequest(
            "target_node must be non-empty".to_string(),
        ));
    }
    let trace_id = trace_id.trim();
    if trace_id.is_empty() {
        return Err(NodeConfigControlError::InvalidRequest(
            "trace_id must be non-empty".to_string(),
        ));
    }
    Ok(Message {
        routing: Routing {
            src: src_uuid.to_string(),
            dst: Destination::Unicast(target_node.to_string()),
            ttl: normalize_ttl(options.ttl),
            trace_id: trace_id.to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(request_msg.to_string()),
            src_ilk: options.src_ilk.clone(),
            scope: options.scope.clone(),
            target: Some(NODE_CONFIG_CONTROL_TARGET.to_string()),
            action: Some(request_msg.to_string()),
            priority: None,
            context: options.context.clone(),
            ..Meta::default()
        },
        payload,
    })
}

fn build_reply_routing(msg: &Message, src_uuid: &str) -> Routing {
    let dst = match &msg.routing.src {
        value if value.trim().is_empty() => Destination::Broadcast,
        value => Destination::Unicast(value.to_string()),
    };

    Routing {
        src: src_uuid.to_string(),
        dst,
        ttl: normalize_ttl(msg.routing.ttl),
        trace_id: msg.routing.trace_id.clone(),
    }
}

fn build_response_meta(msg: &Message) -> Meta {
    Meta {
        msg_type: SYSTEM_KIND.to_string(),
        msg: Some(MSG_CONFIG_RESPONSE.to_string()),
        src_ilk: msg.meta.src_ilk.clone(),
        scope: msg.meta.scope.clone(),
        target: Some(NODE_CONFIG_CONTROL_TARGET.to_string()),
        action: Some(MSG_CONFIG_RESPONSE.to_string()),
        priority: msg.meta.priority.clone(),
        context: msg.meta.context.clone(),
        ..Meta::default()
    }
}

fn normalize_ttl(ttl: u8) -> u8 {
    ttl.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_and_parse_config_get_message_roundtrip() {
        let payload = NodeConfigGetPayload {
            node_name: "AI.chat@motherbee".to_string(),
            requested_by: Some("archi".to_string()),
            ..Default::default()
        };
        let msg = build_node_config_get_message(
            "node-src-1",
            "AI.chat@motherbee",
            &payload,
            &NodeConfigEnvelopeOptions::default(),
            "trace-1",
        )
        .unwrap();

        assert!(is_node_config_get_message(&msg));
        assert_eq!(msg.meta.target.as_deref(), Some(NODE_CONFIG_CONTROL_TARGET));
        assert_eq!(msg.meta.action.as_deref(), Some(MSG_CONFIG_GET));

        let parsed = parse_node_config_request(&msg).unwrap();
        assert_eq!(parsed, NodeConfigControlRequest::Get(payload));
    }

    #[test]
    fn build_and_parse_config_set_message_roundtrip() {
        let payload = NodeConfigSetPayload {
            node_name: "AI.chat@motherbee".to_string(),
            schema_version: 1,
            config_version: 7,
            apply_mode: NODE_CONFIG_APPLY_MODE_REPLACE.to_string(),
            config: json!({"behavior":{"kind":"openai_chat"}}),
            request_id: Some("req-1".to_string()),
            contract_version: Some(1),
            requested_by: Some("archi".to_string()),
            extra: Map::new(),
        };
        let options = NodeConfigEnvelopeOptions {
            ttl: 9,
            src_ilk: Some("ilk:test".to_string()),
            scope: Some("global".to_string()),
            context: Some(json!({"session_id":"abc"})),
        };
        let msg = build_node_config_set_message(
            "node-src-2",
            "AI.chat@motherbee",
            &payload,
            &options,
            "trace-2",
        )
        .unwrap();

        assert!(is_node_config_set_message(&msg));
        assert_eq!(msg.routing.ttl, 9);
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:test"));

        let parsed = parse_node_config_request(&msg).unwrap();
        assert_eq!(parsed, NodeConfigControlRequest::Set(payload));
    }

    #[test]
    fn build_config_response_uses_same_trace_id() {
        let incoming = build_node_config_get_message(
            "caller-1",
            "AI.chat@motherbee",
            &NodeConfigGetPayload {
                node_name: "AI.chat@motherbee".to_string(),
                ..Default::default()
            },
            &NodeConfigEnvelopeOptions::default(),
            "trace-3",
        )
        .unwrap();

        let response = build_node_config_response_message(
            &incoming,
            "runtime-uuid-1",
            json!({"ok":true,"node_name":"AI.chat@motherbee"}),
        );

        assert!(is_node_config_response_message(&response));
        assert_eq!(response.routing.trace_id, "trace-3");
        assert!(matches!(
            response.routing.dst,
            Destination::Unicast(ref value) if value == "caller-1"
        ));
        assert_eq!(response.meta.msg.as_deref(), Some(MSG_CONFIG_RESPONSE));
    }

    #[test]
    fn parse_config_response_accepts_open_payload() {
        let msg = Message {
            routing: Routing {
                src: "runtime-1".to_string(),
                dst: Destination::Unicast("caller-1".to_string()),
                ttl: 16,
                trace_id: "trace-4".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_CONFIG_RESPONSE.to_string()),
                src_ilk: None,
                scope: None,
                target: Some(NODE_CONFIG_CONTROL_TARGET.to_string()),
                action: Some(MSG_CONFIG_RESPONSE.to_string()),
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload: json!({
                "ok": true,
                "node_name": "AI.chat@motherbee",
                "state": "configured",
                "schema_version": 1,
                "config_version": 7,
                "contract": {"supports":["CONFIG_GET","CONFIG_SET"]},
                "extra_field": true
            }),
        };

        let parsed = parse_node_config_response(&msg).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.node_name, "AI.chat@motherbee");
        assert_eq!(parsed.state.as_deref(), Some("configured"));
        assert_eq!(parsed.extra.get("extra_field"), Some(&Value::Bool(true)));
    }
}
