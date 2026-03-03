use std::fs;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    build_reply_message, build_text_response, extract_text, Agent, AiNode, AiNodeConfig,
    Message, ModelSettings, NodeRuntime, OpenAiResponsesClient, RetryPolicy, RouterClient,
    RuntimeConfig,
};
use serde::Deserialize;
use tokio::task::JoinSet;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Deserialize)]
struct RunnerConfig {
    node: NodeSection,
    #[serde(default)]
    runtime: RuntimeSection,
    behavior: BehaviorSection,
}

#[derive(Debug, Deserialize)]
struct NodeSection {
    name: String,
    #[serde(default = "default_version")]
    version: String,
    #[serde(default = "default_router_socket")]
    router_socket: String,
    #[serde(default = "default_state_dir")]
    uuid_persistence_dir: String,
    #[serde(default = "default_config_dir")]
    config_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RuntimeSection {
    read_timeout_ms: u64,
    handler_timeout_ms: u64,
    write_timeout_ms: u64,
    queue_capacity: usize,
    worker_pool_size: usize,
    retry_max_attempts: usize,
    retry_initial_backoff_ms: u64,
    retry_max_backoff_ms: u64,
    metrics_log_interval_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BehaviorSection {
    Echo,
    OpenaiChat(OpenAiChatSection),
}

#[derive(Debug, Deserialize)]
struct OpenAiChatSection {
    #[serde(default = "default_model")]
    model: String,
    #[serde(default)]
    instructions: Option<InstructionsSourceConfig>,
    #[serde(default)]
    model_settings: Option<RunnerModelSettings>,
    #[serde(default = "default_openai_api_key_env")]
    api_key_env: String,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RunnerModelSettings {
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum InstructionsSourceConfig {
    // Backward-compatible short form:
    // instructions: "You are concise"
    Inline(String),
    // Structured strategy:
    // instructions:
    //   source: file|env|inline|none
    //   value: /path/file.txt | ENV_VAR | inline text
    //   trim: true|false (default true)
    Strategy(InstructionsStrategy),
}

#[derive(Debug, Deserialize)]
struct InstructionsStrategy {
    source: InstructionsSourceKind,
    #[serde(default)]
    value: Option<String>,
    #[serde(default = "default_trim_true")]
    trim: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InstructionsSourceKind {
    Inline,
    File,
    Env,
    None,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            read_timeout_ms: 30_000,
            handler_timeout_ms: 60_000,
            write_timeout_ms: 10_000,
            queue_capacity: 128,
            worker_pool_size: 4,
            retry_max_attempts: 3,
            retry_initial_backoff_ms: 200,
            retry_max_backoff_ms: 2_000,
            metrics_log_interval_ms: 30_000,
        }
    }
}

fn default_version() -> String {
    "0.1.0".to_string()
}

fn default_router_socket() -> String {
    "/var/run/fluxbee/routers".to_string()
}

fn default_state_dir() -> String {
    "/var/lib/fluxbee/state/nodes".to_string()
}

fn default_config_dir() -> String {
    "/etc/fluxbee".to_string()
}

fn default_model() -> String {
    "gpt-4.1-mini".to_string()
}

fn default_openai_api_key_env() -> String {
    "OPENAI_API_KEY".to_string()
}

fn default_trim_true() -> bool {
    true
}

enum NodeBehavior {
    Echo,
    OpenAiChat { agent: Agent },
}

struct GenericAiNode {
    behavior: NodeBehavior,
}

#[async_trait]
impl AiNode for GenericAiNode {
    async fn on_message(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        if msg.meta.msg_type.eq_ignore_ascii_case("system") {
            return Ok(None);
        }

        let input = extract_text(&msg.payload).unwrap_or_default();
        let output = match &self.behavior {
            NodeBehavior::Echo => format!("Echo: {input}"),
            NodeBehavior::OpenAiChat { agent } => agent.run_text(input).await?.content,
        };

        let payload = build_text_response(output)?;
        Ok(Some(build_reply_message(&msg, "pending-src", payload)))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config_paths = parse_config_paths()?;
    let mut loaded = Vec::with_capacity(config_paths.len());
    for path in &config_paths {
        let raw = fs::read_to_string(path)?;
        let cfg: RunnerConfig = serde_yaml::from_str(&raw)?;
        loaded.push((path.clone(), cfg));
    }

    ensure_unique_node_names(&loaded)?;

    let mut runners = JoinSet::new();
    for (config_path, cfg) in loaded {
        runners.spawn(async move { run_one_config(config_path, cfg).await });
    }

    while let Some(result) = runners.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(err),
            Err(err) => return Err(format!("runner task join error: {err}").into()),
        }
    }
    Ok(())
}

async fn run_one_config(
    config_path: PathBuf,
    cfg: RunnerConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let behavior = build_behavior(&cfg)?;
    let ai_node_config = AiNodeConfig {
        name: cfg.node.name,
        version: cfg.node.version,
        router_socket: PathBuf::from(cfg.node.router_socket),
        uuid_persistence_dir: PathBuf::from(cfg.node.uuid_persistence_dir),
        config_dir: PathBuf::from(cfg.node.config_dir),
    };

    let runtime_config = RuntimeConfig {
        read_timeout: Duration::from_millis(cfg.runtime.read_timeout_ms),
        handler_timeout: Duration::from_millis(cfg.runtime.handler_timeout_ms),
        write_timeout: Duration::from_millis(cfg.runtime.write_timeout_ms),
        queue_capacity: cfg.runtime.queue_capacity,
        worker_pool_size: cfg.runtime.worker_pool_size,
        retry_policy: RetryPolicy {
            max_attempts: cfg.runtime.retry_max_attempts,
            initial_backoff: Duration::from_millis(cfg.runtime.retry_initial_backoff_ms),
            max_backoff: Duration::from_millis(cfg.runtime.retry_max_backoff_ms),
        },
        metrics_log_interval: Duration::from_millis(cfg.runtime.metrics_log_interval_ms),
    };

    tracing::info!(
        config = %config_path.display(),
        node_name = %ai_node_config.name,
        "starting ai_node_runner node instance"
    );

    let client = RouterClient::connect(ai_node_config).await?;
    let runtime = NodeRuntime::new(client, GenericAiNode { behavior });
    runtime.run_with_config(runtime_config).await?;
    Ok(())
}

fn parse_config_paths() -> Result<Vec<PathBuf>, Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut configs = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        if args[i] == "--config" {
            let Some(path) = args.get(i + 1) else {
                return Err("missing path after --config".to_string().into());
            };
            configs.push(PathBuf::from(path));
            i += 2;
            continue;
        }
        return Err(format!("unknown argument: {}", args[i]).into());
    }

