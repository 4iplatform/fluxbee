use std::time::Duration;

use router_client::NodeConfig;
use uuid::Uuid;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NodeConfig {
        name: env("NODE_NAME").unwrap_or_else(|| "IO.slack.T123".to_string()),
        island_id: env("ISLAND_ID").unwrap_or_else(|| "production".to_string()),
        node_uuid: env("NODE_UUID").unwrap_or_else(|| Uuid::new_v4().to_string()),
        version: env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string()),
        router_addr: env("ROUTER_ADDR").unwrap_or_else(|| "127.0.0.1:9000".to_string()),
        queue_capacity: env("QUEUE_CAPACITY")
            .and_then(|v| v.parse().ok())
            .unwrap_or(256),
    };

    let (sender, mut receiver) = router_client::connect(config).await?;

    println!("connected: {} (vpn_id={})", receiver.full_name(), receiver.vpn_id());
    println!("router: {}", receiver.router_name());

    // Optional: wait briefly for a message.
    let _ = receiver.recv_timeout(Duration::from_millis(200)).await;

    sender.close().await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}
