use std::path::PathBuf;

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, NodeSender, OperationalRouteProfile, RouteMatch,
    RouteTarget, RouterDispatcher, RpcCommandReceiver,
};
use serde_json::{json, Value};
use tokio::time::{timeout, Duration};
use tracing_subscriber::EnvFilter;

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or("JSR_LOG_LEVEL", "info")))
        .init();

    let cfg = build_node_config("AI.test.gov", "0.1.0", "AI_TEST");
    tracing::info!(node = %cfg.name, version = %cfg.version, "starting AI.test.gov");

    let profile = OperationalRouteProfile::builder()
        .command_channel("incoming")
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command("incoming"))
        .build()?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(cfg, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    let mut incoming = dispatcher.take_command_receiver("incoming").await?;
    run_loop(&sender, &mut incoming).await
}

async fn run_loop(sender: &NodeSender, incoming: &mut RpcCommandReceiver) -> Result<(), DynError> {
    loop {
        let msg = match timeout(Duration::from_secs(300), incoming.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => return Ok(()),
            Err(_) => continue,
        };
        let _ = SYSTEM_KIND; // keep import used regardless of branches

        if try_handle_default_node_status(sender, &msg).await? {
            continue;
        }

        let src_ilk = src_ilk_from_meta(&msg.meta).unwrap_or_default();
        let text = msg
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let probe_id = msg
            .payload
            .get("probe_id")
            .and_then(Value::as_str)
            .unwrap_or_default();

        tracing::info!(
            trace_id = %msg.routing.trace_id,
            routed_from = %msg.routing.src,
            src_ilk = %src_ilk,
            probe_id = %probe_id,
            text = %text,
            "AI.test.gov received routed message"
        );

        if env_bool("AI_TEST_AUTO_REPLY", true) {
            let reply = Message {
                routing: Routing {
                    src: sender.uuid().to_string(),
                    src_l2_name: None,
                    dst: Destination::Unicast(msg.routing.src.clone()),
                    ttl: 16,
                    trace_id: msg.routing.trace_id.clone(),
                },
                meta: Meta {
                    msg_type: "user".to_string(),
                    msg: None,
                    src_ilk: if src_ilk.is_empty() {
                        None
                    } else {
                        Some(src_ilk.clone())
                    },
                    scope: None,
                    target: Some("ai.test.gov.reply".to_string()),
                    action: None,
                    priority: None,
                    context: Some(json!({
                        "handled_by": sender.full_name().as_ref(),
                    })),
                    ..Meta::default()
                },
                payload: json!({
                    "status": "ok",
                    "handled_by": sender.full_name().as_ref(),
                    "probe_id": probe_id,
                    "echo": text,
                }),
            };
            sender.send(reply).await?;
        }
    }
}

fn src_ilk_from_meta(meta: &Meta) -> Option<String> {
    meta.src_ilk.clone()
}

fn build_node_config(default_name: &str, default_version: &str, prefix: &str) -> NodeConfig {
    let node_name_env = format!("{prefix}_NODE_NAME");
    NodeConfig {
        name: fluxbee_sdk::managed_node_name(default_name, &[node_name_env.as_str()]),
        router_socket: PathBuf::from(env_or(
            &format!("{prefix}_ROUTER_SOCKET_DIR"),
            "/var/run/fluxbee/routers",
        )),
        uuid_persistence_dir: PathBuf::from(env_or(
            &format!("{prefix}_UUID_PERSISTENCE_DIR"),
            "/var/lib/fluxbee/state/nodes",
        )),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: PathBuf::from(env_or(&format!("{prefix}_CONFIG_DIR"), "/etc/fluxbee")),
        version: env_or(&format!("{prefix}_NODE_VERSION"), default_version),
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}
