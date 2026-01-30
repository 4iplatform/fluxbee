use std::path::PathBuf;

use jsr_client::{connect, NodeConfig, NodeReceiver, NodeSender};
use jsr_client::protocol::{
    build_echo, build_echo_reply, build_time_sync, build_withdraw, Destination, Message, Meta,
    Routing, TimeSyncPayload, MSG_OPA_RELOAD, SYSTEM_KIND,
};
use serde_json::json;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("node_test supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let nodes_dir = state_dir.join("nodes");

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("echo");
    let target = args.get(2).map(|s| s.as_str()).unwrap_or("WF.listen");
    let node_name = match mode {
        "listen" => "WF.listen",
        "opa_reload" => "SY.opa.rules",
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

    let node_config = NodeConfig {
        name: node_name.to_string(),
        router_socket,
        uuid_persistence_dir: nodes_dir,
        config_dir,
        version: "1.0".to_string(),
    };
    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, std::time::Duration::from_secs(1)).await?;

    println!(
        "connected as {} (uuid={}, vpn={}, router={})",
        sender.full_name(),
        sender.uuid(),
        receiver.vpn_id(),
        receiver.full_name()
    );

    if mode == "listen" {
        println!("listen mode: waiting for messages");
        loop {
            match receiver.recv().await {
                Ok(msg) => {
                    let summary = payload_summary(&msg.payload);
                    println!(
                        "received: kind={} msg={:?} src={} dst={:?} payload={}",
                        msg.meta.msg_type,
                        msg.meta.msg,
                        msg.routing.src,
                        msg.routing.dst,
                        summary
                    );
                }
                Err(err) => {
                    eprintln!("recv error: {err} (reconnecting)");
                    let (new_sender, new_receiver) =
                        connect_with_retry(&node_config, std::time::Duration::from_secs(1))
                            .await?;
                    sender = new_sender;
                    receiver = new_receiver;
                    println!(
                        "reconnected as {} (uuid={}, vpn={}, router={})",
                        sender.full_name(),
                        sender.uuid(),
                        receiver.vpn_id(),
                        receiver.full_name()
                    );
                }
            }
        }
    }

    if mode == "opa_reload" {
        let version = args
            .get(2)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1);
        let msg = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                dst: Destination::Broadcast,
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_OPA_RELOAD.to_string()),
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: None,
            },
            payload: json!({"version": version, "hash": null}),
        };
        sender.send(msg).await?;
        println!("sent OPA_RELOAD version={version}");
        return Ok(());
    }

    let mut seq = 1u64;
    loop {
        if mode == "opa" {
            let msg = Message {
                routing: Routing {
                    src: sender.uuid().to_string(),
                    dst: Destination::Resolve,
                    ttl: 1,
                    trace_id: Uuid::new_v4().to_string(),
                },
                meta: Meta {
                    msg_type: "user".to_string(),
                    msg: None,
                    scope: None,
                    target: Some(target_name.clone()),
                    action: None,
                    priority: None,
                    context: Some(json!({"opa_test": true, "seq": seq})),
                },
                payload: json!({"type": "opa_test", "content": format!("OPA {}", seq)}),
            };
            if let Err(err) = sender.send(msg).await {
                eprintln!("send error: {err} (reconnecting)");
                let (new_sender, new_receiver) =
                    connect_with_retry(&node_config, std::time::Duration::from_secs(1)).await?;
                sender = new_sender;
                receiver = new_receiver;
                continue;
            }
            println!("sent OPA {}", seq);
            seq += 1;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        let (dst, target_name) = if mode == "unicast" {
            (Destination::Resolve, Some(target_name.clone()))
        } else {
            (Destination::Broadcast, None)
        };
        let msg = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                dst: dst.clone(),
                ttl: 1,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: None,
                scope: None,
                target: target_name,
                action: None,
                priority: None,
                context: None,
            },
            payload: json!({"type": "text", "content": format!("HOLA {}", seq)}),
        };
        if let Err(err) = sender.send(msg).await {
            eprintln!("send error: {err} (reconnecting)");
            let (new_sender, new_receiver) =
                connect_with_retry(&node_config, std::time::Duration::from_secs(1)).await?;
            sender = new_sender;
            receiver = new_receiver;
            continue;
        }
        println!("sent HOLA {} (dst={:?})", seq, dst);

        let trace_id = Uuid::new_v4().to_string();
        let echo = build_echo(&sender.uuid().to_string(), Destination::Broadcast, &trace_id);
        if let Err(err) = sender.send(echo).await {
            eprintln!("send error: {err} (reconnecting)");
            let (new_sender, new_receiver) =
                connect_with_retry(&node_config, std::time::Duration::from_secs(1)).await?;
            sender = new_sender;
            receiver = new_receiver;
            continue;
        }
        println!("sent ECHO (dst=Broadcast)");

        let trace_id = Uuid::new_v4().to_string();
        let echo_reply =
            build_echo_reply(&sender.uuid().to_string(), Destination::Broadcast, &trace_id);
        if let Err(err) = sender.send(echo_reply).await {
            eprintln!("send error: {err} (reconnecting)");
            let (new_sender, new_receiver) =
                connect_with_retry(&node_config, std::time::Duration::from_secs(1)).await?;
            sender = new_sender;
            receiver = new_receiver;
            continue;
        }
        println!("sent ECHO_REPLY (dst=Broadcast)");

        let trace_id = Uuid::new_v4().to_string();
        let now_ms = now_epoch_ms();
        let time_sync = build_time_sync(
            &sender.uuid().to_string(),
            Destination::Broadcast,
            &trace_id,
            TimeSyncPayload {
                timestamp_utc: "1970-01-01T00:00:00Z".to_string(),
                epoch_ms: now_ms,
                seq,
            },
        );
        if let Err(err) = sender.send(time_sync).await {
            eprintln!("send error: {err} (reconnecting)");
            let (new_sender, new_receiver) =
                connect_with_retry(&node_config, std::time::Duration::from_secs(1)).await?;
            sender = new_sender;
            receiver = new_receiver;
            continue;
        }
        println!("sent TIME_SYNC (dst=Broadcast)");

        seq += 1;
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: std::time::Duration,
) -> Result<(NodeSender, NodeReceiver), jsr_client::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                eprintln!("connect failed: {err}");
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn payload_summary(payload: &serde_json::Value) -> String {
    if let Some(obj) = payload.as_object() {
        let kind = obj.get("type").and_then(|v| v.as_str());
        let content = obj.get("content").and_then(|v| v.as_str());
        match (kind, content) {
            (Some(kind), Some(content)) => format!("{kind}:\"{content}\""),
            (Some(kind), None) => kind.to_string(),
            (None, Some(content)) => format!("\"{content}\""),
            _ => payload.to_string(),
        }
    } else {
        payload.to_string()
    }
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
