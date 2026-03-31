use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use fluxbee_sdk::nats::resolve_local_nats_endpoint;
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::{
    build_node_config_response_message, build_node_secret_record, connect, load_node_secret_record,
    managed_node_config_path, managed_node_instance_dir, managed_node_name,
    save_node_secret_record, try_handle_default_node_status, NodeConfig, NodeReceiver,
    NodeSecretDescriptor, NodeSecretWriteOptions, NodeSender, NodeUuidMode,
    NODE_CONFIG_APPLY_MODE_REPLACE, NODE_SECRET_REDACTION_TOKEN,
};
use json_router::nats::{NatsSubscriber as RouterNatsSubscriber, SUBJECT_STORAGE_TURNS};

type CognitionError = Box<dyn std::error::Error + Send + Sync>;

const COGNITION_NODE_BASE_NAME: &str = "SY.cognition";
const COGNITION_NODE_VERSION: &str = "2.0";
const COGNITION_CONFIG_SCHEMA_VERSION: u32 = 1;
const COGNITION_LOCAL_SECRET_KEY_OPENAI: &str = "openai_api_key";
const COGNITION_TURNS_SID: u32 = 27;
const DURABLE_QUEUE_TURNS: &str = "durable.sy-cognition.turns";
const COGNITION_DEFAULT_CONTEXT_OPEN_THRESHOLD: f64 = 0.5;
const COGNITION_DEFAULT_REASON_OPEN_THRESHOLD: f64 = 0.5;
const NATS_ERROR_LOG_EVERY: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CognitionAiSecretSource {
    LocalFile,
    EnvCompat,
    Missing,
}

impl CognitionAiSecretSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local_file",
            Self::EnvCompat => "env_compat",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CognitionThresholds {
    context_open: f64,
    reason_open: f64,
}

impl Default for CognitionThresholds {
    fn default() -> Self {
        Self {
            context_open: COGNITION_DEFAULT_CONTEXT_OPEN_THRESHOLD,
            reason_open: COGNITION_DEFAULT_REASON_OPEN_THRESHOLD,
        }
    }
}

#[derive(Debug, Clone)]
struct CognitionControlState {
    schema_version: u32,
    config_version: u64,
    ai_secret_source: CognitionAiSecretSource,
    thresholds: CognitionThresholds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CognitionConfigStateFile {
    schema_version: u32,
    config_version: u64,
    node_name: String,
    config: Value,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct RuntimePaths {
    state_dir: PathBuf,
    shm_dir: PathBuf,
    cache_dir: PathBuf,
    memory_lance_path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize)]
struct CognitionRuntimeState {
    processed_turns_total: u64,
    invalid_turns_total: u64,
    last_trace_id: Option<String>,
    last_thread_id: Option<String>,
    last_thread_seq: Option<u64>,
    last_src_ilk: Option<String>,
    last_ich: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    #[serde(default)]
    nats: Option<NatsSection>,
}

#[derive(Debug, Deserialize)]
struct NatsSection {
    #[serde(default)]
    mode: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), CognitionError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_cognition supports only Linux targets.");
        std::process::exit(1);
    }

    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let hive = load_hive(&config_dir)?;
    let endpoint = resolve_local_nats_endpoint(&config_dir)?;
    let use_durable_consumer = hive
        .nats
        .as_ref()
        .and_then(|n| n.mode.as_deref())
        .map(|mode| mode.trim().eq_ignore_ascii_case("embedded"))
        .unwrap_or(true);
    let node_base_name = managed_node_name(COGNITION_NODE_BASE_NAME, &["SY_COGNITION_NODE_NAME"]);
    let node_name = ensure_l2_name(&node_base_name, &hive.hive_id);
    let runtime_paths = ensure_runtime_paths(&node_name)?;
    let ai_secret_source = resolve_openai_api_key_source(&node_name);
    let mut control_state = bootstrap_cognition_control_state(&node_name, ai_secret_source)?;
    let runtime_state = Arc::new(Mutex::new(CognitionRuntimeState::default()));
    let nats_subscribe_errors = Arc::new(AtomicU64::new(0));

    let node_config = NodeConfig {
        name: node_base_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: COGNITION_NODE_VERSION.to_string(),
    };
    let (mut sender, mut receiver) =
        connect_with_retry(&node_config, Duration::from_secs(1)).await?;
    tracing::info!(node_name = %sender.full_name(), "sy.cognition connected to router");

    std::mem::drop(tokio::spawn(run_turns_loop(
        endpoint.clone(),
        use_durable_consumer,
        Arc::clone(&runtime_state),
        Arc::clone(&nats_subscribe_errors),
    )));

    tracing::info!(
        hive = %hive.hive_id,
        node_name = %node_name,
        endpoint = %endpoint,
        durable_turns = use_durable_consumer,
        ai_secret_source = %control_state.ai_secret_source.as_str(),
        state_dir = %runtime_paths.state_dir.display(),
        "sy.cognition started"
    );

    let mut heartbeat = time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                tracing::debug!(
                    node_name = %sender.full_name(),
                    config_version = control_state.config_version,
                    ai_secret_source = %control_state.ai_secret_source.as_str(),
                    "sy.cognition heartbeat"
                );
            }
            received = receiver.recv() => {
                let msg = match received {
                    Ok(msg) => msg,
                    Err(err) => {
                        tracing::warn!(error = %err, "sy.cognition recv error; reconnecting");
                        let (new_sender, new_receiver) =
                            connect_with_retry(&node_config, Duration::from_secs(1)).await?;
                        sender = new_sender;
                        receiver = new_receiver;
                        tracing::info!(node_name = %sender.full_name(), "sy.cognition reconnected to router");
                        continue;
                    }
                };
                if let Err(err) = process_router_message(
                    &sender,
                    &msg,
                    &mut control_state,
                    Arc::clone(&runtime_state),
                    Arc::clone(&nats_subscribe_errors),
                    &node_name,
                    &runtime_paths,
                    use_durable_consumer,
                ).await {
                    tracing::warn!(error = %err, action = ?msg.meta.msg, "failed to process sy.cognition system message");
                }
            }
        }
    }
}

