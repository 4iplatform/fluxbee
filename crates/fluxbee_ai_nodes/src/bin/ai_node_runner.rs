use std::fs;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    build_reply_message_runtime_src, build_text_response, extract_text, AiNode, AiNodeConfig, Message,
    ModelSettings, NodeRuntime, OpenAiResponsesClient, RetryPolicy, RouterClient, RuntimeConfig,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;
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
    #[serde(default = "default_dynamic_config_dir")]
    dynamic_config_dir: String,
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
    api_key: Option<String>,
    #[serde(default)]
    openai: Option<OpenAiCredentialsSection>,
    #[serde(default)]
    base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiCredentialsSection {
    #[serde(default)]
    api_key: Option<String>,
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

fn default_dynamic_config_dir() -> String {
    "/var/lib/fluxbee/state/ai-nodes".to_string()
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
    OpenAiChat(OpenAiChatRuntime),
}

#[derive(Debug, Clone)]
struct OpenAiChatRuntime {
    model: String,
    instructions: Option<String>,
    model_settings: ModelSettings,
    api_key_env: String,
    yaml_inline_api_key: Option<String>,
    base_url: Option<String>,
}

struct GenericAiNode {
    node_name: String,
    behavior: NodeBehavior,
    dynamic_config_dir: PathBuf,
    control_plane: Arc<RwLock<ControlPlaneState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeLifecycleState {
    Unconfigured,
    Configured,
    FailedConfig,
}

impl NodeLifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "UNCONFIGURED",
            Self::Configured => "CONFIGURED",
            Self::FailedConfig => "FAILED_CONFIG",
        }
    }
}

#[derive(Debug)]
struct ControlPlaneState {
    current_state: NodeLifecycleState,
    config_source: &'static str,
    effective_config: Option<Value>,
    schema_version: u32,
    config_version: u64,
}

impl Default for ControlPlaneState {
    fn default() -> Self {
        Self {
            current_state: NodeLifecycleState::Unconfigured,
            config_source: "none",
            effective_config: None,
            schema_version: 0,
            config_version: 0,
        }
    }
}

#[async_trait]
impl AiNode for GenericAiNode {
    async fn on_message(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        if is_control_plane(&msg) {
            return self.handle_control_plane(msg).await;
        }
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            let state = self.control_plane.read().await.current_state;
            if state != NodeLifecycleState::Configured {
                let payload = node_not_configured_payload(state);
                return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
            }
        }

        let input = extract_text(&msg.payload).unwrap_or_default();
        let output = match &self.behavior {
            NodeBehavior::Echo => format!("Echo: {input}"),
            NodeBehavior::OpenAiChat(openai) => self.run_openai_chat(openai, input).await?,
        };

        let payload = build_text_response(output)?;
        Ok(Some(build_reply_message_runtime_src(&msg, payload)))
    }
}

