#![forbid(unsafe_code)]

use anyhow::Result;
use fluxbee_sdk::{
    connect, managed_node_config_path, managed_node_name, NodeConfig, NodeSender, NodeUuidMode,
};
use futures_util::{SinkExt, StreamExt};
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_context::{extract_slack_post_target, slack_inbound_io_context};
use io_common::provision::{FluxbeeIdentityProvisioner, IdentityProvisionConfig, RouterInbox};
use io_common::text_v1_blob::{
    build_text_v1_inbound_payload, resolve_text_v1_for_outbound, InboundAttachmentInput,
    IoBlobContractError, IoBlobRuntimeConfig, IoTextBlobConfig,
};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
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
        },
    )));
    let blob_toolkit = Arc::new(
        config
            .blob_runtime
            .build_toolkit()
            .map_err(|err| anyhow::anyhow!("invalid blob runtime config: {err}"))?,
    );
    let blob_payload_cfg = config.blob_runtime.text_v1.clone();
    let sessionizer = if config.slack_session_window_ms > 0 {
        Some(Arc::new(SlackSessionizer::new(
            Duration::from_millis(config.slack_session_window_ms),
            config.slack_session_max_sessions,
            config.slack_session_max_fragments,
        )))
    } else {
        None
    };

    let outbound_task = tokio::spawn(run_outbound_loop(
        inbox.clone(),
        slack.clone(),
        blob_toolkit.clone(),
        blob_payload_cfg.clone(),
    ));

    let inbound_task = tokio::spawn(run_inbound_socket_mode(
        config.clone(),
        sender,
        slack,
        identity,
        provisioner,
        inbound,
        sessionizer,
        blob_toolkit,
        blob_payload_cfg,
    ));

    let _ = tokio::join!(inbound_task, outbound_task);
    Ok(())
}

#[derive(Clone)]
struct Config {
    slack_app_token: String,
    slack_bot_token: String,
    node_name: String,
    island_id: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    dev_mode: bool,
    ttl: u32,
    dedup_ttl_ms: u64,
    dedup_max_entries: usize,
    dst_node: Option<String>,
    identity_target: String,
    identity_timeout_ms: u64,
    slack_session_window_ms: u64,
    slack_session_max_sessions: usize,
    slack_session_max_fragments: usize,
    blob_runtime: IoBlobRuntimeConfig,
}

impl Config {
    fn from_env() -> Result<Self> {
        let env_node_name = managed_node_name("IO.slack.T123", &["NODE_NAME"]);
        let env_island_id = env("ISLAND_ID").unwrap_or_else(|| "local".to_string());
        let env_config_dir =
            PathBuf::from(env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string()));
        let explicit_spawn_config_path =
            env("NODE_CONFIG_PATH").or_else(|| env("IO_SLACK_FORCE_CONFIG_PATH"));
        let spawn_cfg = load_spawn_config(
            explicit_spawn_config_path.map(PathBuf::from),
            &env_node_name,
            &env_island_id,
        );

        if let Some(cfg) = &spawn_cfg {
            tracing::info!(path = %cfg.path.display(), "io-slack loaded spawn config");
        }