async fn run_turns_loop(
    endpoint: String,
    use_durable_consumer: bool,
    runtime_state: Arc<Mutex<CognitionRuntimeState>>,
    nats_subscribe_errors: Arc<AtomicU64>,
) {
    loop {
        let subscriber = if use_durable_consumer {
            RouterNatsSubscriber::new(
                endpoint.clone(),
                SUBJECT_STORAGE_TURNS.to_string(),
                COGNITION_TURNS_SID,
            )
            .with_queue(DURABLE_QUEUE_TURNS)
        } else {
            RouterNatsSubscriber::new(
                endpoint.clone(),
                SUBJECT_STORAGE_TURNS.to_string(),
                COGNITION_TURNS_SID,
            )
        };
        let runtime_state = Arc::clone(&runtime_state);
        let run_result = subscriber
            .run(move |payload| {
                let runtime_state = Arc::clone(&runtime_state);
                async move { handle_turn_payload(payload, runtime_state).await }
            })
            .await;
        if let Err(err) = run_result {
            let count = nats_subscribe_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if count == 1 || count % NATS_ERROR_LOG_EVERY == 0 {
                tracing::warn!(
                    subject = SUBJECT_STORAGE_TURNS,
                    error = %err,
                    failures = count,
                    "sy.cognition turns subscribe loop failed; retrying"
                );
            }
        }
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handle_turn_payload(
    payload: Vec<u8>,
    runtime_state: Arc<Mutex<CognitionRuntimeState>>,
) -> Result<(), std::io::Error> {
    let msg: Message = match serde_json::from_slice(&payload) {
        Ok(msg) => msg,
        Err(err) => {
            let mut state = runtime_state.lock().await;
            state.invalid_turns_total = state.invalid_turns_total.saturating_add(1);
            tracing::warn!(
                error = %err,
                payload_bytes = payload.len(),
                "sy.cognition received invalid storage.turns payload"
            );
            return Ok(());
        }
    };

    let mut state = runtime_state.lock().await;
    state.processed_turns_total = state.processed_turns_total.saturating_add(1);
    state.last_trace_id = Some(msg.routing.trace_id.clone());
    state.last_thread_id = msg
        .meta
        .thread_id
        .clone()
        .or_else(|| legacy_context_thread_id(&msg));
    state.last_thread_seq = msg.meta.thread_seq;
    state.last_src_ilk = Some(msg.meta.src_ilk.clone());
    state.last_ich = msg.meta.ich.clone();
    Ok(())
}

async fn process_router_message(
    sender: &NodeSender,
    msg: &Message,
    control_state: &mut CognitionControlState,
    runtime_state: Arc<Mutex<CognitionRuntimeState>>,
    nats_subscribe_errors: Arc<AtomicU64>,
    node_name: &str,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
) -> Result<(), CognitionError> {
    tracing::info!(
        node_name = %node_name,
        trace_id = %msg.routing.trace_id,
        src = %msg.routing.src,
        dst = ?msg.routing.dst,
        msg_type = %msg.meta.msg_type,
        msg = msg.meta.msg.as_deref().unwrap_or(""),
        action = msg.meta.action.as_deref().unwrap_or(""),
        target = msg.meta.target.as_deref().unwrap_or(""),
        "sy.cognition received router message"
    );
    if try_handle_default_node_status(sender, msg).await? {
        return Ok(());
    }
    if msg.meta.msg_type != SYSTEM_KIND {
        return Ok(());
    }
    let Some(command) = msg.meta.msg.as_deref() else {
        return Ok(());
    };
    match command {
        "CONFIG_GET" => {
            let snapshot = runtime_state.lock().await.clone();
            let payload = build_cognition_config_get_payload(
                node_name,
                control_state,
                &snapshot,
                runtime_paths,
                use_durable_consumer,
                nats_subscribe_errors.load(Ordering::Relaxed),
                None,
            );
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
        }
        "CONFIG_SET" => {
            let payload = apply_cognition_config_set(
                msg,
                node_name,
                control_state,
                runtime_paths,
                use_durable_consumer,
            )?;
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
        }
        "PING" => {
            let snapshot = runtime_state.lock().await.clone();
            sender
                .send(build_system_reply(
                    sender,
                    msg,
                    "PONG",
                    build_status_payload(
                        node_name,
                        control_state,
                        &snapshot,
                        runtime_paths,
                        use_durable_consumer,
                        nats_subscribe_errors.load(Ordering::Relaxed),
                    ),
                ))
                .await?;
        }
        "STATUS" => {
            let snapshot = runtime_state.lock().await.clone();
            sender
                .send(build_system_reply(
                    sender,
                    msg,
                    "STATUS_RESPONSE",
                    build_status_payload(
                        node_name,
                        control_state,
                        &snapshot,
                        runtime_paths,
                        use_durable_consumer,
                        nats_subscribe_errors.load(Ordering::Relaxed),
                    ),
                ))
                .await?;
        }
        _ => {}
    }
    Ok(())
}

fn build_system_reply(
    sender: &NodeSender,
    incoming: &Message,
    response_msg: &str,
    payload: Value,
) -> Message {
    Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(incoming.routing.src.clone()),
            ttl: incoming.routing.ttl.max(1),
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(response_msg.to_string()),
            src_ilk: incoming.meta.src_ilk.clone(),
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

fn build_status_payload(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_state: &CognitionRuntimeState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
    nats_subscribe_errors: u64,
) -> Value {
    let degraded_reasons = cognition_degraded_reasons(control_state);
    json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "degraded": {
            "active": !degraded_reasons.is_empty(),
            "reasons": degraded_reasons
        },
        "ai_provider": {
            "provider": "openai",
            "configured": control_state.ai_secret_source != CognitionAiSecretSource::Missing,
            "source": control_state.ai_secret_source.as_str()
        },
        "turns_consumer": {
            "subject": SUBJECT_STORAGE_TURNS,
            "mode": if use_durable_consumer { "durable" } else { "volatile" },
            "durable_queue": if use_durable_consumer {
                Value::String(DURABLE_QUEUE_TURNS.to_string())
            } else {
                Value::Null
            },
            "processed_turns_total": runtime_state.processed_turns_total,
            "invalid_turns_total": runtime_state.invalid_turns_total,
            "subscribe_failures": nats_subscribe_errors,
            "last_trace_id": runtime_state.last_trace_id,
            "last_thread_id": runtime_state.last_thread_id,
            "last_thread_seq": runtime_state.last_thread_seq,
            "last_src_ilk": runtime_state.last_src_ilk,
            "last_ich": runtime_state.last_ich
        },
        "paths": {
            "state_dir": runtime_paths.state_dir,
            "cache_dir": runtime_paths.cache_dir,
            "shm_dir": runtime_paths.shm_dir,
            "memory_lance": runtime_paths.memory_lance_path
        }
    })
}

fn build_cognition_config_get_payload(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_state: &CognitionRuntimeState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
    nats_subscribe_errors: u64,
    note: Option<&str>,
) -> Value {
    let configured = control_state.ai_secret_source != CognitionAiSecretSource::Missing;
    let mut secret_descriptor = NodeSecretDescriptor::new(
        "config.secrets.openai.api_key",
        COGNITION_LOCAL_SECRET_KEY_OPENAI,
    );
    secret_descriptor.required = false;
    secret_descriptor.configured = configured;
    secret_descriptor.persistence = control_state.ai_secret_source.as_str().to_string();

    let mut notes = vec![
        Value::String(
            "SY.cognition currently runs as a live skeleton: router control-plane plus storage.turns consumer."
                .to_string(),
        ),
        Value::String(
            "The OpenAI provider secret is optional at this stage; the node starts in degraded mode when missing."
                .to_string(),
        ),
        Value::String(
            "Derived cognition writers and SHM materialization remain pending; this config path is already canonical."
                .to_string(),
        ),
    ];
    if let Some(note) = note.filter(|value| !value.trim().is_empty()) {
        notes.push(Value::String(note.to_string()));
    }

    json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "config": {
            "nats": {
                "input_subject": SUBJECT_STORAGE_TURNS,
                "consumer_mode": if use_durable_consumer { "durable" } else { "volatile" },
                "durable_queue": if use_durable_consumer { Value::String(DURABLE_QUEUE_TURNS.to_string()) } else { Value::Null }
            },
            "storage": {
                "write_subject_prefix": "storage.cognition",
                "enabled": true
            },
            "ai_providers": {
                "openai": {
                    "provider": "openai",
                    "api_key": if configured { Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()) } else { Value::Null }
                }
            },
            "thresholds": {
                "context_open": control_state.thresholds.context_open,
                "reason_open": control_state.thresholds.reason_open
            },
            "paths": {
                "state_dir": runtime_paths.state_dir,
                "cache_dir": runtime_paths.cache_dir,
                "shm_dir": runtime_paths.shm_dir,
                "memory_lance": runtime_paths.memory_lance_path
            }
        },
        "runtime": {
            "processed_turns_total": runtime_state.processed_turns_total,
            "invalid_turns_total": runtime_state.invalid_turns_total,
            "subscribe_failures": nats_subscribe_errors,
            "last_thread_id": runtime_state.last_thread_id,
            "last_thread_seq": runtime_state.last_thread_seq
        },
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.cognition",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": [],
            "optional_fields": [
                "config.secrets.openai.api_key",
                "config.thresholds.context_open",
                "config.thresholds.reason_open"
            ],
            "secrets": [secret_descriptor],
            "notes": notes
        }
    })
}

