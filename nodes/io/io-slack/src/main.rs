#![forbid(unsafe_code)]

use anyhow::Result;
use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    connect, managed_node_config_path, NodeConfig, NodeSender, NodeUuidMode, FLUXBEE_NODE_NAME_ENV,
};
use futures_util::{SinkExt, StreamExt};
use io_common::frontdesk_contract::parse_frontdesk_result_payload;
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_context::{extract_slack_post_target, slack_inbound_io_context};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload,
    ensure_config_version_advances, parse_and_validate_io_control_plane_request, IoConfigSource,
    IoControlPlaneErrorInfo, IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_invalid_runtime_credentials,
    log_config_set_persist_error, log_config_set_stale_rejected,
    log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::{default_state_dir, persist_io_control_plane_state};
use io_common::io_slack_adapter_config::IoSlackAdapterConfigContract;
use io_common::provision::{FluxbeeIdentityProvisioner, IdentityProvisionConfig, RouterInbox};
use io_common::relay::{
    AssembledTurn, InMemoryRelayStore, RelayBuffer, RelayDecision, RelayFlushHints, RelayFragment,
    RelayPolicy,
};
use io_common::text_v1_blob::{
    resolve_text_v1_for_outbound, InboundAttachmentInput, IoBlobContractError, IoBlobRuntimeConfig,
    IoTextBlobConfig,
};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if config.dev_mode {
            tracing_subscriber::EnvFilter::new("info,io_slack=debug,fluxbee_sdk=info")
        } else {
            tracing_subscriber::EnvFilter::new("info")
        }
    });

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        state_dir = %config.state_dir.display(),
        dst_node = %config.dst_node.clone().unwrap_or_else(|| "resolve".to_string()),
        dev_mode = %config.dev_mode,
        "io-slack starting"
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
        "connected to router"
    );

    let inbox = Arc::new(Mutex::new(RouterInbox::new(receiver)));
    let slack = Arc::new(SlackClients::new(&config)?);
    let identity: Arc<dyn IdentityResolver> = Arc::new(ShmIdentityResolver::new(&config.island_id));
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
            blob_runtime: Some(config.blob_runtime.clone()),
        },
    )));
    let blob_toolkit = Arc::new(
        config
            .blob_runtime
            .build_toolkit()
            .map_err(|err| anyhow::anyhow!("invalid blob runtime config: {err}"))?,
    );
    let blob_payload_cfg = config.blob_runtime.text_v1.clone();
    let mut boot_state = bootstrap_io_control_plane_state(&config.state_dir, &config.node_name)
        .unwrap_or_else(|err| {
            tracing::warn!(
                error = %err,
                state_dir = %config.state_dir.display(),
                node_name = %config.node_name,
                "failed to bootstrap IO control-plane state; using UNCONFIGURED"
            );
            IoControlPlaneState::default()
        });
    if let Some(effective) = boot_state.effective_config.as_ref() {
        match extract_runtime_slack_credentials(effective) {
            Ok((app_token, bot_token)) => {
                slack.reload_credentials(app_token, bot_token).await;
            }
            Err(err) => {
                boot_state.current_state = IoNodeLifecycleState::FailedConfig;
                boot_state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err.to_string(),
                });
                tracing::warn!(
                    node_name = %config.node_name,
                    error = %err,
                    "boot effective config is missing/invalid slack credentials; starting in FAILED_CONFIG"
                );
            }
        }
    }
    let relay_policy = slack_relay_policy(&config, boot_state.effective_config.as_ref())
        .map_err(|err| anyhow::anyhow!("invalid relay policy from node config: {err}"))?;
    tracing::info!(
        relay_enabled = relay_policy.enabled,
        relay_window_ms = relay_policy.relay_window_ms,
        relay_max_open_sessions = relay_policy.max_open_sessions,
        relay_max_fragments = relay_policy.max_fragments_per_session,
        relay_max_bytes = relay_policy.max_bytes_per_session,
        relay_source = if boot_state.effective_config.is_some() {
            "effective_config"
        } else {
            "spawn_config"
        },
        "io-slack relay policy initialized"
    );
    let relay = Arc::new(Mutex::new(
        RelayBuffer::new(relay_policy.clone(), InMemoryRelayStore::new())
            .map_err(|err| anyhow::anyhow!("invalid relay policy: {err}"))?,
    ));
    let control_plane = Arc::new(RwLock::new(boot_state.clone()));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    ));
    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoSlackAdapterConfigContract);

    let outbound_task = tokio::spawn(run_outbound_loop(
        inbox.clone(),
        sender.clone(),
        config.node_name.clone(),
        config.state_dir.clone(),
        control_plane.clone(),
        control_metrics.clone(),
        adapter_contract.clone(),
        inbound.clone(),
        slack.clone(),
        relay.clone(),
        blob_toolkit.clone(),
        blob_payload_cfg.clone(),
    ));

    let inbound_task = tokio::spawn(run_inbound_socket_mode(
        config.clone(),
        sender.clone(),
        slack.clone(),
        identity.clone(),
        provisioner.clone(),
        inbound.clone(),
        relay.clone(),
        blob_toolkit.clone(),
        blob_payload_cfg.clone(),
    ));

    let relay_flush_task = tokio::spawn(run_relay_flush_loop(
        relay,
        sender,
        identity,
        provisioner,
        inbound,
    ));

    let _ = tokio::join!(inbound_task, outbound_task, relay_flush_task);
    Ok(())
}