        let resolved_node_name = env("NODE_NAME")
            .or_else(|| {
                json_get_string_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["node.name", "_system.node_name"],
                )
            })
            .unwrap_or_else(|| env_node_name.clone());

        let resolved_island_id = env("ISLAND_ID")
            .or_else(|| {
                json_get_string_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["_system.hive_id", "node.island_id", "island_id"],
                )
            })
            .unwrap_or_else(|| env_island_id.clone());

        let slack_app_token = env("SLACK_APP_TOKEN")
            .or_else(|| resolve_secret(
                spawn_cfg.as_ref().map(|c| &c.doc),
                &[
                    "slack.app_token",
                    "slack_app_token",
                ],
                &[
                    "slack.app_token_ref",
                    "slack_app_token_ref",
                ],
            ))
            .ok_or_else(|| anyhow::anyhow!("missing Slack app token (set SLACK_APP_TOKEN or config slack.app_token / slack.app_token_ref=env:VAR)"))?;
        let slack_bot_token = env("SLACK_BOT_TOKEN")
            .or_else(|| resolve_secret(
                spawn_cfg.as_ref().map(|c| &c.doc),
                &[
                    "slack.bot_token",
                    "slack_bot_token",
                ],
                &[
                    "slack.bot_token_ref",
                    "slack_bot_token_ref",
                ],
            ))
            .ok_or_else(|| anyhow::anyhow!("missing Slack bot token (set SLACK_BOT_TOKEN or config slack.bot_token / slack.bot_token_ref=env:VAR)"))?;

        let blob_root = PathBuf::from(
            env("BLOB_ROOT")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_cfg.as_ref().map(|c| &c.doc),
                        &["blob.path", "io.blob.path"],
                    )
                })
                .unwrap_or_else(|| "/var/lib/fluxbee/blob".to_string()),
        );
        let max_blob_bytes = env("BLOB_MAX_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["blob.max_blob_bytes", "io.blob.max_blob_bytes"],
                )
            });
        let allowed_mimes_override = env("IO_SLACK_ALLOWED_MIMES").or_else(|| {
            json_get_string_opt(
                spawn_cfg.as_ref().map(|c| &c.doc),
                &["io.blob.allowed_mimes_csv"],
            )
        });
        let mut text_v1_cfg = IoTextBlobConfig::default();
        text_v1_cfg.max_message_bytes = env("IO_SLACK_MAX_MESSAGE_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["io.blob.max_message_bytes"],
                )
                .map(|v| v as usize)
            })
            .unwrap_or(text_v1_cfg.max_message_bytes);
        text_v1_cfg.message_overhead_bytes = env("IO_SLACK_MESSAGE_OVERHEAD_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["io.blob.message_overhead_bytes"],
                )
                .map(|v| v as usize)
            })
            .unwrap_or(text_v1_cfg.message_overhead_bytes);
        text_v1_cfg.max_attachments = env("IO_SLACK_MAX_ATTACHMENTS")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["io.blob.max_attachments"],
                )
                .map(|v| v as usize)
            })
            .unwrap_or(text_v1_cfg.max_attachments);
        text_v1_cfg.max_attachment_bytes = env("IO_SLACK_MAX_ATTACHMENT_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["io.blob.max_attachment_bytes"],
                )
            })
            .unwrap_or(text_v1_cfg.max_attachment_bytes);
        text_v1_cfg.max_total_attachment_bytes = env("IO_SLACK_MAX_TOTAL_ATTACHMENT_BYTES")
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                json_get_u64_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["io.blob.max_total_attachment_bytes"],
                )
            })
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
                .or_else(|| {
                    json_get_u64_opt(
                        spawn_cfg.as_ref().map(|c| &c.doc),
                        &["io.blob.resolve_retry.max_wait_ms"],
                    )
                })
                .unwrap_or(fluxbee_sdk::blob::BLOB_RETRY_MAX_MS),
            initial_delay_ms: env("IO_SLACK_BLOB_RETRY_INITIAL_MS")
                .and_then(|v| v.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(
                        spawn_cfg.as_ref().map(|c| &c.doc),
                        &["io.blob.resolve_retry.initial_delay_ms"],
                    )
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

        Ok(Self {
            slack_app_token,
            slack_bot_token,
            node_name: resolved_node_name,
            island_id: resolved_island_id.clone(),
            node_version: env("NODE_VERSION")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_cfg.as_ref().map(|c| &c.doc),
                        &["_system.runtime_version", "runtime.version", "node.version"],
                    )
                })
                .unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_cfg.as_ref().map(|c| &c.doc),
                            &["node.router_socket", "router_socket"],
                        )
                    })
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_cfg.as_ref().map(|c| &c.doc),
                            &["node.uuid_persistence_dir", "uuid_persistence_dir"],
                        )
                    })
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_cfg.as_ref().map(|c| &c.doc),
                            &["node.config_dir", "config_dir"],
                        )
                    })
                    .unwrap_or_else(|| env_config_dir.display().to_string()),
            ),
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
            dst_node: env("IO_SLACK_DST_NODE").or_else(|| {
                json_get_string_opt(
                    spawn_cfg.as_ref().map(|c| &c.doc),
                    &["io.dst_node", "dst_node"],
                )
            }),
            identity_target: env("IDENTITY_TARGET")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_cfg.as_ref().map(|c| &c.doc),
                        &["identity.target", "identity_target"],
                    )
                })
                .unwrap_or_else(|| format!("SY.identity@{resolved_island_id}")),
            identity_timeout_ms: env("IDENTITY_TIMEOUT_MS")
                .and_then(|v| v.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(
                        spawn_cfg.as_ref().map(|c| &c.doc),
                        &["identity.timeout_ms", "identity_timeout_ms"],
                    )
                })
                .unwrap_or(10_000),
            slack_session_window_ms: env("SLACK_SESSION_WINDOW_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            slack_session_max_sessions: env("SLACK_SESSION_MAX_SESSIONS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            slack_session_max_fragments: env("SLACK_SESSION_MAX_FRAGMENTS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
            blob_runtime,
        })
    }
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