fn apply_cognition_config_set(
    msg: &Message,
    node_name: &str,
    control_state: &mut CognitionControlState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
) -> Result<Value, CognitionError> {
    let Some(requested_node_name) = msg.payload.get("node_name").and_then(Value::as_str) else {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            use_durable_consumer,
            "invalid_config",
            "config-set requires node_name",
        ));
    };
    if requested_node_name != node_name {
        return Ok(config_error_response(
            node_name,
            control_state,
            runtime_paths,
            use_durable_consumer,
            "invalid_config",
            "config-set node_name does not match this node",
        ));
    }
    if let Some(apply_mode) = msg.payload.get("apply_mode").and_then(Value::as_str) {
        if !apply_mode
            .trim()
            .eq_ignore_ascii_case(NODE_CONFIG_APPLY_MODE_REPLACE)
        {
            return Ok(config_error_response(
                node_name,
                control_state,
                runtime_paths,
                use_durable_consumer,
                "unsupported_apply_mode",
                "SY.cognition currently supports only apply_mode=replace",
            ));
        }
    }

    let openai_api_key = extract_cognition_openai_api_key(&msg.payload);
    let stored_openai_secret = openai_api_key.is_some();
    let thresholds = match extract_cognition_thresholds(&msg.payload) {
        Ok(value) => value,
        Err(err) => {
            return Ok(config_error_response(
                node_name,
                control_state,
                runtime_paths,
                use_durable_consumer,
                "invalid_config",
                &err.to_string(),
            ));
        }
    };

    if let Some(api_key) = openai_api_key.as_deref() {
        persist_local_openai_api_key(
            node_name,
            api_key,
            &NodeSecretWriteOptions {
                updated_by_ilk: msg
                    .payload
                    .get("requested_by")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string),
                updated_by_label: Some("sy.cognition".to_string()),
                trace_id: Some(msg.routing.trace_id.clone()),
            },
        )?;
        control_state.ai_secret_source = CognitionAiSecretSource::LocalFile;
    }

    if let Some(thresholds) = thresholds {
        control_state.thresholds = thresholds;
    }
    control_state.config_version = control_state.config_version.saturating_add(1);
    persist_cognition_config_state(node_name, control_state)?;

    Ok(json!({
        "ok": true,
        "node_name": node_name,
        "state": cognition_state_label(control_state),
        "schema_version": control_state.schema_version,
        "config_version": control_state.config_version,
        "apply_mode": NODE_CONFIG_APPLY_MODE_REPLACE,
        "config": {
            "thresholds": {
                "context_open": control_state.thresholds.context_open,
                "reason_open": control_state.thresholds.reason_open
            }
        },
        "stored_secrets": if stored_openai_secret {
            json!([{
                "field": "config.secrets.openai.api_key",
                "storage_key": COGNITION_LOCAL_SECRET_KEY_OPENAI,
                "value_redacted": true
            }])
        } else {
            Value::Array(Vec::new())
        },
        "message": "SY.cognition local config persisted. Provider secret affects future AI calls once the cognitive pipeline is enabled."
    }))
}