#[derive(Clone)]
struct Config {
    slack_app_token: Option<String>,
    slack_bot_token: Option<String>,
    node_name: String,
    island_id: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    state_dir: PathBuf,
    dev_mode: bool,
    ttl: u32,
    dedup_ttl_ms: u64,
    dedup_max_entries: usize,
    dst_node: Option<String>,
    identity_target: String,
    identity_timeout_ms: u64,
    relay: SlackRelayConfig,
    blob_runtime: IoBlobRuntimeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlackRelayConfig {
    window_ms: u64,
    max_open_sessions: usize,
    max_fragments_per_session: usize,
    max_bytes_per_session: usize,
}

impl Default for SlackRelayConfig {
    fn default() -> Self {
        Self {
            window_ms: 0,
            max_open_sessions: 10_000,
            max_fragments_per_session: 8,
            max_bytes_per_session: 256 * 1024,
        }
    }
}

impl Config {
    fn from_env() -> Result<Self> {
        let resolved_node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            anyhow::anyhow!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let resolved_island_id = hive_from_node_name(&resolved_node_name).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid {FLUXBEE_NODE_NAME_ENV}='{resolved_node_name}': expected <name>@<hive>"
            )
        })?;
        let env_config_dir =
            PathBuf::from(env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string()));
        let spawn_cfg = load_spawn_config(&resolved_node_name)?;
        tracing::info!(path = %spawn_cfg.path.display(), "io-slack loaded spawn config");
        let spawn_doc = Some(&spawn_cfg.doc);

        let slack_app_token = env("SLACK_APP_TOKEN").or_else(|| {
            resolve_secret(
                spawn_doc,
                &["slack.app_token", "slack_app_token"],
                &["slack.app_token_ref", "slack_app_token_ref"],
            )
        });
        let slack_bot_token = env("SLACK_BOT_TOKEN").or_else(|| {
            resolve_secret(
                spawn_doc,
                &["slack.bot_token", "slack_bot_token"],
                &["slack.bot_token_ref", "slack_bot_token_ref"],
            )
        });

        let blob_root = PathBuf::from(
            env("BLOB_ROOT")
                .or_else(|| json_get_string_opt(spawn_doc, &["blob.path", "io.blob.path"]))
                .unwrap_or_else(|| "/var/lib/fluxbee/blob".to_string()),
        );
        let max_blob_bytes = env("BLOB_MAX_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_doc,
                    &["blob.max_blob_bytes", "io.blob.max_blob_bytes"],
                )
            });
        let allowed_mimes_override = env("IO_SLACK_ALLOWED_MIMES")
            .or_else(|| json_get_string_opt(spawn_doc, &["io.blob.allowed_mimes_csv"]));
        let mut text_v1_cfg = IoTextBlobConfig::default();
        text_v1_cfg.allowed_mimes = default_slack_allowed_mimes();
        text_v1_cfg.max_message_bytes = env("IO_SLACK_MAX_MESSAGE_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(spawn_doc, &["io.blob.max_message_bytes"]).map(|v| v as usize)
            })
            .unwrap_or(text_v1_cfg.max_message_bytes);
        text_v1_cfg.message_overhead_bytes = env("IO_SLACK_MESSAGE_OVERHEAD_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(spawn_doc, &["io.blob.message_overhead_bytes"]).map(|v| v as usize)
            })
            .unwrap_or(text_v1_cfg.message_overhead_bytes);
        text_v1_cfg.max_attachments = env("IO_SLACK_MAX_ATTACHMENTS")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(spawn_doc, &["io.blob.max_attachments"]).map(|v| v as usize)
            })
            .unwrap_or(text_v1_cfg.max_attachments);
        text_v1_cfg.max_attachment_bytes = env("IO_SLACK_MAX_ATTACHMENT_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| json_get_u64_opt(spawn_doc, &["io.blob.max_attachment_bytes"]))
            .unwrap_or(text_v1_cfg.max_attachment_bytes);
        text_v1_cfg.max_total_attachment_bytes = env("IO_SLACK_MAX_TOTAL_ATTACHMENT_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| json_get_u64_opt(spawn_doc, &["io.blob.max_total_attachment_bytes"]))
            .unwrap_or(text_v1_cfg.max_total_attachment_bytes);
        if let Some(csv) = allowed_mimes_override {
            let parsed = csv
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.to_ascii_lowercase())
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                text_v1_cfg.allowed_mimes = parsed;
            }
        }
        let resolve_retry = fluxbee_sdk::blob::ResolveRetryConfig {
            max_wait_ms: env("IO_SLACK_BLOB_RETRY_MAX_WAIT_MS")
                .and_then(|v| v.parse().ok())
                .or_else(|| json_get_u64_opt(spawn_doc, &["io.blob.resolve_retry.max_wait_ms"]))
                .unwrap_or(fluxbee_sdk::blob::BLOB_RETRY_MAX_MS),
            initial_delay_ms: env("IO_SLACK_BLOB_RETRY_INITIAL_MS")
                .and_then(|v| v.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(spawn_doc, &["io.blob.resolve_retry.initial_delay_ms"])
                })
                .unwrap_or(fluxbee_sdk::blob::BLOB_RETRY_INITIAL_MS),
            backoff_factor: env("IO_SLACK_BLOB_RETRY_BACKOFF")
                .and_then(|v| v.parse().ok())
                .unwrap_or(fluxbee_sdk::blob::BLOB_RETRY_BACKOFF),
        };
        let blob_runtime = IoBlobRuntimeConfig {
            blob_root,
            max_blob_bytes,
            text_v1: text_v1_cfg,
            resolve_retry,
        };
        let relay = SlackRelayConfig {
            window_ms: json_get_u64_opt(
                spawn_doc,
                &[
                    "config.io.relay.window_ms",
                    "io.relay.window_ms",
                    "relay.window_ms",
                ],
            )
            .or_else(|| env("SLACK_SESSION_WINDOW_MS").and_then(|v| v.parse().ok()))
            .unwrap_or_default(),
            max_open_sessions: json_get_usize_opt(
                spawn_doc,
                &[
                    "config.io.relay.max_open_sessions",
                    "io.relay.max_open_sessions",
                    "relay.max_open_sessions",
                ],
            )
            .or_else(|| env("SLACK_SESSION_MAX_SESSIONS").and_then(|v| v.parse().ok()))
            .unwrap_or(10_000),
            max_fragments_per_session: json_get_usize_opt(
                spawn_doc,
                &[
                    "config.io.relay.max_fragments_per_session",
                    "io.relay.max_fragments_per_session",
                    "relay.max_fragments_per_session",
                ],
            )
            .or_else(|| env("SLACK_SESSION_MAX_FRAGMENTS").and_then(|v| v.parse().ok()))
            .unwrap_or(8),
            max_bytes_per_session: json_get_usize_opt(
                spawn_doc,
                &[
                    "config.io.relay.max_bytes_per_session",
                    "io.relay.max_bytes_per_session",
                    "relay.max_bytes_per_session",
                ],
            )
            .unwrap_or(256 * 1024),
        };

        Ok(Self {
            slack_app_token,
            slack_bot_token,
            node_name: resolved_node_name,
            island_id: resolved_island_id.clone(),
            node_version: env("NODE_VERSION")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_doc,
                        &["_system.runtime_version", "runtime.version", "node.version"],
                    )
                })
                .unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| {
                        json_get_string_opt(spawn_doc, &["node.router_socket", "router_socket"])
                    })
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_doc,
                            &["node.uuid_persistence_dir", "uuid_persistence_dir"],
                        )
                    })
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.config_dir", "config_dir"]))
                    .unwrap_or_else(|| env_config_dir.display().to_string()),
            ),
            state_dir: env("STATE_DIR")
                .or_else(|| json_get_string_opt(spawn_doc, &["node.state_dir", "state_dir"]))
                .map(PathBuf::from)
                .unwrap_or_else(default_state_dir),
            dev_mode: env_bool("DEV_MODE").unwrap_or(false),
            ttl: env("TTL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(io_common::router_message::DEFAULT_TTL),
            dedup_ttl_ms: env("DEDUP_TTL_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10 * 60 * 1000),
            dedup_max_entries: env("DEDUP_MAX_ENTRIES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50_000),
            dst_node: env("IO_SLACK_DST_NODE")
                .or_else(|| json_get_string_opt(spawn_doc, &["io.dst_node", "dst_node"])),
            identity_target: env("IDENTITY_TARGET")
                .or_else(|| json_get_string_opt(spawn_doc, &["identity.target", "identity_target"]))
                .unwrap_or_else(|| format!("SY.identity@{}", resolved_island_id)),
            identity_timeout_ms: env("IDENTITY_TIMEOUT_MS")
                .and_then(|v| v.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(spawn_doc, &["identity.timeout_ms", "identity_timeout_ms"])
                })
                .unwrap_or(10_000),
            relay,
            blob_runtime,
        })
    }
}

fn default_slack_allowed_mimes() -> Vec<String> {
    vec![
        "image/jpeg".to_string(),
        "image/png".to_string(),
        "image/webp".to_string(),
        "image/gif".to_string(),
        "application/pdf".to_string(),
        "text/plain".to_string(),
        "text/markdown".to_string(),
        "application/json".to_string(),
        "text/csv".to_string(),
        "application/msword".to_string(),
        "application/vnd.ms-excel".to_string(),
        "application/vnd.ms-powerpoint".to_string(),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string(),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string(),
    ]
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_bool(key: &str) -> Option<bool> {
    let v = env(key)?.to_ascii_lowercase();
    match v.as_str() {
        "1" | "true" | "yes" | "y" | "on" => Some(true),
        "0" | "false" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

struct SpawnConfig {
    path: PathBuf,
    doc: Value,
}

fn load_spawn_config(node_name: &str) -> Result<SpawnConfig> {
    let path = managed_node_config_path(node_name)
        .map_err(|err| anyhow::anyhow!("failed to resolve managed config path: {err}"))?;
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        anyhow::anyhow!(
            "failed to read managed config file {}: {err}",
            path.display()
        )
    })?;
    let doc = serde_json::from_str::<Value>(&raw).map_err(|err| {
        anyhow::anyhow!(
            "failed to parse managed config JSON {}: {err}",
            path.display()
        )
    })?;
    Ok(SpawnConfig { path, doc })
}

fn hive_from_node_name(node_name: &str) -> Option<String> {
    node_name
        .split_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|hive| !hive.is_empty())
        .map(ToString::to_string)
}