fn load_spawn_config(
    explicit_path: Option<PathBuf>,
    node_name: &str,
    island_id: &str,
) -> Option<SpawnConfig> {
    let mut candidates = Vec::new();
    if let Some(path) = explicit_path {
        candidates.push(path);
    }

    // Canonical managed-node paths from SDK helper.
    if let Ok(path) = managed_node_config_path(node_name) {
        candidates.push(path);
    }
    if !node_name.contains('@') && !island_id.is_empty() {
        let fq_node_name = format!("{node_name}@{island_id}");
        if let Ok(path) = managed_node_config_path(&fq_node_name) {
            candidates.push(path);
        }
    }

    for path in candidates {
        if !path.exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(doc) => return Some(SpawnConfig { path, doc }),
                Err(err) => tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to parse spawn config JSON; ignoring file"
                ),
            },
            Err(err) => tracing::warn!(
                path = %path.display(),
                error = %err,
                "failed to read spawn config file; ignoring file"
            ),
        }
    }
    None
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
    slack_app_token: String,
    slack_bot_token: String,
}

impl SlackClients {
    fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::new(),
            slack_app_token: config.slack_app_token.clone(),
            slack_bot_token: config.slack_bot_token.clone(),
        })
    }

    async fn socket_mode_url(&self) -> Result<Url> {
        #[derive(Deserialize)]
        struct OpenResp {
            ok: bool,
            url: Option<String>,
            error: Option<String>,
        }

        let resp: OpenResp = self
            .http
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(&self.slack_app_token)
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

        let resp: PostResp = self
            .http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.slack_bot_token)
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
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.slack_bot_token)
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

        let length = bytes.len().to_string();
        let get_resp: GetUploadUrlResp = self
            .http
            .post("https://slack.com/api/files.getUploadURLExternal")
            .bearer_auth(&self.slack_bot_token)
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
            .bearer_auth(&self.slack_bot_token)
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
    sessionizer: Option<Arc<SlackSessionizer>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
) -> Result<()> {
    let mention_re = Regex::new(r"^<@[^>]+>\s*")?;

    loop {
        let url = slack.socket_mode_url().await?;
        tracing::info!(
            host = url.host_str().unwrap_or(""),
            path = url.path(),
            "socket mode connected"
        );

        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (mut write, mut read) = ws.split();

        while let Some(item) = read.next().await {
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

            if let Some(sessionizer) = &sessionizer {
                if !event_has_files(&event) {
                    sessionizer
                        .handle_event(
                            &config.node_name,
                            sender.clone(),
                            identity.clone(),
                            provisioner.clone(),
                            inbound.clone(),
                            &team_id,
                            &user,
                            &channel,
                            thread_ts.as_deref(),
                            &message_id,
                            content,
                            payload,
                        )
                        .await;
                    continue;
                }
            }

            let io_ctx = slack_inbound_io_context(
                &team_id,
                &user,
                &channel,
                thread_ts.as_deref(),
                &message_id,
            );
            let payload = match build_slack_inbound_payload(
                slack.as_ref(),
                blob_toolkit.as_ref(),
                &blob_payload_cfg,
                &team_id,
                &user,
                &channel,
                thread_ts.as_deref(),
                &message_id,
                &content,
                &event,
            )
            .await
            {
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

            let outcome = inbound
                .lock()
                .await
                .process_inbound(
                    identity.as_ref(),
                    Some(provisioner.as_ref()),
                    ResolveOrCreateInput {
                        channel: "slack".to_string(),
                        external_id: slack_external_id(&config.node_name, &user),
                        tenant_hint: Some(team_id.to_string()),
                        attributes: serde_json::json!({ "team_id": team_id }),
                    },
                    io_ctx,
                    payload,
                )
                .await;

            match outcome {
                InboundOutcome::SendNow(msg) => {
                    let trace_id = msg.routing.trace_id.clone();
                    let has_src_ilk = msg.meta.src_ilk.as_deref().is_some_and(|v| !v.is_empty());
                    if let Err(e) = sender.send(msg).await {
                        tracing::warn!(error=?e, %trace_id, "failed to send to router");
                    } else {
                        tracing::debug!(%trace_id, has_src_ilk, "sent to router");
                    }
                }
                InboundOutcome::DroppedDuplicate => {
                    tracing::debug!("dedup hit; dropping inbound");
                }
            }
        }
    }
}

fn event_has_files(event: &Value) -> bool {
    event
        .get("files")
        .and_then(|v| v.as_array())
        .is_some_and(|files| !files.is_empty())
}

async fn build_slack_inbound_payload(
    slack: &SlackClients,
    blob_toolkit: &fluxbee_sdk::blob::BlobToolkit,
    blob_payload_cfg: &IoTextBlobConfig,
    team_id: &str,
    user: &str,
    channel: &str,
    thread_ts: Option<&str>,
    message_id: &str,
    content: &str,
    event: &Value,
) -> Option<Value> {
    let attachments =
        collect_slack_blob_attachments(slack, blob_toolkit, blob_payload_cfg, event).await;
    if content.trim().is_empty() && attachments.is_empty() {
        return None;
    }

    match build_text_v1_inbound_payload(blob_toolkit, blob_payload_cfg, content, attachments) {
        Ok(mut payload) => {
            attach_slack_raw_stub(&mut payload, team_id, user, channel, thread_ts, message_id);
            Some(payload)
        }
        Err(err) => {
            tracing::warn!(
                code = err.canonical_code(),
                error = %err,
                "failed to build payload with attachments; retrying with text-only fallback"
            );
            if content.trim().is_empty() {
                return None;
            }
            match build_text_v1_inbound_payload(blob_toolkit, blob_payload_cfg, content, vec![]) {
                Ok(mut payload) => {
                    attach_slack_raw_stub(
                        &mut payload,
                        team_id,
                        user,
                        channel,
                        thread_ts,
                        message_id,
                    );
                    Some(payload)
                }
                Err(fallback_err) => {
                    tracing::warn!(
                        code = fallback_err.canonical_code(),
                        error = %fallback_err,
                        "failed to build text-only fallback payload"
                    );
                    None
                }
            }
        }
    }
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

#[derive(Debug)]
struct SlackSessionizer {
    window: Duration,
    max_sessions: usize,
    max_fragments: usize,
    sessions: Mutex<HashMap<String, SlackSessionState>>,
}

#[derive(Debug)]
struct SlackSessionState {
    deadline: tokio::time::Instant,
    version: u64,
    seen_message_ids: HashSet<String>,
    io_ctx: io_common::io_context::IoContext,
    identity_input: ResolveOrCreateInput,
    contents: Vec<String>,
    raws: Vec<serde_json::Value>,
}

impl SlackSessionizer {
    fn new(window: Duration, max_sessions: usize, max_fragments: usize) -> Self {
        Self {
            window,
            max_sessions: max_sessions.max(1),
            max_fragments: max_fragments.max(1),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn key(team_id: &str, user: &str, channel: &str, thread_ts: Option<&str>) -> String {
        // Suggested spec key: (channel, user, thread_ts || channel)
        let thread_or_channel = thread_ts.unwrap_or(channel);
        format!("slack:{team_id}:{channel}:{thread_or_channel}:{user}")
    }

    async fn handle_event(
        self: &Arc<Self>,
        node_name: &str,
        sender: NodeSender,
        identity: Arc<dyn IdentityResolver>,
        provisioner: Arc<dyn IdentityProvisioner>,
        inbound: Arc<Mutex<InboundProcessor>>,
        team_id: &str,
        user: &str,
        channel: &str,
        thread_ts: Option<&str>,
        message_id: &str,
        content: String,
        raw_envelope_payload: serde_json::Value,
    ) {
        let key = Self::key(team_id, user, channel, thread_ts);
        let now = tokio::time::Instant::now();
        let mut maybe_spawn = None;

        {
            let mut sessions = self.sessions.lock().await;
            if !sessions.contains_key(&key) && sessions.len() >= self.max_sessions {
                drop(sessions);
                tracing::warn!(%key, "session buffer full; sending message immediately");
                self.send_one(
                    node_name,
                    sender,
                    identity,
                    provisioner,
                    inbound,
                    team_id,
                    user,
                    channel,
                    thread_ts,
                    message_id,
                    content,
                    raw_envelope_payload,
                )
                .await;
                return;
            }

            let entry = sessions
                .entry(key.clone())
                .or_insert_with(|| SlackSessionState {
                    deadline: now + self.window,
                    version: 0,
                    seen_message_ids: HashSet::new(),
                    io_ctx: slack_inbound_io_context(team_id, user, channel, thread_ts, message_id),
                    identity_input: ResolveOrCreateInput {
                        channel: "slack".to_string(),
                        external_id: slack_external_id(node_name, user),
                        tenant_hint: Some(team_id.to_string()),
                        attributes: serde_json::json!({ "team_id": team_id }),
                    },
                    contents: Vec::new(),
                    raws: Vec::new(),
                });

            if !entry.seen_message_ids.insert(message_id.to_string()) {
                tracing::debug!(%key, %message_id, "session dedup hit; ignoring duplicate event");
                return;
            }

            if entry.contents.len() < self.max_fragments {
                entry.contents.push(content);
                entry.raws.push(raw_envelope_payload);
            } else {
                tracing::warn!(%key, max_fragments=self.max_fragments, "session max_fragments reached; dropping fragment");
            }

            entry.deadline = now + self.window;
            entry.version = entry.version.wrapping_add(1);

            // Spawn flusher only for new sessions.
            if entry.version == 1 {
                maybe_spawn = Some((key.clone(), entry.version));
            }
        }

        if let Some((key, version)) = maybe_spawn {
            tokio::spawn(self.clone().flush_task(
                key,
                version,
                sender,
                identity,
                provisioner,
                inbound,
            ));
        }
    }

    async fn flush_task(
        self: Arc<Self>,
        key: String,
        mut version: u64,
        sender: NodeSender,
        identity: Arc<dyn IdentityResolver>,
        provisioner: Arc<dyn IdentityProvisioner>,
        inbound: Arc<Mutex<InboundProcessor>>,
    ) {
        loop {
            let deadline = {
                let sessions = self.sessions.lock().await;
                let Some(state) = sessions.get(&key) else {
                    return;
                };
                if state.version != version {
                    version = state.version;
                }
                state.deadline
            };

            tokio::time::sleep_until(deadline).await;

            let state = {
                let mut sessions = self.sessions.lock().await;
                let Some(state) = sessions.get(&key) else {
                    return;
                };
                if tokio::time::Instant::now() < state.deadline || state.version != version {
                    continue;
                }
                sessions.remove(&key)
            };

            let Some(state) = state else {
                return;
            };

            let content = state.contents.join("\n");
            let payload = serde_json::json!({
              "type": "text",
              "content": content,
              "raw": { "slack_batch": state.raws },
            });

            let outcome = inbound
                .lock()
                .await
                .process_inbound(
                    identity.as_ref(),
                    Some(provisioner.as_ref()),
                    state.identity_input,
                    state.io_ctx,
                    payload,
                )
                .await;

            match outcome {
                InboundOutcome::SendNow(msg) => {
                    let trace_id = msg.routing.trace_id.clone();
                    let has_src_ilk = msg.meta.src_ilk.as_deref().is_some_and(|v| !v.is_empty());
                    if let Err(e) = sender.send(msg).await {
                        tracing::warn!(error=?e, %trace_id, "failed to send to router (session flush)");
                    } else {
                        tracing::debug!(%trace_id, has_src_ilk, "sent to router (session flush)");
                    }
                }
                InboundOutcome::DroppedDuplicate => {
                    tracing::debug!("dedup hit; dropping inbound (session flush)");
                }
            }
            return;
        }
    }

    async fn send_one(
        &self,
        node_name: &str,
        sender: NodeSender,
        identity: Arc<dyn IdentityResolver>,
        provisioner: Arc<dyn IdentityProvisioner>,
        inbound: Arc<Mutex<InboundProcessor>>,
        team_id: &str,
        user: &str,
        channel: &str,
        thread_ts: Option<&str>,
        message_id: &str,
        content: String,
        raw_envelope_payload: serde_json::Value,
    ) {
        let io_ctx = slack_inbound_io_context(team_id, user, channel, thread_ts, message_id);
        let payload = serde_json::json!({
          "type": "text",
          "content": content,
          "raw": { "slack": raw_envelope_payload },
        });

        let outcome = inbound
            .lock()
            .await
            .process_inbound(
                identity.as_ref(),
                Some(provisioner.as_ref()),
                ResolveOrCreateInput {
                    channel: "slack".to_string(),
                    external_id: slack_external_id(node_name, user),
                    tenant_hint: Some(team_id.to_string()),
                    attributes: serde_json::json!({ "team_id": team_id }),
                },
                io_ctx,
                payload,
            )
            .await;

        if let InboundOutcome::SendNow(msg) = outcome {
            let trace_id = msg.routing.trace_id.clone();
            let has_src_ilk = msg.meta.src_ilk.as_deref().is_some_and(|v| !v.is_empty());
            if let Err(e) = sender.send(msg).await {
                tracing::warn!(error=?e, %trace_id, "failed to send to router");
            } else {
                tracing::debug!(%trace_id, has_src_ilk, "sent to router");
            }
        }
    }
}

fn slack_external_id(node_name: &str, user_id: &str) -> String {
    format!("{node_name}:{user_id}")
}

async fn run_outbound_loop(
    inbox: Arc<Mutex<RouterInbox>>,
    slack: Arc<SlackClients>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
) -> Result<()> {
    loop {
        let Some(msg) = ({
            let mut guard = inbox.lock().await;
            guard.recv_next_timeout(Duration::from_secs(1)).await?
        }) else {
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

fn render_outbound_text(payload: &Value) -> String {
    if let Some(content) = payload.get("content").and_then(|v| v.as_str()) {
        if !content.trim().is_empty() {
            return content.to_string();
        }
    }

    let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if payload_type.eq_ignore_ascii_case("error") {
        let message = payload
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let code = payload.get("code").and_then(|v| v.as_str()).unwrap_or("");
        if !message.trim().is_empty() && !code.trim().is_empty() {
            return format!("Error ({code}): {message}");
        }
        if !message.trim().is_empty() {
            return format!("Error: {message}");
        }
        if !code.trim().is_empty() {
            return format!("Error: {code}");
        }
    }

    if let Some(message) = payload.get("message").and_then(|v| v.as_str()) {
        if !message.trim().is_empty() {
            return message.to_string();
        }
    }

    String::new()
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = s.chars().take(max_chars).collect::<String>();
    out.push('…');
    out
}
