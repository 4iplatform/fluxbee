use serde_json::json;

use crate::node_client::NodeError;
use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_NODE_STATUS_GET, MSG_NODE_STATUS_GET_RESPONSE,
    SYSTEM_KIND,
};
use crate::split::NodeSender;

fn normalize_health_state(raw: &str) -> &'static str {
    match raw.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => "HEALTHY",
        "DEGRADED" => "DEGRADED",
        "ERROR" => "ERROR",
        "UNKNOWN" => "UNKNOWN",
        _ => "HEALTHY",
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        })
        .unwrap_or(default)
}

pub async fn try_handle_default_node_status(
    sender: &NodeSender,
    incoming: &Message,
) -> Result<bool, NodeError> {
    if incoming.meta.msg_type != SYSTEM_KIND
        || incoming.meta.msg.as_deref() != Some(MSG_NODE_STATUS_GET)
    {
        return Ok(false);
    }
    if !env_bool("NODE_STATUS_DEFAULT_HANDLER_ENABLED", true) {
        return Ok(false);
    }

    let health_state = std::env::var("NODE_STATUS_DEFAULT_HEALTH_STATE")
        .ok()
        .as_deref()
        .map(normalize_health_state)
        .unwrap_or("HEALTHY");
    let payload = json!({
        "status": "ok",
        "health_state": health_state,
    });
    let response = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(incoming.routing.src.clone()),
            ttl: 16,
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_NODE_STATUS_GET_RESPONSE.to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload,
    };
    sender.send(response).await?;
    Ok(true)
}