impl GenericAiNode {
    async fn run_openai_chat(
        &self,
        openai: &OpenAiChatRuntime,
        input: String,
    ) -> fluxbee_ai_sdk::Result<String> {
        let api_key = self.resolve_openai_api_key(openai).await.ok_or_else(|| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(
                "missing OpenAI api key (CONFIG_SET override, YAML inline, or env)".to_string(),
            )
        })?;
        let mut client = OpenAiResponsesClient::new(api_key);
        if let Some(base_url) = &openai.base_url {
            client = client.with_base_url(base_url.clone());
        }
        let req = fluxbee_ai_sdk::llm::LlmRequest {
            model: openai.model.clone(),
            system: openai.instructions.clone(),
            input,
            max_output_tokens: None,
            model_settings: Some(openai.model_settings.clone()),
        };
        let response = fluxbee_ai_sdk::llm::LlmClient::generate(&client, req).await?;
        Ok(response.content)
    }

    async fn resolve_openai_api_key(&self, openai: &OpenAiChatRuntime) -> Option<String> {
        let from_control_plane = {
            let state = self.control_plane.read().await;
            state
                .effective_config
                .as_ref()
                .and_then(extract_openai_api_key_from_config)
        };
        if from_control_plane.is_some() {
            return from_control_plane;
        }
        if openai.yaml_inline_api_key.is_some() {
            return openai.yaml_inline_api_key.clone();
        }
        std::env::var(&openai.api_key_env).ok()
    }

    async fn handle_control_plane(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        let Some(command) = msg.meta.msg.as_deref() else {
            return Ok(None);
        };
        let response_payload = if command.eq_ignore_ascii_case("CONFIG_SET") {
            self.apply_config_set(&msg).await
        } else if command.eq_ignore_ascii_case("CONFIG_GET") {
            self.build_config_get_response().await
        } else {
            return Ok(None);
        };
        Ok(Some(build_control_plane_response(
            &msg,
            "CONFIG_RESPONSE",
            response_payload,
        )))
    }

    async fn apply_config_set(&self, msg: &Message) -> Value {
        let subsystem = match msg.payload.get("subsystem").and_then(Value::as_str) {
            Some(value) if value == "ai_node" => value,
            Some(value) => {
                return self.invalid_config_response(None, None, format!(
                    "Invalid payload.subsystem: expected 'ai_node', got '{value}'"
                ));
            }
            None => {
                return self.invalid_config_response(
                    None,
                    None,
                    "Missing required field: payload.subsystem".to_string(),
                );
            }
        };

        let requested_node_name = match msg.payload.get("node_name").and_then(Value::as_str) {
            Some(value) => value,
            None => {
                return self.invalid_config_response(
                    None,
                    None,
                    "Missing required field: payload.node_name".to_string(),
                );
            }
        };
        if !self.node_name_matches(requested_node_name) {
            return self.invalid_config_response(None, None, format!(
                "Invalid payload.node_name: expected '{}', got '{}'",
                self.node_name, requested_node_name
            ));
        }

        let schema_version = match msg.payload.get("schema_version").and_then(Value::as_u64) {
            Some(raw) => match u32::try_from(raw) {
                Ok(value) => value,
                Err(_) => {
                    return self.invalid_config_response(
                        None,
                        None,
                        "Invalid payload.schema_version: must fit u32".to_string(),
                    );
                }
            },
            None => {
                return self.invalid_config_response(
                    None,
                    None,
                    "Missing required field: payload.schema_version".to_string(),
                );
            }
        };

        let config_version = match msg.payload.get("config_version").and_then(Value::as_u64) {
            Some(value) => value,
            None => {
                return self.invalid_config_response(
                    Some(schema_version),
                    None,
                    "Missing required field: payload.config_version".to_string(),
                );
            }
        };

        let config = match msg.payload.get("config") {
            Some(Value::Object(_)) => msg.payload.get("config").cloned().unwrap_or(Value::Null),
            Some(_) => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    "Invalid payload.config: must be an object".to_string(),
                );
            }
            None => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    "Missing required field: payload.config".to_string(),
                );
            }
        };

        let mut state = self.control_plane.write().await;
        if config_version < state.config_version {
            return self.error_response(
                "stale_config_version",
                format!(
                    "Stale config_version: received {}, current {}",
                    config_version, state.config_version
                ),
                state.schema_version,
                state.config_version,
                state.current_state.as_str(),
            );
        }
        if config_version == state.config_version && state.effective_config.is_some() {
            return self.ok_response(
                subsystem,
                state.schema_version,
                state.config_version,
                state.current_state.as_str(),
                state.effective_config.as_ref(),
            );
        }

        let prev_state = state.current_state;
        let prev_source = state.config_source;
        let prev_effective = state.effective_config.clone();
        let prev_schema = state.schema_version;
        let prev_version = state.config_version;

        state.current_state = NodeLifecycleState::Configured;
        state.config_source = "persisted";
        state.effective_config = Some(config);
        state.schema_version = schema_version;
        state.config_version = config_version;
        if let Err(err) = persist_dynamic_config(
            &self.dynamic_config_dir,
            &self.node_name,
            state.schema_version,
            state.config_version,
            state.effective_config.as_ref().unwrap_or(&Value::Null),
        ) {
            state.current_state = prev_state;
            state.config_source = prev_source;
            state.effective_config = prev_effective;
            state.schema_version = prev_schema;
            state.config_version = prev_version;
            return self.error_response(
                "config_persist_error",
                format!("Failed to persist dynamic config: {err}"),
                prev_schema,
                prev_version,
                prev_state.as_str(),
            );
        }

        self.ok_response(
            subsystem,
            state.schema_version,
            state.config_version,
            state.current_state.as_str(),
            state.effective_config.as_ref(),
        )
    }

    fn invalid_config_response(
        &self,
        schema_version: Option<u32>,
        config_version: Option<u64>,
        message: String,
    ) -> Value {
        self.error_response(
            "invalid_config",
            message,
            schema_version.unwrap_or(1),
            config_version.unwrap_or(0),
            NodeLifecycleState::Unconfigured.as_str(),
        )
    }

    fn ok_response(
        &self,
        subsystem: &str,
        schema_version: u32,
        config_version: u64,
        state: &str,
        effective_config: Option<&Value>,
    ) -> Value {
        json!({
            "subsystem": subsystem,
            "node_name": self.node_name.as_str(),
            "ok": true,
            "status": "ok",
            "state": state,
            "schema_version": schema_version,
            "config_version": config_version,
            "effective_config": effective_config.map(redact_secrets),
        })
    }

    fn error_response(
        &self,
        code: &str,
        message: String,
        schema_version: u32,
        config_version: u64,
        state: &str,
    ) -> Value {
        json!({
            "subsystem": "ai_node",
            "node_name": self.node_name.as_str(),
            "ok": false,
            "status": "error",
            "state": state,
            "schema_version": schema_version,
            "config_version": config_version,
            "error": {
                "code": code,
                "message": message
            },
            "error_code": code,
            "error_message": message
        })
    }

    fn node_name_matches(&self, requested: &str) -> bool {
        if requested == self.node_name {
            return true;
        }
        let with_hive_prefix = format!("{}@", self.node_name);
        requested.starts_with(&with_hive_prefix)
    }

    async fn build_config_get_response(&self) -> Value {
        let state = self.control_plane.read().await;
        let (ok, config_source) = if state.effective_config.is_some() {
            (true, state.config_source)
        } else {
            (false, "none")
        };
        json!({
            "subsystem": "ai_node",
            "node_name": self.node_name.as_str(),
            "ok": ok,
            "status": if ok { "ok" } else { "error" },
            "state": state.current_state.as_str(),
            "config_source": config_source,
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "config": state.effective_config.as_ref().map(redact_secrets),
            "effective_config": state.effective_config.as_ref().map(redact_secrets),
            "error": if ok { Value::Null } else { json!({"code":"node_not_configured","message":"No effective config available"}) },
            "error_code": if ok { Value::Null } else { Value::String("node_not_configured".to_string()) },
            "error_message": if ok { Value::Null } else { Value::String("No effective config available".to_string()) },
        })
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
    let startup_effective_config = build_startup_effective_config(&cfg);
    let persisted_dynamic = load_persisted_dynamic_config(
        &PathBuf::from(&cfg.node.dynamic_config_dir),
        &cfg.node.name,
    );
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

    let node_name = ai_node_config.name.clone();
    let client = RouterClient::connect(ai_node_config).await?;
    let runtime = NodeRuntime::new(
        client,
        GenericAiNode {
            node_name,
            behavior,
            dynamic_config_dir: PathBuf::from(cfg.node.dynamic_config_dir),
            control_plane: Arc::new(RwLock::new(ControlPlaneState {
                current_state: NodeLifecycleState::Configured,
                config_source: "yaml",
                effective_config: Some(startup_effective_config),
                schema_version: persisted_dynamic
                    .as_ref()
                    .map(|v| v.schema_version)
                    .unwrap_or(1),
                config_version: persisted_dynamic
                    .as_ref()
                    .map(|v| v.config_version)
                    .unwrap_or(1),
                ..ControlPlaneState::default()
            })),
        },
    );
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
            let yaml_inline_api_key = openai
                .openai
                .as_ref()
                .and_then(|v| v.api_key.clone())
                .or_else(|| openai.api_key.clone());
            NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                model: openai.model.clone(),
                instructions,
                model_settings,
                api_key_env: openai.api_key_env.clone(),
                yaml_inline_api_key,
                base_url: openai.base_url.clone(),
            })
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

