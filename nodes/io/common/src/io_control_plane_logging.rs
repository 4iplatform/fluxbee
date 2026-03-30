use crate::io_control_plane::IoControlPlaneState;
use fluxbee_sdk::protocol::Message;

pub fn log_config_get_served(trace_id: &str, node_name: &str, state: &IoControlPlaneState) {
    tracing::info!(
        trace_id = trace_id,
        node_name = node_name,
        lifecycle_state = state.current_state.as_str(),
        config_source = state.config_source.as_str(),
        schema_version = state.schema_version,
        config_version = state.config_version,
        "control-plane CONFIG_GET served"
    );
}

pub fn log_control_plane_request_rejected(
    trace_id: &str,
    node_name: &str,
    error_code: &str,
    error_detail: &str,
) {
    tracing::warn!(
        trace_id = trace_id,
        node_name = node_name,
        error_code = error_code,
        error_detail = error_detail,
        "control-plane request rejected"
    );
}

pub fn log_config_set_stale_rejected(
    node_name: &str,
    incoming_config_version: u64,
    current_config_version: u64,
) {
    tracing::warn!(
        node_name = node_name,
        incoming_config_version = incoming_config_version,
        current_config_version = current_config_version,
        "CONFIG_SET rejected: stale config_version"
    );
}

pub fn log_config_set_invalid_runtime_credentials(
    node_name: &str,
    config_version: u64,
    error_detail: &str,
) {
    tracing::warn!(
        node_name = node_name,
        config_version = config_version,
        error_code = "invalid_config",
        error_detail = error_detail,
        "CONFIG_SET rejected: invalid adapter credentials for runtime reload"
    );
}

pub fn log_config_set_persist_error(
    node_name: &str,
    schema_version: u32,
    config_version: u64,
    error_detail: &str,
) {
    tracing::error!(
        node_name = node_name,
        config_version = config_version,
        schema_version = schema_version,
        error_code = "config_persist_error",
        error_detail = error_detail,
        "CONFIG_SET failed: cannot persist IO control-plane state"
    );
}

pub fn log_config_set_applied(
    node_name: &str,
    schema_version: u32,
    config_version: u64,
    hot_applied: &[String],
    reinit_performed: &[String],
    restart_required: &[String],
) {
    tracing::info!(
        node_name = node_name,
        schema_version = schema_version,
        config_version = config_version,
        hot_applied = render_apply_list(hot_applied),
        reinit_performed = render_apply_list(reinit_performed),
        restart_required = render_apply_list(restart_required),
        "CONFIG_SET applied"
    );
}

pub fn log_config_response_message(msg: &Message, source: &str) -> bool {
    let msg_kind = msg.meta.msg.as_deref().unwrap_or("");
    if !msg.meta.msg_type.eq_ignore_ascii_case("system")
        || !msg_kind.eq_ignore_ascii_case("CONFIG_RESPONSE")
    {
        return false;
    }

    let ok = msg
        .payload
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let node_name = msg
        .payload
        .get("node_name")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let state = msg
        .payload
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let schema_version = msg
        .payload
        .get("schema_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let config_version = msg
        .payload
        .get("config_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let error_code = msg
        .payload
        .get("error")
        .and_then(|v| v.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let error_message = msg
        .payload
        .get("error")
        .and_then(|v| v.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    if ok {
        tracing::info!(
            source = source,
            trace_id = %msg.routing.trace_id,
            node_name = node_name,
            state = state,
            schema_version = schema_version,
            config_version = config_version,
            "received CONFIG_RESPONSE ok"
        );
    } else {
        tracing::warn!(
            source = source,
            trace_id = %msg.routing.trace_id,
            node_name = node_name,
            state = state,
            schema_version = schema_version,
            config_version = config_version,
            error_code = error_code,
            error_message = error_message,
            "received CONFIG_RESPONSE error"
        );
    }

    true
}

fn render_apply_list(entries: &[String]) -> String {
    if entries.is_empty() {
        return "-".to_string();
    }
    entries.join(", ")
}
