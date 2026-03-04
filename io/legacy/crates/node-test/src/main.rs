#![forbid(unsafe_code)]

use anyhow::Result;
use router_client::NodeConfig;
use router_protocol::Message;
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if mode != "slack_send" {
        eprintln!(
            "usage:\n  node-test slack_send <target_node> <island_id> <team_id> <channel_id> <text> [thread_ts]\n\nexample:\n  ROUTER_ADDR=\"unix:/var/run/json-router/routers/<router_uuid>.sock\" cargo run -p node-test -- slack_send IO.slack.T123 local T123 C0ACXNG14RG \"hola\""
        );
        std::process::exit(2);
    }

    let target_node = args.get(2).map(|s| s.as_str()).unwrap_or("");
    let island_id = args.get(3).map(|s| s.as_str()).unwrap_or("");
    let team_id = args.get(4).map(|s| s.as_str()).unwrap_or("");
    let channel_id = args.get(5).map(|s| s.as_str()).unwrap_or("");
    let text = args.get(6).map(|s| s.as_str()).unwrap_or("");
    let thread_ts = args.get(7).map(|s| s.as_str());

    if target_node.is_empty() || island_id.is_empty() || team_id.is_empty() || channel_id.is_empty() || text.is_empty()
    {
        anyhow::bail!("missing args; run without args to see usage");
    }

    let router_addr = env_req("ROUTER_ADDR")?;
    let node_name = env("NODE_NAME").unwrap_or_else(|| "WF.node_test".to_string());
    let node_uuid = env("NODE_UUID").unwrap_or_else(|| Uuid::new_v4().to_string());
    let node_version = env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string());
    let queue_capacity = env("QUEUE_CAPACITY")
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);

    let (sender, _receiver) = router_client::connect(NodeConfig {
        name: node_name,
        island_id: island_id.to_string(),
        node_uuid,
        version: node_version,
        router_addr,
        queue_capacity,
    })
    .await?;

    let target_full = if target_node.contains('@') {
        target_node.to_string()
    } else {
        format!("{target_node}@{island_id}")
    };

    let params = match thread_ts {
        Some(ts) => json!({ "workspace_id": team_id, "thread_ts": ts }),
        None => json!({ "workspace_id": team_id }),
    };

    let msg = Message {
        routing: router_protocol::Routing {
            src: sender.uuid().to_string(),
            dst: Some("broadcast".to_string()),
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: router_protocol::Meta {
            ty: "user".to_string(),
            msg: None,
            target: Some(target_full.clone()),
            action: None,
            src_ilk: None,
            dst_ilk: None,
            context: json!({
                "io": {
                    "reply_target": {
                        "kind": "slack_post",
                        "address": channel_id,
                        "params": params
                    }
                }
            }),
        },
        payload: json!({ "type": "text", "content": text }),
    };

    sender.send(msg).await?;
    println!("sent slack_send to {target_full} channel={channel_id}");
    Ok(())
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_req(key: &str) -> Result<String> {
    env(key).ok_or_else(|| anyhow::anyhow!("missing env var {key}"))
}