    if configs.is_empty() {
        return Err(
            "usage: ai_node_runner --config <path-1.yaml> [--config <path-2.yaml> ...]"
                .to_string()
                .into(),
        );
    }

    Ok(configs)
}

fn ensure_unique_node_names(
    configs: &[(PathBuf, RunnerConfig)],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut names = HashSet::new();
    for (path, cfg) in configs {
        if !names.insert(cfg.node.name.clone()) {
            return Err(format!(
                "duplicate node name '{}' found in config {}",
                cfg.node.name,
                path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn build_behavior(
    cfg: &RunnerConfig,
) -> Result<NodeBehavior, Box<dyn std::error::Error + Send + Sync>> {
    let behavior = match &cfg.behavior {
        BehaviorSection::Echo => NodeBehavior::Echo,
        BehaviorSection::OpenaiChat(openai) => {
            let api_key = std::env::var(&openai.api_key_env).map_err(|_| {
                format!(
                    "missing required env var for OpenAI api key: {}",
                    openai.api_key_env
                )
            })?;
            let mut client = OpenAiResponsesClient::new(api_key);
            if let Some(base_url) = &openai.base_url {
                client = client.with_base_url(base_url.clone());
            }
            let instructions = resolve_instructions(&openai.instructions)?;
            let model_settings = openai
                .model_settings
                .as_ref()
                .map(|v| ModelSettings {
                    temperature: v.temperature,
                    top_p: v.top_p,
                    max_output_tokens: v.max_output_tokens,
                })
                .unwrap_or_default();
            let agent = Agent::with_model_settings(
                "ai_node_runner",
                openai.model.clone(),
                instructions,
                model_settings,
                Arc::new(client),
            );
            NodeBehavior::OpenAiChat { agent }
        }
    };
    Ok(behavior)
}

fn resolve_instructions(
    cfg: &Option<InstructionsSourceConfig>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };

    match cfg {
        InstructionsSourceConfig::Inline(value) => Ok(Some(value.clone())),
        InstructionsSourceConfig::Strategy(strategy) => match strategy.source {
            InstructionsSourceKind::Inline => {
                let Some(value) = strategy.value.clone() else {
                    return Err("instructions.source=inline requires instructions.value".into());
                };
                Ok(Some(maybe_trim(value, strategy.trim)))
            }
            InstructionsSourceKind::File => {
                let Some(path) = strategy.value.clone() else {
                    return Err("instructions.source=file requires instructions.value (path)".into());
                };
                let content = fs::read_to_string(path)?;
                Ok(Some(maybe_trim(content, strategy.trim)))
            }
            InstructionsSourceKind::Env => {
                let Some(env_name) = strategy.value.clone() else {
                    return Err("instructions.source=env requires instructions.value (env var)".into());
                };
                let value = std::env::var(&env_name).map_err(|_| {
                    format!(
                        "missing env var for instructions source env: {}",
                        env_name
                    )
                })?;
                Ok(Some(maybe_trim(value, strategy.trim)))
            }
            InstructionsSourceKind::None => Ok(None),
        },
    }
}

fn maybe_trim(value: String, trim: bool) -> String {
    if trim {
        value.trim().to_string()
    } else {
        value
    }
}
