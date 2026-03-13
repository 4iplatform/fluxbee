#![forbid(unsafe_code)]

use anyhow::Result;
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};
use io_common::identity::{
    DisabledIdentityResolver, IdentityResolver, MockIdentityResolver, ResolveOrCreateInput,
    ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_context::{ConversationRef, IoContext, MessageRef, PartyRef, ReplyTarget};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;
    let config = Config::from_env()?;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,io_sim=debug,fluxbee_sdk=info")
    });
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        identity_mode = %config.identity_mode,
        dst_node = %config.dst_node.clone().unwrap_or_else(|| "resolve".to_string()),
        "io-sim starting"
    );

    let (sender, receiver) = connect(&NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    })
    .await?;

    tracing::info!(
        full_name = %receiver.full_name(),
        vpn_id = %receiver.vpn_id(),
        "io-sim connected to router"
    );

    let identity: Arc<dyn IdentityResolver> = match config.identity_mode.as_str() {
        "mock" => Arc::new(MockIdentityResolver::new()),
        "shm" => Arc::new(ShmIdentityResolver::new(&config.island_id)),
        "disabled" => Arc::new(DisabledIdentityResolver::new()),
        other => anyhow::bail!("unsupported IDENTITY_MODE={other} (use shm|mock|disabled)"),
    };
    let inbound = Arc::new(Mutex::new(InboundProcessor::new(
        sender.uuid().to_string(),
        InboundConfig {
            ttl: config.ttl,
            dedup_ttl: Duration::from_millis(config.dedup_ttl_ms),
            dedup_max_entries: config.dedup_max_entries,
            dst_node: config.dst_node.clone(),
        },
    )));

    let outbound_task = tokio::spawn(run_outbound_log_loop(receiver));

    if let Some(text) = args.once {
        process_one_inbound(&config, &sender, identity.as_ref(), inbound.clone(), text).await?;
    } else {
        run_stdin_inbound_loop(&config, &sender, identity.as_ref(), inbound.clone()).await?;
    }

    outbound_task.abort();
    Ok(())
}

#[derive(Debug, Clone)]
struct Config {
    identity_mode: String,
    node_name: String,
    island_id: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    ttl: u32,
    dedup_ttl_ms: u64,
    dedup_max_entries: usize,
    dst_node: Option<String>,
    sim_channel: String,
    sim_sender_id: String,
    sim_conversation_id: String,
    sim_thread_id: Option<String>,
    sim_tenant_hint: Option<String>,
}