fn config_error_response(
    node_name: &str,
    control_state: &CognitionControlState,
    runtime_paths: &RuntimePaths,
    use_durable_consumer: bool,
    code: &str,
    message: &str,
) -> Value {
    build_cognition_config_get_payload(
        node_name,
        control_state,
        &CognitionRuntimeState::default(),
        runtime_paths,
        use_durable_consumer,
        0,
        Some(message),
    )
    .as_object()
    .cloned()
    .map(|mut value| {
        value.insert("ok".to_string(), Value::Bool(false));
        value.insert(
            "error".to_string(),
            json!({
                "code": code,
                "message": message
            }),
        );
        Value::Object(value)
    })
    .unwrap_or_else(|| {
        json!({
            "ok": false,
            "node_name": node_name,
            "state": cognition_state_label(control_state),
            "error": {
                "code": code,
                "message": message
            }
        })
    })
}

fn cognition_state_label(control_state: &CognitionControlState) -> &'static str {
    match control_state.ai_secret_source {
        CognitionAiSecretSource::Missing => "degraded_no_ai_provider",
        CognitionAiSecretSource::LocalFile | CognitionAiSecretSource::EnvCompat => "ready_scaffold",
    }
}

fn cognition_degraded_reasons(control_state: &CognitionControlState) -> Vec<&'static str> {
    let mut reasons = vec!["writer_pipeline_pending", "memory_shm_pending"];
    if control_state.ai_secret_source == CognitionAiSecretSource::Missing {
        reasons.push("ai_provider_missing");
    }
    reasons
}

