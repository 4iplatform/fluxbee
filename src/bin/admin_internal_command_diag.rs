use std::error::Error;
use std::time::Duration;

use fluxbee_sdk::{admin_command, connect, AdminCommandRequest, NodeConfig};
use serde_json::json;
use uuid::Uuid;

type DiagError = Box<dyn Error + Send + Sync>;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_params() -> Result<serde_json::Value, DiagError> {
    let raw = env_or("ADMIN_PARAMS_JSON", "{}");
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    if !value.is_object() && !value.is_null() {
        return Err("ADMIN_PARAMS_JSON must be a JSON object or null".into());
    }
    Ok(value)
}

#[tokio::main]
async fn main() -> Result<(), DiagError> {
    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();

    let action = env_or("ADMIN_ACTION", "inventory");
    let admin_target = env_or("ADMIN_TARGET", "SY.admin@motherbee");
    let timeout_secs = env_u64("ADMIN_TIMEOUT_SECS", 15);
    let expect_status = env_non_empty("ADMIN_EXPECT_STATUS");
    let request_id =
        env_non_empty("ADMIN_REQUEST_ID").unwrap_or_else(|| Uuid::new_v4().to_string());
    let payload_target = env_non_empty("ADMIN_PAYLOAD_TARGET");
    let params = parse_params()?;
    let node_name = env_non_empty("ADMIN_NODE_NAME")
        .unwrap_or_else(|| format!("WF.admin.internal.diag.{}", Uuid::new_v4().simple()));

    let node_config = NodeConfig {
        name: node_name,
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir,
        version: "1.0".to_string(),
    };
    let (sender, mut receiver) = connect(&node_config).await?;

    let out = admin_command(
        &sender,
        &mut receiver,
        AdminCommandRequest {
            admin_target: &admin_target,
            action: &action,
            target: payload_target.as_deref(),
            params,
            request_id: Some(&request_id),
            timeout: Duration::from_secs(timeout_secs),
        },
    )
    .await?;

    if let Some(expected) = expect_status.as_deref() {
        if out.status != expected {
            return Err(format!(
                "unexpected status: expected={} got={}",
                expected, out.status
            )
            .into());
        }
    }

    let response = json!({
        "status": out.status,
        "action": out.action,
        "payload": out.payload,
        "error_code": out.error_code,
        "error_detail": out.error_detail,
        "request_id": out.request_id,
        "trace_id": out.trace_id,
    });
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
