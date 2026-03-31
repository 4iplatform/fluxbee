#![forbid(unsafe_code)]

use anyhow::Result;
use fluxbee_sdk::{connect, NodeConfig, NodeSender, NodeUuidMode};
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_context::{ConversationRef, IoContext, MessageRef, PartyRef, ReplyTarget};
use io_common::io_control_plane_logging::log_config_response_message;
use io_common::provision::{FluxbeeIdentityProvisioner, IdentityProvisionConfig, RouterInbox};
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
        dst_node = %config.dst_node.clone().unwrap_or_else(|| "resolve".to_string()),
        "io-sim starting"
    );

    let (sender, receiver) = connect(&NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    })
    .await?;

    tracing::info!(
        full_name = %receiver.full_name(),
        vpn_id = %receiver.vpn_id(),
        "io-sim connected to router"
    );

    let identity: Arc<dyn IdentityResolver> = Arc::new(ShmIdentityResolver::new(&config.island_id));
    let inbox = Arc::new(Mutex::new(RouterInbox::new(receiver)));
    let provisioner: Arc<dyn IdentityProvisioner> = Arc::new(FluxbeeIdentityProvisioner::new(
        sender.clone(),
        inbox.clone(),
        IdentityProvisionConfig {
            target: config.identity_target.clone(),
            timeout: Duration::from_millis(config.identity_timeout_ms),
        },
    ));
    let inbound = Arc::new(Mutex::new(InboundProcessor::new(
        sender.uuid().to_string(),
        InboundConfig {
            ttl: config.ttl,
            dedup_ttl: Duration::from_millis(config.dedup_ttl_ms),
            dedup_max_entries: config.dedup_max_entries,
            dst_node: config.dst_node.clone(),
            provision_on_miss: true,
        },
    )));

    if let Some(text) = args.once {
        process_one_inbound(
            &config,
            &sender,
            &inbox,
            identity.as_ref(),
            provisioner.as_ref(),
            inbound.clone(),
            text,
        )
        .await?;
    } else {
        run_stdin_inbound_loop(
            &config,
            &sender,
            &inbox,
            identity.as_ref(),
            provisioner.as_ref(),
            inbound.clone(),
        )
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Config {
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
    sim_reply_kind: String,
    sim_reply_address: String,
    sim_reply_thread_ts: Option<String>,
    sim_reply_workspace_id: Option<String>,
    sim_attachments_json: Option<String>,
    sim_content_ref_json: Option<String>,
    identity_target: String,
    identity_timeout_ms: u64,
}

impl Config {
    fn from_env() -> Result<Self> {
        let island_id = env("ISLAND_ID").unwrap_or_else(|| "local".to_string());
        Ok(Self {
            node_name: env("NODE_NAME").unwrap_or_else(|| "IO.sim.local".to_string()),
            island_id: island_id.clone(),
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
            sim_reply_kind: env("SIM_REPLY_KIND").unwrap_or_else(|| "sim_log".to_string()),
            sim_reply_address: env("SIM_REPLY_ADDRESS").unwrap_or_else(|| "stdout".to_string()),
            sim_reply_thread_ts: env("SIM_REPLY_THREAD_TS"),
            sim_reply_workspace_id: env("SIM_REPLY_WORKSPACE_ID"),
            sim_attachments_json: env("SIM_ATTACHMENTS_JSON"),
            sim_content_ref_json: env("SIM_CONTENT_REF_JSON"),
            identity_target: env("IDENTITY_TARGET")
                .unwrap_or_else(|| format!("SY.identity@{island_id}")),
            identity_timeout_ms: env("IDENTITY_TIMEOUT_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
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
    inbox: &Arc<Mutex<RouterInbox>>,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
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
        process_one_inbound(
            config,
            sender,
            inbox,
            identity,
            provisioner,
            inbound.clone(),
            text,
        )
        .await?;
    }
    Ok(())
}

async fn process_one_inbound(
    config: &Config,
    sender: &NodeSender,
    inbox: &Arc<Mutex<RouterInbox>>,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
    inbound: Arc<Mutex<InboundProcessor>>,
    text: String,
) -> Result<()> {
    let message_id = format!("sim-{}", Uuid::new_v4());
    let reply_params = if config.sim_reply_kind == "slack_post" {
        let mut obj = serde_json::Map::new();
        if let Some(thread_ts) = &config.sim_reply_thread_ts {
            obj.insert(
                "thread_ts".to_string(),
                serde_json::Value::String(thread_ts.clone()),
            );
        }
        if let Some(workspace_id) = &config.sim_reply_workspace_id {
            obj.insert(
                "workspace_id".to_string(),
                serde_json::Value::String(workspace_id.clone()),
            );
        }
        serde_json::Value::Object(obj)
    } else {
        serde_json::json!({})
    };
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
            kind: config.sim_reply_kind.clone(),
            address: config.sim_reply_address.clone(),
            params: reply_params,
        },
    };
    tracing::debug!(
        sim_thread_id = ?config.sim_thread_id,
        sim_conversation_id = %config.sim_conversation_id,
        "io-sim building inbound io context"
    );

    let mut payload = serde_json::json!({
      "type": "text",
      "content": text,
      "attachments": [],
      "raw": { "sim": true },
    });
    if let Some(raw_attachments) = &config.sim_attachments_json {
        match serde_json::from_str::<serde_json::Value>(raw_attachments) {
            Ok(attachments) if attachments.is_array() => {
                payload["attachments"] = attachments;
            }
            Ok(_) => tracing::warn!("SIM_ATTACHMENTS_JSON ignored: expected JSON array"),
            Err(err) => tracing::warn!(error = %err, "SIM_ATTACHMENTS_JSON ignored: invalid JSON"),
        }
    }
    if let Some(raw_content_ref) = &config.sim_content_ref_json {
        match serde_json::from_str::<serde_json::Value>(raw_content_ref) {
            Ok(content_ref) if content_ref.is_object() => {
                payload["content_ref"] = content_ref;
                payload
                    .as_object_mut()
                    .expect("payload object")
                    .remove("content");
            }
            Ok(_) => tracing::warn!("SIM_CONTENT_REF_JSON ignored: expected JSON object"),
            Err(err) => tracing::warn!(error = %err, "SIM_CONTENT_REF_JSON ignored: invalid JSON"),
        }
    }

    let outcome = inbound
        .lock()
        .await
        .process_inbound(
            identity,
            Some(provisioner),
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
            let src_ilk = msg.meta.src_ilk.clone().unwrap_or_default();
            let has_src_ilk = !src_ilk.is_empty();
            let legacy_context_src_ilk = msg
                .meta
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("src_ilk"))
                .is_some();
            let thread_id = msg
                .meta
                .thread_id
                .clone()
                .or_else(|| {
                    msg.meta
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("thread_id"))
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string)
                })
                .unwrap_or_default();
            if legacy_context_src_ilk {
                tracing::warn!(
                    %trace_id,
                    "io-sim outbound still includes legacy meta.context.src_ilk"
                );
            }
            if let Ok(wire) = serde_json::to_string(&msg) {
                tracing::debug!(%trace_id, wire = %wire, "io-sim outbound wire message");
            }
            sender.send(msg).await?;
            tracing::info!(
                %trace_id,
                dst,
                thread_id = %thread_id,
                has_src_ilk,
                src_ilk = %src_ilk,
                "io-sim sent inbound message to router"
            );
            match inbox
                .lock()
                .await
                .recv_for_trace_id(&trace_id, Duration::from_secs(30))
                .await
            {
                Ok(outbound) => log_outbound_message(&outbound),
                Err(io_common::identity::IdentityError::Timeout) => {
                    tracing::warn!(%trace_id, "io-sim timed out waiting outbound from router");
                }
                Err(err) => {
                    tracing::warn!(%trace_id, ?err, "io-sim failed waiting outbound from router");
                }
            }
        }
        InboundOutcome::DroppedDuplicate => {
            tracing::warn!(message_id = %message_id, "io-sim dropped duplicate inbound");
        }
    }

    Ok(())
}

fn log_outbound_message(msg: &fluxbee_sdk::protocol::Message) {
    if log_config_response_message(msg, "io-sim") {
        return;
    }

    let msg_kind = msg.meta.msg.as_deref().unwrap_or("");
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
    let unreachable_reason = msg
        .payload
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let original_dst = msg
        .payload
        .get("original_dst")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Ok(wire) = serde_json::to_string(msg) {
        tracing::debug!(trace_id = %msg.routing.trace_id, wire = %wire, "io-sim inbound wire message");
    }

    tracing::info!(
        trace_id = %msg.routing.trace_id,
        src = %msg.routing.src,
        msg_kind = %msg_kind,
        payload_type = %payload_type,
        content = %content,
        error_code = %error_code,
        error_message = %error_message,
        unreachable_reason = %unreachable_reason,
        original_dst = %original_dst,
        "io-sim received outbound from router"
    );
}