impl Config {
    fn from_env() -> Result<Self> {
        Ok(Self {
            identity_mode: env("IDENTITY_MODE").unwrap_or_else(|| "mock".to_string()),
            node_name: env("NODE_NAME").unwrap_or_else(|| "IO.sim.local".to_string()),
            island_id: env("ISLAND_ID").unwrap_or_else(|| "local".to_string()),
            node_version: env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET").unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            ttl: env("TTL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(io_common::router_message::DEFAULT_TTL),
            dedup_ttl_ms: env("DEDUP_TTL_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 * 60 * 1000),
            dedup_max_entries: env("DEDUP_MAX_ENTRIES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50_000),
            dst_node: env("SIM_DST_NODE"),
            sim_channel: env("SIM_CHANNEL").unwrap_or_else(|| "sim".to_string()),
            sim_sender_id: env("SIM_SENDER_ID").unwrap_or_else(|| "user.local".to_string()),
            sim_conversation_id: env("SIM_CONVERSATION_ID")
                .unwrap_or_else(|| "sim-console".to_string()),
            sim_thread_id: env("SIM_THREAD_ID"),
            sim_tenant_hint: env("SIM_TENANT_HINT"),
        })
    }
}

struct Args {
    once: Option<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let raw = std::env::args().skip(1).collect::<Vec<_>>();
        let mut once = None;
        let mut i = 0usize;
        while i < raw.len() {
            match raw[i].as_str() {
                "--once" => {
                    let Some(value) = raw.get(i + 1) else {
                        anyhow::bail!("missing value for --once");
                    };
                    once = Some(value.clone());
                    i += 2;
                }
                "--help" | "-h" => {
                    println!("usage: io-sim [--once \"message\"]");
                    std::process::exit(0);
                }
                other => anyhow::bail!("unknown argument: {other}"),
            }
        }
        Ok(Self { once })
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

async fn run_stdin_inbound_loop(
    config: &Config,
    sender: &NodeSender,
    identity: &dyn IdentityResolver,
    inbound: Arc<Mutex<InboundProcessor>>,
) -> Result<()> {
    tracing::info!("io-sim reading lines from stdin");
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    while let Some(line) = lines.next_line().await? {
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }
        process_one_inbound(config, sender, identity, inbound.clone(), text).await?;
    }
    Ok(())
}

async fn process_one_inbound(
    config: &Config,
    sender: &NodeSender,
    identity: &dyn IdentityResolver,
    inbound: Arc<Mutex<InboundProcessor>>,
    text: String,
) -> Result<()> {
    let message_id = format!("sim-{}", Uuid::new_v4());
    let io_ctx = IoContext {
        channel: config.sim_channel.clone(),
        entrypoint: PartyRef {
            kind: "sim_entrypoint".to_string(),
            id: "local".to_string(),
        },
        sender: PartyRef {
            kind: "sim_user".to_string(),
            id: config.sim_sender_id.clone(),
        },
        conversation: ConversationRef {
            kind: "sim_conversation".to_string(),
            id: config.sim_conversation_id.clone(),
            thread_id: config.sim_thread_id.clone(),
        },
        message: MessageRef {
            id: message_id.clone(),
            timestamp: None,
        },
        reply_target: ReplyTarget {
            kind: "sim_log".to_string(),
            address: "stdout".to_string(),
            params: serde_json::json!({}),
        },
    };
    tracing::debug!(
        sim_thread_id = ?config.sim_thread_id,
        sim_conversation_id = %config.sim_conversation_id,
        "io-sim building inbound io context"
    );

    let payload = serde_json::json!({
      "type": "text",
      "content": text,
      "raw": { "sim": true },
    });

    let outcome = inbound
        .lock()
        .await
        .process_inbound(
            identity,
            ResolveOrCreateInput {
                channel: config.sim_channel.clone(),
                external_id: config.sim_sender_id.clone(),
                tenant_hint: config.sim_tenant_hint.clone(),
                attributes: serde_json::json!({
                    "conversation_id": config.sim_conversation_id,
                    "source": "io-sim",
                }),
            },
            io_ctx,
            payload,
        )
        .await;

    match outcome {
        InboundOutcome::SendNow(msg) => {
            let trace_id = msg.routing.trace_id.clone();
            let dst = match &msg.routing.dst {
                fluxbee_sdk::protocol::Destination::Unicast(v) => v.clone(),
                fluxbee_sdk::protocol::Destination::Broadcast => "broadcast".to_string(),
                fluxbee_sdk::protocol::Destination::Resolve => "resolve".to_string(),
            };
            let thread_id = msg
                .meta
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("thread_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            sender.send(msg).await?;
            tracing::info!(%trace_id, dst, thread_id = %thread_id, "io-sim sent inbound message to router");
        }
        InboundOutcome::DroppedDuplicate => {
            tracing::warn!(message_id = %message_id, "io-sim dropped duplicate inbound");
        }
    }

    Ok(())
}

async fn run_outbound_log_loop(mut receiver: NodeReceiver) -> Result<()> {
    loop {
        let msg = receiver.recv().await?;
        let payload_type = msg
            .payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let content = msg
            .payload
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let error_code = msg
            .payload
            .get("code")
            .and_then(|v| v.as_str())
            .or_else(|| {
                msg.payload
                    .get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");
        let error_message = msg
            .payload
            .get("message")
            .and_then(|v| v.as_str())
            .or_else(|| {
                msg.payload
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("");

        tracing::info!(
            trace_id = %msg.routing.trace_id,
            src = %msg.routing.src,
            payload_type = %payload_type,
            content = %content,
            error_code = %error_code,
            error_message = %error_message,
            "io-sim received outbound from router"
        );
    }
}
