#![forbid(unsafe_code)]

use anyhow::Result;
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};
use futures_util::{SinkExt, StreamExt};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::identity::{
    DisabledIdentityResolver, IdentityResolver, MockIdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::io_context::{extract_slack_post_target, slack_inbound_io_context};
use regex::Regex;
use serde::Deserialize;
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
            tracing_subscriber::EnvFilter::new(
                "info,io_slack=debug,fluxbee_sdk=info",
            )
        } else {
            tracing_subscriber::EnvFilter::new("info")
        }
    });

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        identity_mode = %config.identity_mode,
        dst_node = %config.dst_node.clone().unwrap_or_else(|| "resolve".to_string()),
        dev_mode = %config.dev_mode,
        "io-slack starting"
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
        "connected to router"
    );

    let receiver = Arc::new(Mutex::new(receiver));
    let slack = Arc::new(SlackClients::new(&config)?);
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
        receiver.clone(),
        slack.clone(),
    ));

    let inbound_task = tokio::spawn(run_inbound_socket_mode(
        config.clone(),
        sender,
        slack,
        identity,
        inbound,
        sessionizer,
    ));

    let _ = tokio::join!(inbound_task, outbound_task);
    Ok(())
}

#[derive(Clone)]
struct Config {
    slack_app_token: String,
    slack_bot_token: String,
    identity_mode: String,
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
    slack_session_window_ms: u64,
    slack_session_max_sessions: usize,
    slack_session_max_fragments: usize,
}

impl Config {
    fn from_env() -> Result<Self> {
        let slack_app_token = env_req("SLACK_APP_TOKEN")?;
        let slack_bot_token = env_req("SLACK_BOT_TOKEN")?;

        Ok(Self {
            slack_app_token,
            slack_bot_token,
            identity_mode: env("IDENTITY_MODE").unwrap_or_else(|| "mock".to_string()),
            node_name: env("NODE_NAME").unwrap_or_else(|| "IO.slack.T123".to_string()),
            island_id: env("ISLAND_ID").unwrap_or_else(|| "local".to_string()),
            node_version: env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET").unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string())),
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
            dst_node: env("IO_SLACK_DST_NODE"),
            slack_session_window_ms: env("SLACK_SESSION_WINDOW_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            slack_session_max_sessions: env("SLACK_SESSION_MAX_SESSIONS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            slack_session_max_fragments: env("SLACK_SESSION_MAX_FRAGMENTS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(8),
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

fn env_req(key: &str) -> Result<String> {
    env(key).ok_or_else(|| anyhow::anyhow!("missing env var {key}"))
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

        let url = resp.url.ok_or_else(|| anyhow::anyhow!("missing url in response"))?;
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
}

#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    envelope_id: String,
    #[serde(default)]
    payload: serde_json::Value,
}

async fn run_inbound_socket_mode(
    _config: Config,
    sender: NodeSender,
    slack: Arc<SlackClients>,
    identity: Arc<dyn IdentityResolver>,
    inbound: Arc<Mutex<InboundProcessor>>,
    sessionizer: Option<Arc<SlackSessionizer>>,
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
                sessionizer
                    .handle_event(
                        sender.clone(),
                        identity.clone(),
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

            let io_ctx = slack_inbound_io_context(
                &team_id,
                &user,
                &channel,
                thread_ts.as_deref(),
                &message_id,
            );

            let payload = serde_json::json!({
              "type": "text",
              "content": content,
              "raw": { "slack": payload },
            });

            let outcome = inbound
                .lock()
                .await
                .process_inbound(
                    identity.as_ref(),
                    ResolveOrCreateInput {
                        channel: "slack".to_string(),
                        external_id: user.to_string(),
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
                    let has_src_ilk = msg
                        .meta
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("src_ilk"))
                        .is_some_and(|v| !v.is_null());
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
        sender: NodeSender,
        identity: Arc<dyn IdentityResolver>,
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
                    sender,
                    identity,
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

            let entry = sessions.entry(key.clone()).or_insert_with(|| SlackSessionState {
                deadline: now + self.window,
                version: 0,
                seen_message_ids: HashSet::new(),
                io_ctx: slack_inbound_io_context(team_id, user, channel, thread_ts, message_id),
                identity_input: ResolveOrCreateInput {
                    channel: "slack".to_string(),
                    external_id: user.to_string(),
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
                .process_inbound(identity.as_ref(), state.identity_input, state.io_ctx, payload)
                .await;

            match outcome {
                InboundOutcome::SendNow(msg) => {
                    let trace_id = msg.routing.trace_id.clone();
                    let has_src_ilk = msg
                        .meta
                        .context
                        .as_ref()
                        .and_then(|ctx| ctx.get("src_ilk"))
                        .is_some_and(|v| !v.is_null());
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
        sender: NodeSender,
        identity: Arc<dyn IdentityResolver>,
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
                ResolveOrCreateInput {
                    channel: "slack".to_string(),
                    external_id: user.to_string(),
                    tenant_hint: Some(team_id.to_string()),
                    attributes: serde_json::json!({ "team_id": team_id }),
                },
                io_ctx,
                payload,
            )
            .await;

        if let InboundOutcome::SendNow(msg) = outcome {
            let trace_id = msg.routing.trace_id.clone();
            let has_src_ilk = msg
                .meta
                .context
                .as_ref()
                .and_then(|ctx| ctx.get("src_ilk"))
                .is_some_and(|v| !v.is_null());
            if let Err(e) = sender.send(msg).await {
                tracing::warn!(error=?e, %trace_id, "failed to send to router");
            } else {
                tracing::debug!(%trace_id, has_src_ilk, "sent to router");
            }
        }
    }
}

async fn run_outbound_loop(receiver: Arc<Mutex<NodeReceiver>>, slack: Arc<SlackClients>) -> Result<()> {
    loop {
        let msg = {
            let mut guard = receiver.lock().await;
            guard.recv().await?
        };

        tracing::debug!(
            trace_id = %msg.routing.trace_id,
            payload_type = %msg.payload.get("type").and_then(|v| v.as_str()).unwrap_or(""),
            "outbound received from router"
        );

        let Some(meta_context) = msg.meta.context.as_ref() else {
            continue;
        };
        let Some(target) = extract_slack_post_target(meta_context) else {
            continue;
        };

        let text = msg
            .payload
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let blocks = msg.payload.get("blocks").cloned();

        tracing::debug!(
            slack_channel = target.channel_id,
            slack_thread_ts = target.thread_ts.as_deref().unwrap_or(""),
            text_len = text.len(),
            text_preview = %truncate(text, 120),
            "sending slack message"
        );

        if let Err(e) = slack
            .post_message(&target.channel_id, text, target.thread_ts.as_deref(), blocks)
            .await
        {
            tracing::warn!(error=?e, "failed to send slack message");
        } else {
            tracing::debug!("slack message sent");
        }
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
