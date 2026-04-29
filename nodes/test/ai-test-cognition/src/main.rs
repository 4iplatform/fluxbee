use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use fluxbee_sdk::{connect, try_handle_default_node_status, NodeConfig, NodeReceiver, NodeSender};
use serde_json::{json, Value};
use tokio::time::{timeout, Duration};
use tracing_subscriber::EnvFilter;

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or("JSR_LOG_LEVEL", "info")))
        .init();

    let cfg = build_node_config("AI.test.cognition", "0.1.0", "AI_TEST_COGNITION");
    tracing::info!(
        node = %cfg.name,
        version = %cfg.version,
        "starting AI.test.cognition"
    );

    let (sender, mut receiver) = connect(&cfg).await?;
    run_loop(&sender, &mut receiver).await
}

async fn run_loop(sender: &NodeSender, receiver: &mut NodeReceiver) -> Result<(), DynError> {
    loop {
        let msg = match timeout(Duration::from_secs(300), receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => continue,
        };

        if try_handle_default_node_status(sender, &msg).await? {
            continue;
        }

        let text = extract_text(&msg.payload);
        let probe_id = msg
            .meta
            .context
            .as_ref()
            .and_then(|value| value.get("probe_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let probe_step = msg
            .meta
            .context
            .as_ref()
            .and_then(|value| value.get("probe_step"))
            .and_then(Value::as_u64)
            .unwrap_or(0);

        let memory_package = msg.meta.memory_package.clone();
        let observation = json!({
            "receiver": sender.full_name(),
            "trace_id": msg.routing.trace_id,
            "from": msg.routing.src,
            "probe_id": probe_id,
            "probe_step": probe_step,
            "msg_type": msg.meta.msg_type,
            "ich": msg.meta.ich,
            "thread_id": msg.meta.thread_id,
            "thread_seq": msg.meta.thread_seq,
            "context": msg.meta.context,
            "text": text,
            "memory_package_present": memory_package.is_some(),
            "memory_package": memory_package.as_ref().map(|package| json!({
                "thread_id": package.thread_id,
                "package_version": package.package_version,
                "dominant_context": package.dominant_context.as_ref().map(|value| value.label.clone()),
                "dominant_reason": package.dominant_reason.as_ref().map(|value| value.label.clone()),
                "contexts": package.contexts.len(),
                "reasons": package.reasons.len(),
                "memories": package.memories.len(),
                "episodes": package.episodes.len(),
                "truncated": package.truncated.as_ref().map(|value| json!({
                    "applied": value.applied,
                    "dropped_contexts": value.dropped_contexts,
                    "dropped_reasons": value.dropped_reasons,
                    "dropped_memories": value.dropped_memories,
                    "dropped_episodes": value.dropped_episodes
                }))
            })),
        });

        tracing::info!(
            trace_id = %msg.routing.trace_id,
            probe_id = %probe_id,
            probe_step,
            thread_id = ?msg.meta.thread_id,
            thread_seq = ?msg.meta.thread_seq,
            memory_package = msg.meta.memory_package.is_some(),
            "AI.test.cognition received routed message"
        );

        maybe_append_capture(&observation)?;

        if env_bool("AI_TEST_COGNITION_AUTO_REPLY", true) {
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
                    src_ilk: None,
                    dst_ilk: None,
                    ich: msg.meta.ich.clone(),
                    thread_id: msg.meta.thread_id.clone(),
                    thread_seq: None,
                    ctx: msg.meta.ctx.clone(),
                    ctx_seq: None,
                    ctx_window: None,
                    memory_package: None,
                    scope: None,
                    target: Some("ai.test.cognition.reply".to_string()),
                    action: None,
                    priority: None,
                    context: Some(json!({
                        "probe_id": probe_id,
                        "probe_step": probe_step,
                        "handled_by": sender.full_name(),
                    })),
                    ..Meta::default()
                },
                payload: json!({
                    "status": "ok",
                    "handled_by": sender.full_name(),
                    "observation": observation,
                }),
            };
            sender.send(reply).await?;
        }
    }
}

fn maybe_append_capture(observation: &Value) -> Result<(), DynError> {
    let Some(path) = env_opt("AI_TEST_COGNITION_CAPTURE_PATH") else {
        return Ok(());
    };
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, observation)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn extract_text(payload: &Value) -> String {
    if let Ok(text_payload) = TextV1Payload::from_value(payload) {
        if let Some(content) = text_payload.content {
            return content;
        }
    }
    payload
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
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

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
