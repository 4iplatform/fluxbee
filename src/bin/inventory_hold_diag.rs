use std::error::Error;
use std::time::Duration;

use fluxbee_sdk::protocol::SYSTEM_KIND;
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, OperationalRouteProfile, RouteMatch, RouteTarget,
    RouterDispatcher,
};
use tracing_subscriber::EnvFilter;

type DynError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let node_name = env_or("INVENTORY_HOLD_NODE_NAME", "WF.inventory.hold");
    let node_version = env_or("INVENTORY_HOLD_NODE_VERSION", "0.0.1");
    let hold_secs = env_u64("INVENTORY_HOLD_SECS", 0);

    let node_cfg = NodeConfig {
        name: node_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: json_router::paths::config_dir(),
        version: node_version,
    };

    let profile = OperationalRouteProfile::builder()
        .command_channel("system")
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command("system"),
        )
        .build()
        .map_err(|err| format!("inventory_hold_diag rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_cfg, Duration::from_secs(1), profile).await?;
    tracing::info!("inventory hold diag connected");

    let dispatcher_status = dispatcher.clone();
    tokio::spawn(async move {
        let mut system_rx = match dispatcher_status.take_command_receiver("system").await {
            Ok(rx) => rx,
            Err(err) => {
                tracing::warn!(error = %err, "inventory hold diag system receiver");
                return;
            }
        };
        let sender = dispatcher_status.sender_snapshot();
        while let Some(message) = system_rx.recv().await {
            if let Err(err) = try_handle_default_node_status(&sender, &message).await {
                tracing::warn!(error = %err, "failed to handle default node status");
            }
        }
    });

    if hold_secs == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(Duration::from_secs(hold_secs)).await;
    }

    Ok(())
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}