fn json_get_string_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<String> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(v) = json_get_path(doc, path) {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

fn json_get_u64_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<u64> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(v) = json_get_path(doc, path) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

fn json_get_usize_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<usize> {
    json_get_u64_opt(doc, dotted_paths).and_then(|value| usize::try_from(value).ok())
}

fn resolve_secret(doc: Option<&Value>, value_paths: &[&str], ref_paths: &[&str]) -> Option<String> {
    if let Some(value) = json_get_string_opt(doc, value_paths) {
        return Some(value);
    }
    let reference = json_get_string_opt(doc, ref_paths)?;
    if let Some(var) = reference.strip_prefix("env:") {
        return env(var);
    }
    None
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for part in dotted_path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

#[derive(Clone)]
struct SlackClients {
    http: reqwest::Client,
    runtime: Arc<RwLock<SlackRuntimeState>>,
}

#[derive(Debug, Clone)]
struct SlackRuntimeState {
    slack_app_token: Option<String>,
    slack_bot_token: Option<String>,
    config_generation: u64,
}

impl SlackClients {
    fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::new(),
            runtime: Arc::new(RwLock::new(SlackRuntimeState {
                slack_app_token: config.slack_app_token.clone(),
                slack_bot_token: config.slack_bot_token.clone(),
                config_generation: 0,
            })),
        })
    }

    async fn config_generation(&self) -> u64 {
        self.runtime.read().await.config_generation
    }

    async fn reload_credentials(&self, slack_app_token: String, slack_bot_token: String) {
        let mut guard = self.runtime.write().await;
        guard.slack_app_token = Some(slack_app_token);
        guard.slack_bot_token = Some(slack_bot_token);
        guard.config_generation = guard.config_generation.wrapping_add(1);
    }

    async fn slack_app_token(&self) -> Result<String> {
        self.runtime
            .read()
            .await
            .slack_app_token
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing Slack app token (set via CONFIG_SET slack.app_token/slack.app_token_ref)"
                )
            })
    }

    async fn slack_bot_token(&self) -> Result<String> {
        self.runtime
            .read()
            .await
            .slack_bot_token
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing Slack bot token (set via CONFIG_SET slack.bot_token/slack.bot_token_ref)"
                )
            })
    }

    async fn socket_mode_url(&self) -> Result<Url> {
        #[derive(Deserialize)]
        struct OpenResp {
            ok: bool,
            url: Option<String>,
            error: Option<String>,
        }
        let slack_app_token = self.slack_app_token().await?;

        let resp: OpenResp = self
            .http
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(slack_app_token)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(anyhow::anyhow!(
                "apps.connections.open failed: {}",
                resp.error.unwrap_or_else(|| "unknown".to_string())
            ));
        }

        let url = resp
            .url
            .ok_or_else(|| anyhow::anyhow!("missing url in response"))?;
        Ok(Url::parse(&url)?)
    }

    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
        blocks: Option<serde_json::Value>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }
        if let Some(b) = blocks {
            body["blocks"] = b;
        }

        #[derive(Deserialize)]
        struct PostResp {
            ok: bool,
            error: Option<String>,
        }
        let slack_bot_token = self.slack_bot_token().await?;

        let resp: PostResp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(slack_bot_token)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(anyhow::anyhow!(
                "chat.postMessage failed: {}",
                resp.error.unwrap_or_else(|| "unknown".to_string())
            ));
        }
        Ok(())
    }

    async fn download_private_file(&self, url: &str) -> Result<Vec<u8>> {
        let slack_bot_token = self.slack_bot_token().await?;
        let response = self
            .http
            .get(url)
            .bearer_auth(slack_bot_token)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "slack private download failed with status {status}"
            ));
        }
        Ok(response.bytes().await?.to_vec())
    }

    async fn upload_file(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        filename: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        #[derive(Deserialize)]
        struct GetUploadUrlResp {
            ok: bool,
            upload_url: Option<String>,
            file_id: Option<String>,
            error: Option<String>,
            response_metadata: Option<SlackResponseMetadata>,
        }

        #[derive(Deserialize)]
        struct CompleteUploadResp {
            ok: bool,
            error: Option<String>,
            response_metadata: Option<SlackResponseMetadata>,
        }

        #[derive(Deserialize)]
        struct SlackResponseMetadata {
            messages: Option<Vec<String>>,
        }
        let slack_bot_token = self.slack_bot_token().await?;

        let length = bytes.len().to_string();
        let get_resp: GetUploadUrlResp = self
            .http
            .post("https://slack.com/api/files.getUploadURLExternal")
            .bearer_auth(&slack_bot_token)
            .form(&[("filename", filename), ("length", length.as_str())])
            .send()
            .await?
            .json()
            .await?;
        if !get_resp.ok {
            let detail = get_resp
                .response_metadata
                .and_then(|m| m.messages)
                .map(|msgs| msgs.join(" | "))
                .unwrap_or_else(|| "-".to_string());
            return Err(anyhow::anyhow!(
                "files.getUploadURLExternal failed: {} detail={detail}",
                get_resp.error.unwrap_or_else(|| "unknown".to_string()),
            ));
        }
        let upload_url = get_resp
            .upload_url
            .ok_or_else(|| anyhow::anyhow!("missing upload_url in files.getUploadURLExternal"))?;
        let file_id = get_resp
            .file_id
            .ok_or_else(|| anyhow::anyhow!("missing file_id in files.getUploadURLExternal"))?;

        let upload_response = self
            .http
            .post(upload_url)
            .header(reqwest::header::CONTENT_TYPE, mime)
            .body(bytes)
            .send()
            .await?;
        if !upload_response.status().is_success() {
            return Err(anyhow::anyhow!(
                "external upload failed with status {}",
                upload_response.status()
            ));
        }

        let mut complete_payload = serde_json::json!({
            "files": [{
                "id": file_id,
                "title": filename,
            }],
            "channel_id": channel,
        });
        if let Some(ts) = thread_ts {
            complete_payload["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let complete_resp: CompleteUploadResp = self
            .http
            .post("https://slack.com/api/files.completeUploadExternal")
            .bearer_auth(slack_bot_token)
            .json(&complete_payload)
            .send()
            .await?
            .json()
            .await?;
        if !complete_resp.ok {
            let detail = complete_resp
                .response_metadata
                .and_then(|m| m.messages)
                .map(|msgs| msgs.join(" | "))
                .unwrap_or_else(|| "-".to_string());
            return Err(anyhow::anyhow!(
                "files.completeUploadExternal failed: {} detail={detail}",
                complete_resp.error.unwrap_or_else(|| "unknown".to_string()),
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    envelope_id: String,
    #[serde(default)]
    payload: serde_json::Value,
}

async fn run_inbound_socket_mode(
    config: Config,
    sender: NodeSender,
    slack: Arc<SlackClients>,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
) -> Result<()> {
    let mention_re = Regex::new(r"^<@[^>]+>\s*")?;

    loop {
        let socket_generation = slack.config_generation().await;
        let url = match slack.socket_mode_url().await {
            Ok(url) => url,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    config_generation = socket_generation,
                    "socket mode not ready; waiting for valid slack credentials"
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        tracing::info!(
            host = url.host_str().unwrap_or(""),
            path = url.path(),
            config_generation = socket_generation,
            "socket mode connected"
        );

        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (mut write, mut read) = ws.split();

        while let Some(item) = read.next().await {
            let current_generation = slack.config_generation().await;
            if current_generation != socket_generation {
                tracing::info!(
                    previous_generation = socket_generation,
                    current_generation,
                    "slack runtime config changed; reconnecting socket mode"
                );
                break;
            }
            let msg = match item {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error=?e, "socket read error; reconnecting");
                    break;
                }
            };

            let text = match msg {
                WsMessage::Text(t) => t,
                WsMessage::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                WsMessage::Close(_) => break,
                _ => continue,
            };

            let raw_json: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error=?e, raw=%text, "invalid websocket json");
                    continue;
                }
            };

            match raw_json.get("type").and_then(|v| v.as_str()) {
                Some("hello") => {
                    tracing::debug!("socket mode hello");
                    continue;
                }
                Some("disconnect") => {
                    tracing::warn!(raw=%text, "socket mode disconnect; reconnecting");
                    break;
                }
                _ => {}
            }

            let env: SocketEnvelope = match serde_json::from_value(raw_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error=?e, raw=%text, "invalid socket envelope");
                    continue;
                }
            };

            // ACK immediately.
            let ack = serde_json::json!({ "envelope_id": env.envelope_id });
            if let Err(e) = write.send(WsMessage::Text(ack.to_string())).await {
                tracing::warn!(error=?e, "failed to ack envelope; reconnecting");
                break;
            }
            tracing::debug!("slack envelope acked");

            let payload = env.payload;

            // Parse Slack event callback payload (only what we need).
            let payload_type = payload.get("type").and_then(|v| v.as_str());
            if payload_type != Some("event_callback") {
                continue;
            }
            let team_id = match payload.get("team_id").and_then(|v| v.as_str()) {
                Some(v) => v.to_string(),
                None => continue,
            };
            let event = match payload.get("event") {
                Some(v) => v.clone(),
                None => continue,
            };
            let message_id = payload
                .get("event_id")
                .and_then(|v| v.as_str())
                .or_else(|| event.get("event_id").and_then(|v| v.as_str()))
                .or_else(|| event.get("ts").and_then(|v| v.as_str()))
                .unwrap_or("unknown")
                .to_string();
            if event.get("type").and_then(|v| v.as_str()) != Some("app_mention") {
                continue;
            }
            let user = match event.get("user").and_then(|v| v.as_str()) {
                Some(v) => v.to_string(),
                None => continue,
            };
            let channel = match event.get("channel").and_then(|v| v.as_str()) {
                Some(v) => v.to_string(),
                None => continue,
            };
            let thread_ts = event
                .get("thread_ts")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let raw_text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let content = mention_re.replace(raw_text, "").to_string();

            tracing::debug!(
                team_id = %team_id,
                user = %user,
                channel = %channel,
                thread_ts = thread_ts.as_deref().unwrap_or(""),
                message_id = %message_id,
                content_len = content.len(),
                content_preview = %truncate(&content, 120),
                "inbound app_mention"
            );

            let io_ctx = slack_inbound_io_context(
                &team_id,
                &user,
                &channel,
                thread_ts.as_deref(),
                &message_id,
            );
            let attachments = collect_slack_blob_attachments(
                slack.as_ref(),
                blob_toolkit.as_ref(),
                &blob_payload_cfg,
                &event,
            )
            .await;
            let payload = match build_slack_inbound_payload_from_parts(
                &team_id,
                &user,
                &channel,
                thread_ts.as_deref(),
                &message_id,
                &content,
                &attachments,
            ) {
                Some(payload) => payload,
                None => {
                    tracing::warn!(
                        team_id = %team_id,
                        user = %user,
                        channel = %channel,
                        message_id = %message_id,
                        "dropping inbound event: no valid text/attachments after blob processing"
                    );
                    continue;
                }
            };

            let relay_fragment = build_slack_relay_fragment(
                &config.node_name,
                &team_id,
                &user,
                &channel,
                thread_ts.as_deref(),
                &message_id,
                &content,
                attachments,
                payload.clone(),
            );

            let relay_decision = relay.lock().await.handle_fragment(relay_fragment);
            match relay_decision {
                RelayDecision::Hold => continue,
                RelayDecision::FlushNow(turn) => {
                    dispatch_assembled_turn(
                        sender.clone(),
                        identity.as_ref(),
                        provisioner.as_ref(),
                        inbound.clone(),
                        turn,
                        "relay immediate flush",
                    )
                    .await;
                    continue;
                }
                RelayDecision::DropDuplicate => {
                    tracing::debug!(message_id = %message_id, "relay dedup hit; dropping inbound fragment");
                    continue;
                }
                RelayDecision::RejectCapacity => {
                    tracing::warn!(
                        team_id = %team_id,
                        user = %user,
                        channel = %channel,
                        message_id = %message_id,
                        "relay capacity reached; using fail-open direct inbound path"
                    );
                }
                RelayDecision::DropExpired => {
                    tracing::warn!(
                        team_id = %team_id,
                        user = %user,
                        channel = %channel,
                        message_id = %message_id,
                        "relay dropped fragment due to expired/invalid session state"
                    );
                    continue;
                }
            }

            let outcome = inbound
                .lock()
                .await
                .process_inbound(
                    identity.as_ref(),
                    Some(provisioner.as_ref()),
                    ResolveOrCreateInput {
                        channel: "slack".to_string(),
                        external_id: slack_external_id(&config.node_name, &user),
                        src_ilk_override: None,
                        tenant_id: None,
                        tenant_hint: Some(team_id.to_string()),
                        attributes: serde_json::json!({ "team_id": team_id }),
                    },
                    None,
                    io_ctx,
                    payload,
                )
                .await;

            dispatch_inbound_outcome(sender.clone(), outcome, "direct inbound").await;
        }
    }
}