fn build_startup_effective_config(cfg: &RunnerConfig) -> Value {
    let behavior_value = match &cfg.behavior {
        BehaviorSection::Echo => json!({ "kind": "echo" }),
        BehaviorSection::OpenaiChat(openai) => json!({
            "kind": "openai_chat",
            "model": openai.model,
            "base_url": openai.base_url,
            "instructions": format_instructions_snapshot(&openai.instructions),
            "model_settings": openai.model_settings.as_ref().map(|v| {
                json!({
                    "temperature": v.temperature,
                    "top_p": v.top_p,
                    "max_output_tokens": v.max_output_tokens
                })
            }),
            "api_key_env": openai.api_key_env
        }),
    };

    json!({
        "node": {
            "name": cfg.node.name,
            "version": cfg.node.version,
            "router_socket": cfg.node.router_socket,
            "uuid_persistence_dir": cfg.node.uuid_persistence_dir,
            "config_dir": cfg.node.config_dir
            ,"dynamic_config_dir": cfg.node.dynamic_config_dir
        },
        "runtime": {
            "read_timeout_ms": cfg.runtime.read_timeout_ms,
            "handler_timeout_ms": cfg.runtime.handler_timeout_ms,
            "write_timeout_ms": cfg.runtime.write_timeout_ms,
            "queue_capacity": cfg.runtime.queue_capacity,
            "worker_pool_size": cfg.runtime.worker_pool_size,
            "retry_max_attempts": cfg.runtime.retry_max_attempts,
            "retry_initial_backoff_ms": cfg.runtime.retry_initial_backoff_ms,
            "retry_max_backoff_ms": cfg.runtime.retry_max_backoff_ms,
            "metrics_log_interval_ms": cfg.runtime.metrics_log_interval_ms
        },
        "behavior": behavior_value
    })
}

