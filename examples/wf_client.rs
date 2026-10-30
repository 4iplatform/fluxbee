use std::path::PathBuf;
use std::time::Duration as StdDuration;

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    NodeConfig, NodeError, NodeSender, NodeUuidMode, OperationalRouteProfile, RouteMatch,
    RouteTarget, RouterDispatcher,
};
use serde_json::{json, Value};
use std::sync::Arc;
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

    let dispatcher = connect_example_node().await?;
    let sender = dispatcher.sender_snapshot();
    println!(
        "connected as {} (uuid={})",
        sender.full_name(),
        sender.uuid(),
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
        other => {
            // WF_LIST_INSTANCES, WF_GET_INSTANCE and WF_CANCEL_INSTANCE answer only SY.admin@motherbee
            // and SY.wf-rules of the WF's hive (0.1.90, FINDINGS A-119), never a client node.
            return Err(format!("unsupported mode {other}; use start").into());
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

async fn connect_example_node() -> Result<Arc<RouterDispatcher>, NodeError> {
    let config_dir = PathBuf::from(json_router::paths::CONFIG_DIR);
    let socket_dir = PathBuf::from(json_router::paths::ROUTER_SOCKET_DIR);
    let state_dir = PathBuf::from(json_router::paths::STATE_DIR);
    let nodes_dir = state_dir.join("nodes");
    // A fixed name: a WF starts an instance only from the nodes its definition's start_from names
    // (0.1.91), and the example workflow wf.invoice names this one.
    let node_name = "WF.client.example@motherbee".to_string();
    let node_config = NodeConfig {
        name: node_name,
        router_socket: socket_dir,
        uuid_persistence_dir: nodes_dir,
        uuid_mode: NodeUuidMode::Persistent,
        config_dir,
        version: VERSION.to_string(),
    };
    let profile = OperationalRouteProfile::builder()
        .command_channel("system")
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command("system"),
        )
        .build()
        .map_err(|err| NodeError::Io(std::io::Error::other(err.to_string())))?;
    RouterDispatcher::connect_with_retry(node_config, StdDuration::from_secs(1), profile).await
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