fn build_slack_inbound_payload_from_parts(
    team_id: &str,
    user: &str,
    channel: &str,
    thread_ts: Option<&str>,
    message_id: &str,
    content: &str,
    attachments: &[InboundAttachmentInput],
) -> Option<Value> {
    if content.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    let payload = fluxbee_sdk::payload::TextV1Payload::new(
        content,
        attachments.iter().map(|a| a.blob_ref.clone()).collect(),
    );
    match payload.to_value() {
        Ok(mut payload) => {
            attach_slack_raw_stub(&mut payload, team_id, user, channel, thread_ts, message_id);
            Some(payload)
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to build base text/v1 inbound payload"
            );
            None
        }
    }
}

fn build_slack_relay_fragment(
    node_name: &str,
    team_id: &str,
    user: &str,
    channel: &str,
    thread_ts: Option<&str>,
    message_id: &str,
    content: &str,
    attachments: Vec<InboundAttachmentInput>,
    raw_payload: Value,
) -> RelayFragment {
    RelayFragment {
        relay_key: slack_relay_key(team_id, user, channel, thread_ts),
        fragment_id: message_id.to_string(),
        received_at_ms: now_epoch_ms(),
        content_text: Some(content.to_string()),
        attachments: attachments
            .into_iter()
            .filter_map(|attachment| serde_json::to_value(attachment.blob_ref).ok())
            .collect(),
        raw_payload: Some(raw_payload),
        io_context: slack_inbound_io_context(team_id, user, channel, thread_ts, message_id),
        identity_input: ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: slack_external_id(node_name, user),
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: Some(team_id.to_string()),
            attributes: serde_json::json!({ "team_id": team_id }),
        },
        dst_node_override: None,
        flush_hints: RelayFlushHints::default(),
    }
}

async fn dispatch_assembled_turn(
    sender: NodeSender,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
    inbound: Arc<Mutex<InboundProcessor>>,
    turn: AssembledTurn,
    send_context: &str,
) {
    let outcome = inbound
        .lock()
        .await
        .process_assembled_turn(identity, Some(provisioner), turn)
        .await;

    dispatch_inbound_outcome(sender, outcome, send_context).await;
}

async fn dispatch_inbound_outcome(sender: NodeSender, outcome: InboundOutcome, send_context: &str) {
    match outcome {
        InboundOutcome::SendNow(msg) => {
            let trace_id = msg.routing.trace_id.clone();
            let has_src_ilk = msg.meta.src_ilk.as_deref().is_some_and(|v| !v.is_empty());
            if let Err(e) = sender.send(msg).await {
                tracing::warn!(error=?e, %trace_id, context = send_context, "failed to send to router");
            } else {
                tracing::debug!(%trace_id, has_src_ilk, context = send_context, "sent to router");
            }
        }
        InboundOutcome::DroppedDuplicate => {
            tracing::debug!(context = send_context, "dedup hit; dropping inbound");
        }
    }
}

async fn run_relay_flush_loop(
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    sender: NodeSender,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let turns = relay.lock().await.flush_expired(now_epoch_ms());
        for turn in turns {
            dispatch_assembled_turn(
                sender.clone(),
                identity.as_ref(),
                provisioner.as_ref(),
                inbound.clone(),
                turn,
                "relay scheduled flush",
            )
            .await;
        }
    }
}

fn slack_relay_key(team_id: &str, user: &str, channel: &str, thread_ts: Option<&str>) -> String {
    let thread_or_channel = thread_ts.unwrap_or(channel);
    format!("slack:{team_id}:{channel}:{thread_or_channel}:{user}")
}

fn slack_relay_policy(config: &Config, effective_config: Option<&Value>) -> Result<RelayPolicy> {
    let relay_cfg = extract_runtime_relay_config(effective_config, &config.relay)?;
    slack_relay_policy_from_config(&relay_cfg)
}

