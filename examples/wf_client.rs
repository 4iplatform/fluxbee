use std::path::PathBuf;
use std::time::Duration as StdDuration;

use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{
    NodeConfig, NodeError, NodeSender, NodeUuidMode, OperationalRouteProfile, PendingMatcher,
    RouteMatch, RouteTarget, RouterDispatcher, RpcError, RpcRequestLabels,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::time::Duration;
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
        "list" => {
            let response = system_rpc_via_dispatcher(
                &dispatcher,
                target,
                "WF_LIST_INSTANCES",
                "WF_LIST_INSTANCES_RESPONSE",
                json!({ "limit": 20 }),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&response.payload)?);
        }
        "get" => {
            let instance_id = args.get(3).ok_or("missing instance_id for get")?;
            let response = system_rpc_via_dispatcher(
                &dispatcher,
                target,
                "WF_GET_INSTANCE",
                "WF_GET_INSTANCE_RESPONSE",
                json!({ "instance_id": instance_id, "log_limit": 20 }),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&response.payload)?);
        }
        "cancel" => {
            let instance_id = args.get(3).ok_or("missing instance_id for cancel")?;
            let response = system_rpc_via_dispatcher(
                &dispatcher,
                target,
                "WF_CANCEL_INSTANCE",
                "WF_CANCEL_INSTANCE_RESPONSE",
                json!({ "instance_id": instance_id, "reason": "manual example cancel" }),
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&response.payload)?);
        }
        other => {
            return Err(format!("unsupported mode {other}; use start|list|get|cancel").into());
        }
    }

    Ok(())
}

async fn system_rpc_via_dispatcher(
    dispatcher: &Arc<RouterDispatcher>,
    target: &str,
    request_msg: &str,
    response_msg: &str,
    payload: Value,
) -> Result<Message, Box<dyn std::error::Error>> {
    let message = Message {
        routing: Routing {
            src: String::new(),
            src_l2_name: None,
            dst: Destination::Broadcast,
            ttl: 16,
            trace_id: String::new(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(request_msg.to_string()),
            target: Some(target.to_string()),
            ..Meta::default()
        },
        payload,
    };
    let matcher = PendingMatcher::new(
        vec![RouteMatch::exact(SYSTEM_KIND, response_msg)],
        vec![
            RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
            RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
        ],
        vec![],
    );
    let labels = RpcRequestLabels::new(target, request_msg, response_msg);
    match dispatcher
        .send_with_matcher(message, matcher, labels, Duration::from_secs(30))
        .await
    {
        Ok(msg) => Ok(msg),
        Err(RpcError::Unreachable {
            reason,
            original_dst,
        }) => Err(format!(
            "router returned UNREACHABLE: reason={reason} original_dst={original_dst}"
        )
        .into()),
        Err(RpcError::TtlExceeded {
            original_dst,
            last_hop,
        }) => Err(format!(
            "router returned TTL_EXCEEDED: original_dst={original_dst} last_hop={last_hop}"
        )
        .into()),
        Err(err) => Err(err.into()),
    }
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
    let node_name = format!("WF.client.example.{}@motherbee", std::process::id());
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
        .map_err(|err| {
            NodeError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                err.to_string(),
            ))
        })?;
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

// `await_response` was removed when migrating to RouterDispatcher;
// `system_rpc_via_dispatcher` above replaces it with `send_with_matcher`.
