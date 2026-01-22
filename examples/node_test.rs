use std::path::PathBuf;

use json_router::node_client::{NodeClient, NodeConfig};
use json_router::protocol::{
    build_echo, build_echo_reply, build_time_sync, build_withdraw, Destination, Message, Meta,
    Routing, TimeSyncPayload,
};
use serde_json::json;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from("/etc/json-router");
    let socket_dir = PathBuf::from("/var/run/json-router/routers");
    let state_dir = PathBuf::from("/var/lib/json-router/state");
    let nodes_dir = state_dir.join("nodes");

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("echo");
    let target = args.get(2).map(|s| s.as_str()).unwrap_or("WF.listen");
    let node_name = match mode {
        "listen" => "WF.listen",
        _ => "WF.echo",
    };

    let island_id = load_island_id(&config_dir)?;
    let router_socket = match std::env::var("JSR_ROUTER_NAME") {
        Ok(router_name) => {
            let router_l2_name = ensure_l2_name(&router_name, &island_id);
            match load_router_uuid(&state_dir, &router_l2_name) {
                Ok(router_uuid) => socket_dir.join(format!("{}.sock", router_uuid.simple())),
                Err(_) => socket_dir.clone(),
            }
        }
        Err(_) => socket_dir.clone(),
    };
    let target_name = normalize_target(target, &island_id);
    println!(
        "connecting: name={} socket={} config={} uuid_dir={}",
        node_name,
        router_socket.display(),
        config_dir.display(),
        nodes_dir.display()
    );

    let mut client = NodeClient::connect(NodeConfig {
        name: node_name.to_string(),
        router_socket,
        uuid_persistence_dir: nodes_dir,
        config_dir,
        version: "1.0".to_string(),
    })
    .await?;

    println!(
        "connected as {} (uuid={}, vpn={}, router={})",
        client.name(),
        client.uuid(),
        client.vpn_id(),
        client.router_name()
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
        let (dst, target_name) = if mode == "unicast" {
            (Destination::Resolve, Some(target_name.clone()))
        } else {
            (Destination::Broadcast, None)
        };
        let msg = Message {
            routing: Routing {
                src: client.uuid().to_string(),
                dst,
                ttl: 1,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: None,
                target: target_name,
                action: None,
                priority: None,
                context: None,
            },
            payload: json!({"type": "text", "content": format!("HOLA {}", seq)}),
        };
        client.send(&msg).await?;
        println!("sent HOLA {}", seq);

        let trace_id = Uuid::new_v4().to_string();
        let echo = build_echo(&client.uuid().to_string(), Destination::Broadcast, &trace_id);
        client.send(&echo).await?;
        println!("sent ECHO");

        let trace_id = Uuid::new_v4().to_string();
        let echo_reply =
            build_echo_reply(&client.uuid().to_string(), Destination::Broadcast, &trace_id);
        client.send(&echo_reply).await?;
        println!("sent ECHO_REPLY");

        let trace_id = Uuid::new_v4().to_string();
        let now_ms = now_epoch_ms();
        let time_sync = build_time_sync(
            &client.uuid().to_string(),
            Destination::Broadcast,
            &trace_id,
            TimeSyncPayload {
                timestamp_utc: "1970-01-01T00:00:00Z".to_string(),
                epoch_ms: now_ms,
                seq,
            },
        );
        client.send(&time_sync).await?;
        println!("sent TIME_SYNC");

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

fn load_island_id(config_dir: &PathBuf) -> Result<String, Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(config_dir.join("island.yaml"))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let island_id = value
        .get("island_id")
        .and_then(|v| v.as_str())
        .ok_or("missing island_id")?;
    Ok(island_id.to_string())
}

fn normalize_target(target: &str, island_id: &str) -> String {
    if target.contains('@') {
        target.to_string()
    } else {
        format!("{}@{}", target, island_id)
    }
}

fn ensure_l2_name(name: &str, island_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, island_id)
    }
}

fn load_router_uuid(
    state_dir: &PathBuf,
    router_l2_name: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(
        state_dir
            .join(router_l2_name)
            .join("identity.yaml"),
    )?;
    let value: serde_yaml::Value = serde_yaml::from_str(&data)?;
    let uuid = value
        .get("layer1")
        .and_then(|v| v.get("uuid"))
        .and_then(|v| v.as_str())
        .ok_or("missing layer1.uuid")?;
    Ok(Uuid::parse_str(uuid)?)
}