fn slack_relay_policy_from_config(relay_cfg: &SlackRelayConfig) -> Result<RelayPolicy> {
    let mut policy = RelayPolicy {
        enabled: relay_cfg.window_ms > 0,
        relay_window_ms: relay_cfg.window_ms,
        max_open_sessions: relay_cfg.max_open_sessions,
        max_fragments_per_session: relay_cfg.max_fragments_per_session,
        max_bytes_per_session: relay_cfg.max_bytes_per_session,
        ..RelayPolicy::default()
    };
    if policy.relay_window_ms == 0 {
        policy.enabled = false;
        policy.stale_session_ttl_ms = 0;
    } else {
        policy.stale_session_ttl_ms = policy
            .relay_window_ms
            .saturating_mul(4)
            .max(policy.relay_window_ms);
    }
    policy.validate().map_err(|err| anyhow::anyhow!(err))?;
    Ok(policy)
}

fn extract_runtime_relay_config(
    effective_config: Option<&Value>,
    defaults: &SlackRelayConfig,
) -> Result<SlackRelayConfig> {
    let relay_root = effective_config
        .and_then(|cfg| cfg.get("io"))
        .and_then(|io| io.get("relay"));
    let window_ms = relay_root
        .and_then(|relay| relay.get("window_ms"))
        .map(parse_config_u64)
        .transpose()?
        .unwrap_or(defaults.window_ms);
    let max_open_sessions = relay_root
        .and_then(|relay| relay.get("max_open_sessions"))
        .map(parse_config_usize)
        .transpose()?
        .unwrap_or(defaults.max_open_sessions);
    let max_fragments_per_session = relay_root
        .and_then(|relay| relay.get("max_fragments_per_session"))
        .map(parse_config_usize)
        .transpose()?
        .unwrap_or(defaults.max_fragments_per_session);
    let max_bytes_per_session = relay_root
        .and_then(|relay| relay.get("max_bytes_per_session"))
        .map(parse_config_usize)
        .transpose()?
        .unwrap_or(defaults.max_bytes_per_session);

    let relay = SlackRelayConfig {
        window_ms,
        max_open_sessions,
        max_fragments_per_session,
        max_bytes_per_session,
    };
    validate_slack_relay_config(&relay)?;
    Ok(relay)
}

fn parse_config_u64(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("relay config value must be a non-negative integer"))
}

fn parse_config_usize(value: &Value) -> Result<usize> {
    let raw = parse_config_u64(value)?;
    usize::try_from(raw).map_err(|_| anyhow::anyhow!("relay config value exceeds usize range"))
}

fn validate_slack_relay_config(relay: &SlackRelayConfig) -> Result<()> {
    if relay.max_open_sessions == 0 {
        return Err(anyhow::anyhow!(
            "io.relay.max_open_sessions must be greater than zero"
        ));
    }
    if relay.max_fragments_per_session == 0 {
        return Err(anyhow::anyhow!(
            "io.relay.max_fragments_per_session must be greater than zero"
        ));
    }
    if relay.max_bytes_per_session == 0 {
        return Err(anyhow::anyhow!(
            "io.relay.max_bytes_per_session must be greater than zero"
        ));
    }
    Ok(())
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn collect_slack_blob_attachments(
    slack: &SlackClients,
    blob_toolkit: &fluxbee_sdk::blob::BlobToolkit,
    blob_payload_cfg: &IoTextBlobConfig,
    event: &Value,
) -> Vec<InboundAttachmentInput> {
    let Some(files) = event.get("files").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut total_size: u64 = 0;
    let allowed_mimes = blob_payload_cfg
        .allowed_mimes
        .iter()
        .map(|m| normalize_mime(m))
        .collect::<HashSet<_>>();

    for file in files {
        if out.len() >= blob_payload_cfg.max_attachments {
            let err = IoBlobContractError::TooManyAttachments {
                actual: files.len(),
                max: blob_payload_cfg.max_attachments,
            };
            tracing::warn!(code = err.canonical_code(), error = %err, "attachment skipped");
            break;
        }

        let file_id = file
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown-file");
        let filename = file
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(file_id);
        let mime = file
            .get("mimetype")
            .and_then(|v| v.as_str())
            .unwrap_or("application/octet-stream")
            .to_string();
        if !allowed_mimes.contains(&normalize_mime(&mime)) {
            let err = IoBlobContractError::UnsupportedMime { mime: mime.clone() };
            tracing::warn!(
                file_id = %file_id,
                filename = %filename,
                code = err.canonical_code(),
                error = %err,
                "attachment skipped"
            );
            continue;
        }

        let size_hint = file.get("size").and_then(|v| v.as_u64());
        if let Some(size) = size_hint {
            if size > blob_payload_cfg.max_attachment_bytes {
                let err = IoBlobContractError::BlobTooLarge {
                    actual: size,
                    max: blob_payload_cfg.max_attachment_bytes,
                };
                tracing::warn!(
                    file_id = %file_id,
                    filename = %filename,
                    code = err.canonical_code(),
                    error = %err,
                    "attachment skipped"
                );
                continue;
            }
            if total_size.saturating_add(size) > blob_payload_cfg.max_total_attachment_bytes {
                let err = IoBlobContractError::BlobTooLarge {
                    actual: total_size.saturating_add(size),
                    max: blob_payload_cfg.max_total_attachment_bytes,
                };
                tracing::warn!(
                    file_id = %file_id,
                    filename = %filename,
                    code = err.canonical_code(),
                    error = %err,
                    "attachment skipped"
                );
                continue;
            }
        }

        let download_url = file
            .get("url_private_download")
            .and_then(|v| v.as_str())
            .or_else(|| file.get("url_private").and_then(|v| v.as_str()));
        let Some(download_url) = download_url else {
            let err = IoBlobContractError::Blob(fluxbee_sdk::blob::BlobError::Io(
                "missing Slack file download URL".to_string(),
            ));
            tracing::warn!(
                file_id = %file_id,
                filename = %filename,
                code = err.canonical_code(),
                error = %err,
                "attachment skipped"
            );
            continue;
        };

        let bytes = match slack.download_private_file(download_url).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let err = IoBlobContractError::Blob(fluxbee_sdk::blob::BlobError::Io(format!(
                    "slack download failed: {error}"
                )));
                tracing::warn!(
                    file_id = %file_id,
                    filename = %filename,
                    code = err.canonical_code(),
                    error = %err,
                    "attachment skipped"
                );
                continue;
            }
        };

        let actual_size = bytes.len() as u64;
        if actual_size > blob_payload_cfg.max_attachment_bytes {
            let err = IoBlobContractError::BlobTooLarge {
                actual: actual_size,
                max: blob_payload_cfg.max_attachment_bytes,
            };
            tracing::warn!(
                file_id = %file_id,
                filename = %filename,
                code = err.canonical_code(),
                error = %err,
                "attachment skipped"
            );
            continue;
        }
        if total_size.saturating_add(actual_size) > blob_payload_cfg.max_total_attachment_bytes {
            let err = IoBlobContractError::BlobTooLarge {
                actual: total_size.saturating_add(actual_size),
                max: blob_payload_cfg.max_total_attachment_bytes,
            };
            tracing::warn!(
                file_id = %file_id,
                filename = %filename,
                code = err.canonical_code(),
                error = %err,
                "attachment skipped"
            );
            continue;
        }

        let blob_ref = match blob_toolkit.put_bytes(&bytes, filename, &mime) {
            Ok(blob_ref) => blob_ref,
            Err(error) => {
                let err = IoBlobContractError::Blob(error);
                tracing::warn!(
                    file_id = %file_id,
                    filename = %filename,
                    code = err.canonical_code(),
                    error = %err,
                    "attachment skipped"
                );
                continue;
            }
        };
        if let Err(error) = blob_toolkit.promote(&blob_ref) {
            let err = IoBlobContractError::Blob(error);
            tracing::warn!(
                file_id = %file_id,
                filename = %filename,
                code = err.canonical_code(),
                error = %err,
                "attachment skipped"
            );
            continue;
        }

        total_size = total_size.saturating_add(actual_size);
        out.push(InboundAttachmentInput { blob_ref });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{
        build_system_reply, default_slack_allowed_mimes, extract_runtime_relay_config,
        slack_relay_policy_from_config, SlackRelayConfig,
    };
    use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing};
    use serde_json::json;

    #[test]
    fn default_slack_allowed_mimes_include_office_common_types() {
        let allowed = default_slack_allowed_mimes();

        assert!(allowed.contains(
            &"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
        ));
        assert!(allowed.contains(
            &"application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        ));
        assert!(allowed.contains(
            &"application/vnd.openxmlformats-officedocument.presentationml.presentation"
                .to_string()
        ));
        assert!(allowed.contains(&"text/csv".to_string()));
    }

    #[test]
    fn default_slack_allowed_mimes_preserve_existing_core_types() {
        let allowed = default_slack_allowed_mimes();

        assert!(allowed.contains(&"image/png".to_string()));
        assert!(allowed.contains(&"image/jpeg".to_string()));
        assert!(allowed.contains(&"application/pdf".to_string()));
        assert!(allowed.contains(&"application/json".to_string()));
    }

    #[test]
    fn extract_runtime_relay_config_uses_effective_config_values() {
        let defaults = SlackRelayConfig::default();
        let relay = extract_runtime_relay_config(
            Some(&json!({
                "io": {
                    "relay": {
                        "window_ms": 2500,
                        "max_open_sessions": 2000,
                        "max_fragments_per_session": 6,
                        "max_bytes_per_session": 131072
                    }
                }
            })),
            &defaults,
        )
        .expect("relay config");

        assert_eq!(relay.window_ms, 2500);
        assert_eq!(relay.max_open_sessions, 2000);
        assert_eq!(relay.max_fragments_per_session, 6);
        assert_eq!(relay.max_bytes_per_session, 131072);
    }

    #[test]
    fn extract_runtime_relay_config_falls_back_to_defaults_when_absent() {
        let defaults = SlackRelayConfig {
            window_ms: 1700,
            max_open_sessions: 222,
            max_fragments_per_session: 4,
            max_bytes_per_session: 99_999,
        };
        let relay = extract_runtime_relay_config(Some(&json!({ "io": {} })), &defaults)
            .expect("relay defaults");

        assert_eq!(relay, defaults);
    }

    #[test]
    fn slack_relay_policy_from_config_disables_passthrough_when_window_zero() {
        let relay = SlackRelayConfig {
            window_ms: 0,
            ..SlackRelayConfig::default()
        };

        let policy = slack_relay_policy_from_config(&relay).expect("policy");

        assert!(!policy.enabled);
        assert_eq!(policy.relay_window_ms, 0);
    }

    #[test]
    fn extract_runtime_relay_config_rejects_zero_limits() {
        let err = extract_runtime_relay_config(
            Some(&json!({
                "io": {
                    "relay": {
                        "max_open_sessions": 0
                    }
                }
            })),
            &SlackRelayConfig::default(),
        )
        .expect_err("must reject zero");

        assert_eq!(
            err.to_string(),
            "io.relay.max_open_sessions must be greater than zero"
        );
    }

    #[test]
    fn build_system_reply_preserves_canonical_conversation_carrier() {
        let incoming = WireMessage {
            routing: Routing {
                src: "node-src".to_string(),
                src_l2_name: None,
                dst: Destination::Resolve,
                ttl: 16,
                trace_id: "trace-1".to_string(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some("PING".to_string()),
                src_ilk: Some("ilk:src".to_string()),
                dst_ilk: Some("ilk:dst".to_string()),
                ich: Some("slack://U123".to_string()),
                thread_id: Some("thread:canonical-1".to_string()),
                thread_seq: Some(7),
                context: Some(json!({
                    "io": {
                        "reply_target": {
                            "kind": "slack_post",
                            "address": "C123",
                            "params": { "thread_ts": "171234.567" }
                        }
                    }
                })),
                ..Meta::default()
            },
            payload: json!({}),
        };

        let reply = build_system_reply(
            &incoming,
            "IO.slack.test@motherbee",
            "PONG",
            json!({ "ok": true }),
        );

        assert_eq!(reply.meta.ich.as_deref(), Some("slack://U123"));
        assert_eq!(reply.meta.thread_id.as_deref(), Some("thread:canonical-1"));
        assert_eq!(reply.meta.thread_seq, Some(7));
        assert_eq!(
            reply
                .meta
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("io"))
                .and_then(|io| io.get("reply_target"))
                .and_then(|target| target.get("address"))
                .and_then(|value| value.as_str()),
            Some("C123")
        );
    }
}

fn attach_slack_raw_stub(
    payload: &mut Value,
    team_id: &str,
    user: &str,
    channel: &str,
    thread_ts: Option<&str>,
    message_id: &str,
) {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "raw".to_string(),
            serde_json::json!({
                "slack": {
                    "team_id": team_id,
                    "user": user,
                    "channel": channel,
                    "thread_ts": thread_ts,
                    "message_id": message_id
                }
            }),
        );
    }
}

