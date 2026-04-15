use std::path::PathBuf;
use std::time::Duration as StdDuration;

use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{connect, NodeConfig, NodeError, NodeReceiver, NodeSender, NodeUuidMode};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const VERSION: &str = "1.0";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("start");
    let target = args
        .get(2)
        .map(|s| s.as_str())
        .unwrap_or("WF.invoice@motherbee");

    let (sender, mut receiver) = connect_example_node().await?;
    println!(
        "connected as {} (uuid={}, vpn={})",
        sender.full_name(),
        sender.uuid(),
        receiver.vpn_id(),
    );

    match mode {
        "start" => {
            let customer_id = args.get(3).map(|s| s.as_str()).unwrap_or("cust-001");
            let amount_cents = args
                .get(4)
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(25_000);
            let trace_id = Uuid::new_v4().to_string();
            sender
                .send(build_targeted_message(
                    &sender,
                    target,
                    "user",
                    None,
                    json!({
                        "customer_id": customer_id,
                        "amount_cents": amount_cents,
                        "currency": "USD"
                    }),
                    &trace_id,
                ))
                .await?;
            println!(
                "sent workflow start target={} customer_id={} amount_cents={} trace_id={}",
                target, customer_id, amount_cents, trace_id
            );
        }
        "list" => {
            let trace_id = Uuid::new_v4().to_string();
            sender
                .send(build_targeted_message(
                    &sender,
                    target,
                    SYSTEM_KIND,
                    Some("WF_LIST_INSTANCES"),
                    json!({ "limit": 20 }),
                    &trace_id,
                ))
                .await?;
            let response =
                await_response(&mut receiver, &trace_id, "WF_LIST_INSTANCES_RESPONSE").await?;
            println!("{}", serde_json::to_string_pretty(&response.payload)?);
        }
        "get" => {
            let instance_id = args.get(3).ok_or("missing instance_id for get")?;
            let trace_id = Uuid::new_v4().to_string();
            sender
                .send(build_targeted_message(
                    &sender,
                    target,
                    SYSTEM_KIND,
                    Some("WF_GET_INSTANCE"),
                    json!({ "instance_id": instance_id, "log_limit": 20 }),
                    &trace_id,
                ))
                .await?;
            let response =
                await_response(&mut receiver, &trace_id, "WF_GET_INSTANCE_RESPONSE").await?;
            println!("{}", serde_json::to_string_pretty(&response.payload)?);
        }
        "cancel" => {
            let instance_id = args.get(3).ok_or("missing instance_id for cancel")?;
            let trace_id = Uuid::new_v4().to_string();
            sender
                .send(build_targeted_message(
                    &sender,
                    target,
                    SYSTEM_KIND,
                    Some("WF_CANCEL_INSTANCE"),
                    json!({ "instance_id": instance_id, "reason": "manual example cancel" }),
                    &trace_id,
                ))
                .await?;
            let response =
                await_response(&mut receiver, &trace_id, "WF_CANCEL_INSTANCE_RESPONSE").await?;
            println!("{}", serde_json::to_string_pretty(&response.payload)?);
        }
        other => {
            return Err(format!("unsupported mode {other}; use start|list|get|cancel").into());
        }
    }

    Ok(())
}

fn init_logging() {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .try_init();
}

async fn connect_example_node() -> Result<(NodeSender, NodeReceiver), NodeError> {
    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let nodes_dir = state_dir.join("nodes");
    let node_name = format!("WF.client.example.{}@motherbee", std::process::id());
    let node_config = NodeConfig {
        name: node_name,
        router_socket: socket_dir,
        uuid_persistence_dir: nodes_dir,
        uuid_mode: NodeUuidMode::Persistent,
        config_dir,
        version: VERSION.to_string(),
    };
    connect_with_retry(&node_config, StdDuration::from_secs(1)).await
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: StdDuration,
) -> Result<(NodeSender, NodeReceiver), NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                eprintln!("connect failed: {err}");
                sleep(Duration::from_millis(delay.as_millis() as u64)).await;
            }
        }
    }
}

fn build_targeted_message(
    sender: &NodeSender,
    target: &str,
    msg_type: &str,
    msg_name: Option<&str>,
    payload: Value,
    trace_id: &str,
) -> Message {
    Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: trace_id.to_string(),
        },
        meta: Meta {
            msg_type: msg_type.to_string(),
            msg: msg_name.map(|value| value.to_string()),
            target: Some(target.to_string()),
            ..Meta::default()
        },
        payload,
    }
}

async fn await_response(
    receiver: &mut NodeReceiver,
    trace_id: &str,
    response_msg: &str,
) -> Result<Message, Box<dyn std::error::Error>> {
    loop {
        let msg = receiver.recv_timeout(Duration::from_secs(30)).await?;
        if msg.routing.trace_id != trace_id {
            continue;
        }
        if msg.meta.msg_type == SYSTEM_KIND {
            match msg.meta.msg.as_deref() {
                Some(kind) if kind == response_msg => return Ok(msg),
                Some(MSG_UNREACHABLE) => {
                    return Err(format!(
                        "router returned UNREACHABLE: {}",
                        serde_json::to_string_pretty(&msg.payload)?
                    )
                    .into())
                }
                Some(MSG_TTL_EXCEEDED) => {
                    return Err(format!(
                        "router returned TTL_EXCEEDED: {}",
                        serde_json::to_string_pretty(&msg.payload)?
                    )
                    .into())
                }
                Some(other) => {
                    return Err(format!(
                        "received unexpected system response {other}: {}",
                        serde_json::to_string_pretty(&msg.payload)?
                    )
                    .into())
                }
                None => {}
            }
        }
    }
}