fn bootstrap_cognition_control_state(
    node_name: &str,
    ai_secret_source: CognitionAiSecretSource,
) -> Result<CognitionControlState, CognitionError> {
    let persisted = load_cognition_config_state(node_name);
    let schema_version = persisted
        .as_ref()
        .map(|value| value.schema_version)
        .unwrap_or(COGNITION_CONFIG_SCHEMA_VERSION);
    let config_version = persisted
        .as_ref()
        .map(|value| value.config_version)
        .unwrap_or(0);
    let thresholds = persisted
        .as_ref()
        .and_then(|value| {
            value
                .config
                .get("thresholds")
                .cloned()
                .and_then(|value| serde_json::from_value::<CognitionThresholds>(value).ok())
        })
        .unwrap_or_default();
    let state = CognitionControlState {
        schema_version,
        config_version,
        ai_secret_source,
        thresholds,
    };
    persist_cognition_config_state(node_name, &state)?;
    Ok(state)
}

fn load_cognition_config_state(node_name: &str) -> Option<CognitionConfigStateFile> {
    let path = managed_node_config_path(node_name).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<CognitionConfigStateFile>(&raw).ok()
}

fn persist_cognition_config_state(
    node_name: &str,
    state: &CognitionControlState,
) -> Result<(), CognitionError> {
    let path = managed_node_config_path(node_name)?;
    let payload = CognitionConfigStateFile {
        schema_version: state.schema_version,
        config_version: state.config_version,
        node_name: node_name.to_string(),
        config: json!({
            "thresholds": {
                "context_open": state.thresholds.context_open,
                "reason_open": state.thresholds.reason_open
            }
        }),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_json_atomic(&path, &serde_json::to_string_pretty(&payload)?)?;
    Ok(())
}

fn ensure_runtime_paths(node_name: &str) -> Result<RuntimePaths, CognitionError> {
    let state_dir = managed_node_instance_dir(node_name)?;
    let cache_dir = state_dir.join("cache");
    let shm_dir = state_dir.join("shm");
    fs::create_dir_all(&cache_dir)?;
    fs::create_dir_all(&shm_dir)?;
    Ok(RuntimePaths {
        memory_lance_path: state_dir.join("memory.lance"),
        state_dir,
        shm_dir,
        cache_dir,
    })
}

fn extract_cognition_openai_api_key(body: &Value) -> Option<String> {
    body.get("config")
        .and_then(|config| config.get("secrets"))
        .and_then(|value| value.get("openai"))
        .and_then(|value| value.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn extract_cognition_thresholds(
    body: &Value,
) -> Result<Option<CognitionThresholds>, CognitionError> {
    let Some(thresholds) = body.get("config").and_then(|value| value.get("thresholds")) else {
        return Ok(None);
    };
    let context_open = thresholds
        .get("context_open")
        .and_then(Value::as_f64)
        .unwrap_or(COGNITION_DEFAULT_CONTEXT_OPEN_THRESHOLD);
    let reason_open = thresholds
        .get("reason_open")
        .and_then(Value::as_f64)
        .unwrap_or(COGNITION_DEFAULT_REASON_OPEN_THRESHOLD);
    if !context_open.is_finite() || !(0.0..=1.0).contains(&context_open) {
        return Err(
            "config.thresholds.context_open must be a finite number between 0 and 1".into(),
        );
    }
    if !reason_open.is_finite() || !(0.0..=1.0).contains(&reason_open) {
        return Err("config.thresholds.reason_open must be a finite number between 0 and 1".into());
    }
    Ok(Some(CognitionThresholds {
        context_open,
        reason_open,
    }))
}

fn persist_local_openai_api_key(
    node_name: &str,
    api_key: &str,
    options: &NodeSecretWriteOptions,
) -> Result<(), CognitionError> {
    let mut secrets = load_node_secret_record(node_name)
        .map(|record| record.secrets)
        .unwrap_or_else(|_| Map::new());
    secrets.insert(
        COGNITION_LOCAL_SECRET_KEY_OPENAI.to_string(),
        Value::String(api_key.to_string()),
    );
    let record = build_node_secret_record(secrets, options);
    save_node_secret_record(node_name, &record)?;
    Ok(())
}

fn load_local_openai_api_key(node_name: &str) -> Option<String> {
    load_node_secret_record(node_name)
        .ok()
        .and_then(|record| {
            record
                .secrets
                .get(COGNITION_LOCAL_SECRET_KEY_OPENAI)
                .cloned()
        })
        .and_then(|value| value.as_str().map(ToString::to_string))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_openai_api_key_source(node_name: &str) -> CognitionAiSecretSource {
    if load_local_openai_api_key(node_name).is_some() {
        return CognitionAiSecretSource::LocalFile;
    }
    if std::env::var("OPENAI_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return CognitionAiSecretSource::EnvCompat;
    }
    CognitionAiSecretSource::Missing
}

fn legacy_context_thread_id(msg: &Message) -> Option<String> {
    msg.meta
        .context
        .as_ref()
        .and_then(|value| value.get("thread_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, CognitionError> {
    let path = config_dir.join("hive.yaml");
    let raw = fs::read_to_string(&path)?;
    Ok(serde_yaml::from_str(&raw)?)
}

async fn connect_with_retry(
    config: &NodeConfig,
    delay: Duration,
) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
    loop {
        match connect(config).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                tracing::warn!(error = %err, "sy.cognition router connect failed; retrying");
                time::sleep(delay).await;
            }
        }
    }
}

fn ensure_l2_name(name: &str, hive_id: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    if trimmed.contains('@') {
        trimmed.to_string()
    } else {
        format!("{trimmed}@{hive_id}")
    }
}

fn write_json_atomic(path: &Path, body: &str) -> Result<(), CognitionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        use std::io::Write;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir_file) = OpenOptions::new().read(true).open(parent) {
            let _ = dir_file.sync_all();
        }
    }
    Ok(())
}