fn normalize_mime(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn slack_external_id(node_name: &str, user_id: &str) -> String {
    format!("{node_name}:{user_id}")
}

async fn run_outbound_loop(
    inbox: Arc<Mutex<RouterInbox>>,
    sender: NodeSender,
    node_name: String,
    state_dir: PathBuf,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    inbound: Arc<Mutex<InboundProcessor>>,
    slack: Arc<SlackClients>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
) -> Result<()> {
    loop {
        let next_msg = {
            let mut guard = inbox.lock().await;
            guard.recv_next_timeout(Duration::from_secs(1)).await
        };
        let next_msg = match next_msg {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "router inbox recv failed in outbound loop; continuing"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        };
        let Some(msg) = next_msg else {
            continue;
        };

        let payload_type = msg
            .payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let meta_msg = msg.meta.msg.as_deref().unwrap_or("");
        tracing::debug!(
            trace_id = %msg.routing.trace_id,
            msg_type = %msg.meta.msg_type,
            msg = %meta_msg,
            payload_type = %payload_type,
            "outbound received from router"
        );

        if let Some(response) = handle_io_control_plane_message(
            &msg,
            &node_name,
            sender.uuid(),
            &state_dir,
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            inbound.clone(),
            slack.clone(),
            relay.clone(),
        )
        .await
        {
            if let Err(err) = sender.send(response).await {
                tracing::warn!(
                    error = ?err,
                    trace_id = %msg.routing.trace_id,
                    "failed to send CONFIG_RESPONSE"
                );
            }
            continue;
        }

        if is_control_plane_msg_type(&msg.meta.msg_type)
            && msg
                .meta
                .msg
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("CONFIG_CHANGED"))
        {
            tracing::info!(
                trace_id = %msg.routing.trace_id,
                "received CONFIG_CHANGED (informative); runtime apply remains CONFIG_SET-owned"
            );
            continue;
        }

        let Some(meta_context) = msg.meta.context.as_ref() else {
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                msg = %meta_msg,
                payload_type = %payload_type,
                "skipping outbound: missing meta.context"
            );
            continue;
        };
        let Some(target) = extract_slack_post_target(meta_context) else {
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                msg = %meta_msg,
                payload_type = %payload_type,
                "skipping outbound: missing/unsupported io.reply_target for Slack"
            );
            continue;
        };

        let mut sent_any = false;
        if let Some(frontdesk_result) = parse_frontdesk_result_payload(&msg.payload) {
            let human_message = if frontdesk_result.human_message.trim().is_empty() {
                format!(
                    "Frontdesk devolvió {} ({}) sin mensaje humano.",
                    frontdesk_result.status, frontdesk_result.result_code
                )
            } else {
                frontdesk_result.human_message.clone()
            };
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                result_code = %frontdesk_result.result_code,
                status = %frontdesk_result.status,
                "rendering frontdesk_result for slack outbound"
            );
            if let Err(error) = slack
                .post_message(
                    &target.channel_id,
                    human_message.as_str(),
                    target.thread_ts.as_deref(),
                    None,
                )
                .await
            {
                tracing::warn!(error = ?error, "failed to send slack frontdesk_result message");
            } else {
                sent_any = true;
            }
        }

        if sent_any {
            continue;
        }

        match resolve_text_v1_for_outbound(
            blob_toolkit.as_ref(),
            &blob_payload_cfg,
            &msg.payload,
            true,
        )
        .await
        {
            Ok(resolved) => {
                if !resolved.text.trim().is_empty() {
                    let blocks = msg.payload.get("blocks").cloned();
                    tracing::debug!(
                        slack_channel = target.channel_id,
                        slack_thread_ts = target.thread_ts.as_deref().unwrap_or(""),
                        text_len = resolved.text.len(),
                        text_preview = %truncate(&resolved.text, 120),
                        "sending slack message"
                    );
                    if let Err(e) = slack
                        .post_message(
                            &target.channel_id,
                            &resolved.text,
                            target.thread_ts.as_deref(),
                            blocks,
                        )
                        .await
                    {
                        tracing::warn!(error=?e, "failed to send slack message");
                    } else {
                        sent_any = true;
                        tracing::debug!("slack message sent");
                    }
                }

                for attachment in resolved.attachments {
                    let filename = if attachment.blob_ref.filename_original.trim().is_empty() {
                        attachment.blob_ref.blob_name.as_str()
                    } else {
                        attachment.blob_ref.filename_original.as_str()
                    };
                    match std::fs::read(&attachment.path) {
                        Ok(bytes) => {
                            if let Err(error) = slack
                                .upload_file(
                                    &target.channel_id,
                                    target.thread_ts.as_deref(),
                                    filename,
                                    &attachment.blob_ref.mime,
                                    bytes,
                                )
                                .await
                            {
                                tracing::warn!(
                                    error = ?error,
                                    blob_name = %attachment.blob_ref.blob_name,
                                    "failed to upload attachment to Slack"
                                );
                            } else {
                                sent_any = true;
                                tracing::debug!(
                                    blob_name = %attachment.blob_ref.blob_name,
                                    filename = %filename,
                                    "slack attachment uploaded"
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                blob_name = %attachment.blob_ref.blob_name,
                                "failed to read resolved attachment path"
                            );
                        }
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    code = err.canonical_code(),
                    error = %err,
                    trace_id = %msg.routing.trace_id,
                    "failed to resolve outbound text/v1 payload"
                );
                let fallback = format!(
                    "Error ({}) al procesar contenido/adjuntos.",
                    err.canonical_code()
                );
                if let Err(error) = slack
                    .post_message(
                        &target.channel_id,
                        &fallback,
                        target.thread_ts.as_deref(),
                        None,
                    )
                    .await
                {
                    tracing::warn!(error=?error, "failed to send outbound fallback message");
                } else {
                    sent_any = true;
                }
            }
        }

        if !sent_any {
            let fallback = "No pude entregar el mensaje en Slack (sin contenido util).";
            if let Err(error) = slack
                .post_message(
                    &target.channel_id,
                    fallback,
                    target.thread_ts.as_deref(),
                    None,
                )
                .await
            {
                tracing::warn!(error=?error, "failed to send final outbound fallback");
            }
        }
    }
}

