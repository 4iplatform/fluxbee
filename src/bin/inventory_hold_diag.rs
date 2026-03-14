use std::error::Error;
use std::time::Duration;

use fluxbee_sdk::{connect, try_handle_default_node_status, NodeConfig};
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
        config_dir: json_router::paths::config_dir(),
        version: node_version,
    };

    let (sender, mut receiver) = connect(&node_cfg).await?;
    tracing::info!("inventory hold diag connected");

    let status_sender = sender.clone();
    tokio::spawn(async move {
        loop {
            let message = match receiver.recv().await {
                Ok(msg) => msg,
                Err(err) => {
                    tracing::warn!(error = %err, "inventory hold diag receiver ended");
                    break;
                }
            };
            if let Err(err) = try_handle_default_node_status(&status_sender, &message).await {
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
