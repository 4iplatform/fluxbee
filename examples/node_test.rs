use std::path::PathBuf;

use json_router::node_client::{NodeClient, NodeConfig};
use json_router::protocol::{
    build_echo, build_echo_reply, build_time_sync, build_withdraw, Destination, Message, Meta,
    Routing, TimeSyncPayload,
};
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

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("echo");
    let node_name = match mode {
        "listen" => "WF.listen",
        _ => "WF.echo",
    };

    let mut client = NodeClient::connect(NodeConfig {
        name: node_name.to_string(),
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

    if mode == "listen" {
        println!("listen mode: waiting for messages");
        loop {
            let msg = client.recv().await?;
            println!(
                "received: src={} dst={:?} type={} msg={:?} payload={}",
                msg.routing.src,
                msg.routing.dst,
                msg.meta.msg_type,
                msg.meta.msg,
                msg.payload
            );
        }
    }

    let mut seq = 1u64;
    loop {
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
            payload: json!({\"type\": \"text\", \"content\": format!(\"HOLA {}\", seq)}),
        };
        client.send(&msg).await?;
        println!(\"sent HOLA {}\", seq);

        let trace_id = Uuid::new_v4().to_string();
        let echo = build_echo(&client.uuid().to_string(), Destination::Broadcast, &trace_id);
        client.send(&echo).await?;
        println!(\"sent ECHO\");

        let trace_id = Uuid::new_v4().to_string();
        let echo_reply =
            build_echo_reply(&client.uuid().to_string(), Destination::Broadcast, &trace_id);
        client.send(&echo_reply).await?;
        println!(\"sent ECHO_REPLY\");

        let trace_id = Uuid::new_v4().to_string();
        let now_ms = now_epoch_ms();
        let time_sync = build_time_sync(
            &client.uuid().to_string(),
            Destination::Broadcast,
            &trace_id,
            TimeSyncPayload {
                timestamp_utc: \"1970-01-01T00:00:00Z\".to_string(),
                epoch_ms: now_ms,
                seq,
            },
        );
        client.send(&time_sync).await?;
        println!(\"sent TIME_SYNC\");

        seq += 1;
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