async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    inbound: Arc<Mutex<InboundProcessor>>,
    slack: Arc<SlackClients>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
) -> Option<fluxbee_sdk::protocol::Message> {
    let command = msg.meta.msg.as_deref().unwrap_or_default();
    if !is_control_plane_msg_type(&msg.meta.msg_type) {
        return None;
    }
    if command.eq_ignore_ascii_case("PING") {
        let state = control_plane.read().await.clone();
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
        });
        return Some(build_system_reply(msg, control_src, "PONG", payload));
    }
    if command.eq_ignore_ascii_case("STATUS") {
        let state = control_plane.read().await.clone();
        let metrics = control_metrics.snapshot();
        let payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
            "config_source": state.config_source.as_str(),
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": state.last_error,
            "metrics": {
                "control_plane": metrics
            }
        });
        return Some(build_system_reply(
            msg,
            control_src,
            "STATUS_RESPONSE",
            payload,
        ));
    }
    if !command.eq_ignore_ascii_case("CONFIG_GET") && !command.eq_ignore_ascii_case("CONFIG_SET") {
        return None;
    }

    let payload = match parse_and_validate_io_control_plane_request(msg, node_name) {
        Ok(IoControlPlaneRequest::Get(_)) => {
            let state = control_plane.read().await.clone();
            let redacted = redact_state(&state, adapter_contract);
            log_config_get_served(msg.routing.trace_id.as_str(), node_name, &redacted);
            let mut payload = build_io_config_get_response_payload(
                node_name,
                &redacted,
                build_io_adapter_contract_payload(
                    adapter_contract,
                    state.effective_config.as_ref(),
                ),
            );
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "metrics".to_string(),
                    serde_json::json!({
                        "control_plane": control_metrics.snapshot()
                    }),
                );
            }
            payload
        }
        Ok(IoControlPlaneRequest::Set(set_payload)) => {
            apply_io_config_set(
                &set_payload,
                node_name,
                state_dir,
                control_plane.clone(),
                control_metrics,
                adapter_contract,
                inbound,
                slack,
                relay,
            )
            .await
        }
        Err(err) => {
            let state = control_plane.read().await.clone();
            let redacted = redact_state(&state, adapter_contract);
            let err_text = err.to_string();
            log_control_plane_request_rejected(
                msg.routing.trace_id.as_str(),
                node_name,
                err.code(),
                err_text.as_str(),
            );
            build_io_config_set_error_payload(node_name, &redacted, err.code(), err_text)
        }
    };

    let mut response = build_io_config_response_message(msg, payload);
    response.routing.src = control_src.to_string();
    Some(response)
}

fn is_control_plane_msg_type(msg_type: &str) -> bool {
    msg_type.eq_ignore_ascii_case(SYSTEM_KIND) || msg_type.eq_ignore_ascii_case("admin")
}

