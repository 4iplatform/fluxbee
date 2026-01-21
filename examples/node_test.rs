use std::path::PathBuf;

use json_router::node_client::{NodeClient, NodeConfig};
use json_router::protocol::{Destination, Message, Meta, Routing};
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = std::env::var("JSR_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/json-router"));
    let socket_dir = std::env::var("JSR_SOCKET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/run/json-router"));
    let nodes_dir = std::env::var("JSR_NODE_UUID_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/json-router/nodes"));

    let mut client = NodeClient::connect(NodeConfig {
        name: "WF.test".to_string(),
        router_socket: socket_dir.join("router.sock"),
        uuid_persistence_dir: nodes_dir,
        config_dir,
        version: "1.0".to_string(),
    })
    .await?;

    println!(
        "connected as {} (uuid={}, vpn={})",
        client.name(),
        client.uuid(),
        client.vpn_id()
    );

    let msg = Message {
        routing: Routing {
            src: client.uuid().to_string(),
            dst: Destination::Broadcast,
            ttl: 1,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            target: None,
            action: None,
            priority: None,
            context: None,
        },
        payload: json!({"type": "text", "content": "HOLA"}),
    };

    client.send(&msg).await?;
    println!("sent HOLA broadcast");

    loop {
        let _ = client.recv().await?;
    }
}