#[derive(Debug, Deserialize, serde::Serialize)]
struct PersistedDynamicConfig {
    schema_version: u32,
    config_version: u64,
    node_name: String,
    config: Value,
    updated_at: String,
}

fn dynamic_config_path(base_dir: &std::path::Path, node_name: &str) -> PathBuf {
    let safe_name = node_name.replace(['/', '\\'], "_");
    base_dir.join(format!("{safe_name}.json"))
}

fn load_persisted_dynamic_config(
    base_dir: &std::path::Path,
    node_name: &str,
) -> Option<PersistedDynamicConfig> {
    let path = dynamic_config_path(base_dir, node_name);
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<PersistedDynamicConfig>(&raw).ok()
}

fn persist_dynamic_config(
    base_dir: &std::path::Path,
    node_name: &str,
    schema_version: u32,
    config_version: u64,
    config: &Value,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    fs::create_dir_all(base_dir)?;
    let path = dynamic_config_path(base_dir, node_name);
    let payload = PersistedDynamicConfig {
        schema_version,
        config_version,
        node_name: node_name.to_string(),
        config: redact_secrets(config),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&payload)?;
    fs::write(path, json)?;
    Ok(())
}

fn format_instructions_snapshot(cfg: &Option<InstructionsSourceConfig>) -> Value {
    match cfg {
        None => Value::Null,
        Some(InstructionsSourceConfig::Inline(value)) => {
            json!({ "source": "inline", "value": value, "trim": true })
        }
        Some(InstructionsSourceConfig::Strategy(strategy)) => json!({
            "source": match strategy.source {
                InstructionsSourceKind::Inline => "inline",
                InstructionsSourceKind::File => "file",
                InstructionsSourceKind::Env => "env",
                InstructionsSourceKind::None => "none"
            },
            "value": strategy.value,
            "trim": strategy.trim
        }),
    }
}

fn extract_openai_api_key_from_config(config: &Value) -> Option<String> {
    config
        .get("behavior")
        .and_then(|v| v.get("openai"))
        .and_then(|v| v.get("api_key"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            config
                .get("behavior")
                .and_then(|v| v.get("api_key"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

fn is_control_plane(msg: &Message) -> bool {
    msg.meta.msg_type.eq_ignore_ascii_case("system")
        || msg.meta.msg_type.eq_ignore_ascii_case("admin")
}

fn build_control_plane_response(msg: &Message, response_msg: &str, payload: Value) -> Message {
    let mut response = build_reply_message_runtime_src(msg, payload);
    response.meta.msg = Some(response_msg.to_string());
    response
}

fn redact_secrets(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut output = serde_json::Map::new();
            for (k, v) in map {
                if k.eq_ignore_ascii_case("api_key") {
                    output.insert(k.clone(), Value::String("***REDACTED***".to_string()));
                } else {
                    output.insert(k.clone(), redact_secrets(v));
                }
            }
            Value::Object(output)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_secrets).collect()),
        _ => value.clone(),
    }
}

fn node_not_configured_payload(state: NodeLifecycleState) -> Value {
    json!({
        "type": "error",
        "code": "node_not_configured",
        "message": "AI node is not configured yet. Retry later.",
        "retryable": true,
        "details": {
            "state": state.as_str()
        }
    })
}