async fn apply_io_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    state_dir: &Path,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    inbound: Arc<Mutex<InboundProcessor>>,
    slack: Arc<SlackClients>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
) -> Value {
    let mut state = control_plane.write().await;

    if let Err(err) = ensure_config_version_advances(payload.config_version, state.config_version) {
        log_config_set_stale_rejected(node_name, payload.config_version, state.config_version);
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            state.config_version,
            err.code(),
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            err.code(),
            err.to_string(),
        );
    }

    let effective = match apply_adapter_config_replace(adapter_contract, &payload.config) {
        Ok(cfg) => cfg,
        Err(err) => {
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: err.code().to_string(),
                message: err.to_string(),
            });
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                err.code(),
                err.to_string(),
            );
        }
    };

    let previous_effective = state.effective_config.clone();
    let mut apply_hot = Vec::<String>::new();
    let mut apply_reinit = Vec::<String>::new();
    let mut apply_restart_required = Vec::<String>::new();

    if section_changed(previous_effective.as_ref(), &effective, &["identity"]) {
        apply_restart_required.push("identity.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["io", "blob"]) {
        apply_restart_required.push("io.blob.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["node"]) {
        apply_restart_required.push("node.*".to_string());
    }
    if section_changed(previous_effective.as_ref(), &effective, &["runtime"]) {
        apply_restart_required.push("runtime.*".to_string());
    }

    let app_and_bot = if slack_credentials_changed(previous_effective.as_ref(), &effective) {
        match extract_runtime_slack_credentials(&effective) {
            Ok(pair) => Some(pair),
            Err(err) => {
                let err_text = err.to_string();
                log_config_set_invalid_runtime_credentials(
                    node_name,
                    payload.config_version,
                    err_text.as_str(),
                );
                state.current_state = IoNodeLifecycleState::FailedConfig;
                state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err_text.clone(),
                });
                control_metrics.record_config_set_error(
                    state.current_state.as_str(),
                    payload.config_version,
                    "invalid_config",
                );
                let redacted = redact_state(&state, adapter_contract);
                return build_io_config_set_error_payload(
                    node_name,
                    &redacted,
                    "invalid_config",
                    err_text,
                );
            }
        }
    } else {
        None
    };

    state.current_state = IoNodeLifecycleState::Configured;
    state.config_source = IoConfigSource::Dynamic;
    state.schema_version = payload.schema_version;
    state.config_version = payload.config_version;
    state.effective_config = Some(effective.clone());
    state.last_error = None;

    if let Err(err) = persist_io_control_plane_state(state_dir, node_name, &state) {
        let err_text = err.to_string();
        log_config_set_persist_error(
            node_name,
            payload.schema_version,
            payload.config_version,
            err_text.as_str(),
        );
        state.current_state = IoNodeLifecycleState::FailedConfig;
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: "config_persist_error".to_string(),
            message: err_text.clone(),
        });
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            payload.config_version,
            "config_persist_error",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            "config_persist_error",
            err_text,
        );
    }

    if extract_runtime_dst_node(previous_effective.as_ref())
        != extract_runtime_dst_node(Some(&effective))
    {
        if let Some(dst_node) = extract_runtime_dst_node(Some(&effective)) {
            inbound.lock().await.set_dst_node(Some(dst_node));
            apply_hot.push("io.dst_node".to_string());
        }
    }

    match extract_runtime_relay_config(Some(&effective), &SlackRelayConfig::default())
        .and_then(|relay_cfg| slack_relay_policy_from_config(&relay_cfg))
    {
        Ok(policy) => {
            let mut relay_guard = relay.lock().await;
            let relay_changed = relay_guard.policy() != &policy;
            if let Err(err) = relay_guard.replace_policy(policy.clone()) {
                let err_text = err.to_string();
                state.current_state = IoNodeLifecycleState::FailedConfig;
                state.last_error = Some(IoControlPlaneErrorInfo {
                    code: "invalid_config".to_string(),
                    message: err_text.clone(),
                });
                control_metrics.record_config_set_error(
                    state.current_state.as_str(),
                    payload.config_version,
                    "invalid_config",
                );
                let redacted = redact_state(&state, adapter_contract);
                return build_io_config_set_error_payload(
                    node_name,
                    &redacted,
                    "invalid_config",
                    err_text,
                );
            }
            drop(relay_guard);
            if relay_changed {
                tracing::info!(
                    node_name = %node_name,
                    relay_enabled = policy.enabled,
                    relay_window_ms = policy.relay_window_ms,
                    relay_max_open_sessions = policy.max_open_sessions,
                    relay_max_fragments = policy.max_fragments_per_session,
                    relay_max_bytes = policy.max_bytes_per_session,
                    "io-slack relay policy hot-applied"
                );
                apply_hot.push("io.relay.*".to_string());
            }
        }
        Err(err) => {
            let err_text = err.to_string();
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: "invalid_config".to_string(),
                message: err_text.clone(),
            });
            control_metrics.record_config_set_error(
                state.current_state.as_str(),
                payload.config_version,
                "invalid_config",
            );
            let redacted = redact_state(&state, adapter_contract);
            return build_io_config_set_error_payload(
                node_name,
                &redacted,
                "invalid_config",
                err_text,
            );
        }
    }

    if let Some((app_token, bot_token)) = app_and_bot {
        slack.reload_credentials(app_token, bot_token).await;
        apply_reinit.push("slack.credentials".to_string());
    }
    control_metrics.record_config_set_ok(state.current_state.as_str(), state.config_version);

    log_config_set_applied(
        node_name,
        payload.schema_version,
        payload.config_version,
        &apply_hot,
        &apply_reinit,
        &apply_restart_required,
    );

    let redacted = redact_state(&state, adapter_contract);
    let mut response = build_io_config_set_ok_payload(node_name, &redacted);
    if let Some(obj) = response.as_object_mut() {
        obj.insert(
            "apply".to_string(),
            serde_json::json!({
                "mode": "hot_reload",
                "hot_applied": apply_hot,
                "reinit_performed": apply_reinit,
                "restart_required": apply_restart_required
            }),
        );
    }
    response
}

fn extract_runtime_dst_node(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|cfg| cfg.get("io"))
        .and_then(|io| io.get("dst_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn section_changed(
    previous_effective: Option<&Value>,
    current_effective: &Value,
    path: &[&str],
) -> bool {
    value_at_path(previous_effective, path) != value_at_path(Some(current_effective), path)
}

fn value_at_path<'a>(root: Option<&'a Value>, path: &[&str]) -> Option<&'a Value> {
    let mut current = root?;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn slack_credentials_changed(
    previous_effective: Option<&Value>,
    current_effective: &Value,
) -> bool {
    let app_prev = extract_slack_credential_marker(previous_effective, true);
    let app_curr = extract_slack_credential_marker(Some(current_effective), true);
    let bot_prev = extract_slack_credential_marker(previous_effective, false);
    let bot_curr = extract_slack_credential_marker(Some(current_effective), false);
    app_prev != app_curr || bot_prev != bot_curr
}

fn extract_slack_credential_marker(effective_config: Option<&Value>, app: bool) -> Option<String> {
    let slack = effective_config
        .and_then(|cfg| cfg.get("slack"))
        .and_then(Value::as_object)?;
    let (inline_keys, ref_keys) = if app {
        (
            ["app_token", "slack_app_token"],
            ["app_token_ref", "slack_app_token_ref"],
        )
    } else {
        (
            ["bot_token", "slack_bot_token"],
            ["bot_token_ref", "slack_bot_token_ref"],
        )
    };
    for key in inline_keys {
        let Some(value) = slack.get(key).and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if !value.is_empty() {
            return Some(format!("inline:{value}"));
        }
    }
    for key in ref_keys {
        let Some(value) = slack.get(key).and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if !value.is_empty() {
            return Some(format!("ref:{value}"));
        }
    }
    None
}

fn extract_runtime_slack_credentials(effective_config: &Value) -> Result<(String, String)> {
    let slack = effective_config
        .get("slack")
        .ok_or_else(|| anyhow::anyhow!("missing config.slack object"))?;
    let app_token = resolve_secret_from_config_value(
        slack,
        &["app_token", "slack_app_token"],
        &["app_token_ref", "slack_app_token_ref"],
    )
    .ok_or_else(|| anyhow::anyhow!("missing usable Slack app credential in config.slack"))?;
    let bot_token = resolve_secret_from_config_value(
        slack,
        &["bot_token", "slack_bot_token"],
        &["bot_token_ref", "slack_bot_token_ref"],
    )
    .ok_or_else(|| anyhow::anyhow!("missing usable Slack bot credential in config.slack"))?;
    Ok((app_token, bot_token))
}

fn resolve_secret_from_config_value(
    root: &Value,
    value_keys: &[&str],
    ref_keys: &[&str],
) -> Option<String> {
    for key in value_keys {
        let Some(value) = root.get(*key).and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    for key in ref_keys {
        let Some(reference) = root.get(*key).and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if let Some(var) = reference.strip_prefix("env:") {
            if let Some(resolved) = env(var) {
                return Some(resolved);
            }
        }
    }
    None
}

fn redact_state(
    state: &IoControlPlaneState,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> IoControlPlaneState {
    let mut redacted = state.clone();
    redacted.effective_config = state
        .effective_config
        .as_ref()
        .map(|cfg| adapter_contract.redact_effective_config(cfg));
    redacted
}

fn build_system_reply(
    incoming: &WireMessage,
    control_src: &str,
    response_msg: &str,
    payload: Value,
) -> WireMessage {
    let dst = if incoming.routing.src.trim().is_empty() {
        Destination::Broadcast
    } else {
        Destination::Unicast(incoming.routing.src.clone())
    };
    WireMessage {
        routing: Routing {
            src: control_src.to_string(),
            src_l2_name: None,
            dst,
            ttl: incoming.routing.ttl.max(1),
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(response_msg.to_string()),
            src_ilk: incoming.meta.src_ilk.clone(),
            dst_ilk: incoming.meta.dst_ilk.clone(),
            ich: incoming.meta.ich.clone(),
            thread_id: incoming.meta.thread_id.clone(),
            thread_seq: incoming.meta.thread_seq,
            ctx: None,
            ctx_seq: None,
            ctx_window: None,
            memory_package: incoming.meta.memory_package.clone(),
            scope: incoming.meta.scope.clone(),
            target: incoming.meta.target.clone(),
            action: Some(response_msg.to_string()),
            priority: incoming.meta.priority.clone(),
            context: incoming.meta.context.clone(),
            ..Meta::default()
        },
        payload,
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = s.chars().take(max_chars).collect::<String>();
    out.push('…');
    out
}
